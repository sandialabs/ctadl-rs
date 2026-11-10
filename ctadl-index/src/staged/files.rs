/*!
The stage directory's fact files, the files each stage writes, and the assembly of the index
tables from them.

The facts are written once, before any batch runs, one file per relation in `facts/`. The
per-function facts are sorted by batch, then by function, then by content, and a row group ends
at each batch boundary, so a batch reads its own rows by a [`FunctionFilter`] on its functions
and decodes little else. The program-wide facts (`external_function`, the intent facts and the
input `global_flow`) are sorted by content. A fixed order makes the same facts the same bytes
whatever order they came in, which the [`RunKey`](super::store::RunKey) relies on.

A batch writes, under `batches/<id>/`, its functions' `summary` and `assign` rows and its
frame edges, each sorted by function so that a caller's batch can read one callee's rows by
row-group statistics. The global frame ([`run_frame`]) writes the distinct frame edges under
`frame/`, and the intent stage writes its `intent_pair` and `resolved_call` under `intent/`.
[`assemble`] then streams all of these into the index tables.

Every file is in the encoding of the index's parquet tables: paths and strings by content.
*/

use std::cmp::Ordering;
use std::collections::BTreeMap;

use hashbrown::HashSet;
use std::path::{Path as FsPath, PathBuf};

use crate::error::Error;
use crate::facts::parquet::{
    DecodeColumns, DefaultDecoder, DefaultEncoder, EncodeColumns, FunctionFilter, Reader, Writer,
};
use crate::facts::schema;
use crate::facts::{
    FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, GLOBAL_FRAME, GlobalId, InsnId,
    IntentKind, PackedInsnSiteId, Path, Root, Str,
};
use crate::index_engine::path_set::PathSetRef;
use crate::index_engine::{
    AssignLikeRow, FunctionSummary, GlobalFlow, IndexFacts, IntentInput, IntentOutput, unpack_site,
};
use crate::staged::plan::{Batch, BatchId, Plan};
use crate::staged::store::{STAGE_ROWS_PER_GROUP, Stage, StageDir, write_atomic};

/// The function column of every per-function file.
const FUNC_COLUMN: &str = "func_id";

/// An order by what a value denotes, never by an intern id, so that it is the same in every
/// process.
pub trait ContentOrd {
    fn content_cmp(&self, other: &Self) -> Ordering;
}

macro_rules! content_ord_by_ord {
    ($($t:ty),*) => {$(
        impl ContentOrd for $t {
            fn content_cmp(&self, other: &Self) -> Ordering {
                self.cmp(other)
            }
        }
    )*};
}
content_ord_by_ord!(FunctionId, InsnId, FormalIndex, IntentKind);

macro_rules! content_ord_inherent {
    ($($t:ty),*) => {$(
        impl ContentOrd for $t {
            fn content_cmp(&self, other: &Self) -> Ordering {
                <$t>::content_cmp(self, other)
            }
        }
    )*};
}
content_ord_inherent!(Str, Path, FlowVariable, Root, GlobalId);

impl ContentOrd for FormalType {
    fn content_cmp(&self, other: &Self) -> Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

macro_rules! content_ord_tuple {
    ($($name:ident $i:tt),+) => {
        impl<$($name: ContentOrd),+> ContentOrd for ($($name,)+) {
            fn content_cmp(&self, other: &Self) -> Ordering {
                Ordering::Equal $(.then_with(|| self.$i.content_cmp(&other.$i)))+
            }
        }
    };
}
content_ord_tuple!(A 0);
content_ord_tuple!(A 0, B 1);
content_ord_tuple!(A 0, B 1, C 2);
content_ord_tuple!(A 0, B 1, C 2, D 3);
content_ord_tuple!(A 0, B 1, C 2, D 3, E 4);

/// The batch of each function of a plan.
pub struct BatchIndex {
    /// By [`FunctionId::id`]; [`BatchIndex::NONE`] for a function in no batch.
    batch_of: Vec<BatchId>,
}

impl BatchIndex {
    const NONE: BatchId = BatchId::MAX;

    pub fn new(plan: &Plan) -> Self {
        let len = plan
            .batches
            .iter()
            .flat_map(|b| b.functions.last())
            .map(|f| f.id as usize + 1)
            .max()
            .unwrap_or(0);
        let mut batch_of = vec![Self::NONE; len];
        for batch in &plan.batches {
            for f in &batch.functions {
                batch_of[f.id as usize] = batch.id;
            }
        }
        Self { batch_of }
    }

    /// The batch that runs `f`, if any does.
    pub fn get(&self, f: FunctionId) -> Option<BatchId> {
        self.batch_of
            .get(f.id as usize)
            .copied()
            .filter(|b| *b != Self::NONE)
    }
}

fn site(f: FunctionId, insn: InsnId) -> PackedInsnSiteId {
    PackedInsnSiteId::try_from_parts(f, insn).expect("a site read back packs")
}

/// The site given to the rows whose files leave out the instruction, `assign` and
/// `const_str_assign`: the index rules read only their function.
fn function_site(f: FunctionId) -> PackedInsnSiteId {
    site(f, InsnId::new(0))
}

/// Sorts `rows` by batch, then by content (its record's first column is its function), and
/// writes them to `file` with a row group ending at each batch boundary.
fn write_partitioned<X, R>(
    file: PathBuf,
    columns: &[&str],
    rows: &mut [X],
    batches: &BatchIndex,
    record: impl Fn(&X) -> R,
    func: impl Fn(&R) -> FunctionId,
) -> Result<(), Error>
where
    R: ContentOrd,
    DefaultEncoder: EncodeColumns<R>,
{
    let batch = |r: &R| {
        let f = func(r);
        batches.get(f).ok_or_else(|| {
            Error::FactsConvert(format!(
                "{}: function {} is in no batch",
                file.display(),
                f.id
            ))
        })
    };
    for row in rows.iter() {
        batch(&record(row))?;
    }
    rows.sort_unstable_by(|a, b| {
        let (a, b) = (record(a), record(b));
        batch(&a)
            .unwrap()
            .cmp(&batch(&b).unwrap())
            .then_with(|| a.content_cmp(&b))
    });
    write_atomic(&file, |tmp| {
        Writer::new(tmp)
            .rows_per_group(STAGE_ROWS_PER_GROUP)
            .write_iter_grouped_by(columns, rows.iter().map(&record), |r| batch(r).unwrap())
    })
}

/// Sorts `rows` by content and writes them to `file`.
fn write_sorted<R>(file: PathBuf, columns: &[&str], rows: &mut [R]) -> Result<(), Error>
where
    R: ContentOrd + Copy,
    DefaultEncoder: EncodeColumns<R>,
{
    rows.sort_unstable_by(|a, b| a.content_cmp(b));
    write_atomic(&file, |tmp| {
        Writer::new(tmp)
            .rows_per_group(STAGE_ROWS_PER_GROUP)
            .write_iter(columns, rows.iter().copied())
    })
}

/// Writes the facts any stage reads to `stage`'s `facts/`, partitioned by `plan`'s batches. The
/// fact vectors are sorted in place, not copied. `facts.paths` is not written: the path set is
/// its own file. Nor are the facts no stage reads: `globals`, `callee_info` and
/// `android_call_site`.
///
/// Fails if a per-function fact belongs to a function in no batch.
pub fn write_facts(stage: &StageDir, facts: &mut IndexFacts, plan: &Plan) -> Result<(), Error> {
    let dir = stage.facts_dir();
    let batches = BatchIndex::new(plan);
    let IndexFacts {
        formal_param,
        actual_param,
        call,
        assign,
        const_str_assign,
        android_call_site: _,
        callee_info: _,
        summary,
        paths: _,
        intent_frame,
        intent_send,
        intent_filter,
        intent_component,
        external_function,
        globals: _,
        global_flow,
    } = facts;
    use schema::*;
    write_partitioned(
        dir.join(formal_param::FILENAME),
        &formal_param::COLUMNS,
        formal_param,
        &batches,
        |(f, v, ty)| {
            let i = v.as_formal().expect("a formal_param variable is a formal");
            (*f, i, *ty)
        },
        |r| r.0,
    )?;
    write_partitioned(
        dir.join(actual_param::FILENAME),
        &actual_param::COLUMNS,
        actual_param,
        &batches,
        |(s, i, FlowVertex(v, p, ..))| {
            let s = unpack_site(s);
            (s.func_id, s.insn_id, *i, *v, *p)
        },
        |r| r.0,
    )?;
    write_partitioned(
        dir.join(call::FILENAME),
        &call::COLUMNS,
        call,
        &batches,
        |(s, target)| {
            let s = unpack_site(s);
            (s.func_id, s.insn_id, *target)
        },
        |r| r.0,
    )?;
    write_partitioned(
        dir.join(assign::FILENAME),
        &assign::COLUMNS,
        assign,
        &batches,
        |(s, dst, src)| (unpack_site(s).func_id, dst.0, dst.1, src.0, src.1),
        |r| r.0,
    )?;
    write_partitioned(
        dir.join(const_str_assign::FILENAME),
        &const_str_assign::COLUMNS,
        const_str_assign,
        &batches,
        |(s, vx, value)| (unpack_site(s).func_id, vx.0, vx.1, *value),
        |r| r.0,
    )?;
    write_partitioned(
        dir.join(fact_summary::FILENAME),
        &fact_summary::COLUMNS,
        summary,
        &batches,
        |r| *r,
        |r| r.0,
    )?;
    write_sorted(
        dir.join(external_function::FILENAME),
        &external_function::COLUMNS,
        external_function,
    )?;
    write_sorted(
        dir.join(intent_frame::FILENAME),
        &intent_frame::COLUMNS,
        intent_frame,
    )?;
    write_sorted(
        dir.join(intent_send::FILENAME),
        &intent_send::COLUMNS,
        intent_send,
    )?;
    write_sorted(
        dir.join(intent_filter::FILENAME),
        &intent_filter::COLUMNS,
        intent_filter,
    )?;
    write_sorted(
        dir.join(intent_component::FILENAME),
        &intent_component::COLUMNS,
        intent_component,
    )?;
    write_sorted(
        dir.join(global_flow::FILENAME),
        &global_flow::COLUMNS,
        global_flow,
    )?;
    Ok(())
}

/// A batch's input, read from the stage directory.
pub struct BatchFacts {
    /// The batch's functions' facts, with `summary` also holding its callees' summaries from the
    /// batches that ran them. The program-wide facts are empty.
    pub facts: IndexFacts,
    /// Every file read, in the order read.
    pub read: Vec<PathBuf>,
}

/// The rows of `file` whose function is one of `functions`, which are sorted.
fn read_of<R>(
    file: PathBuf,
    columns: &[&str],
    functions: &[FunctionId],
    read: &mut Vec<PathBuf>,
) -> Result<Vec<R>, Error>
where
    DefaultDecoder: DecodeColumns<R>,
{
    let rows = Reader::new(&file)
        .rows_of(columns, FunctionFilter::new(FUNC_COLUMN, functions))?
        .collect::<Result<Vec<R>, Error>>()?;
    read.push(file);
    Ok(rows)
}

/// Reads `batch`'s input: its functions' facts from `facts/`, and its callees' summaries from
/// the `summary` files of the batches that hold them, which must be done.
pub fn read_batch_facts(stage: &StageDir, batch: &Batch) -> Result<BatchFacts, Error> {
    use schema::*;
    let dir = stage.facts_dir();
    let functions = &batch.functions;
    let mut read = Vec::new();
    let formal_param: Vec<formal_param::Record> = read_of(
        dir.join(formal_param::FILENAME),
        &formal_param::COLUMNS,
        functions,
        &mut read,
    )?;
    let actual_param: Vec<actual_param::Record> = read_of(
        dir.join(actual_param::FILENAME),
        &actual_param::COLUMNS,
        functions,
        &mut read,
    )?;
    let call: Vec<call::Record> = read_of(
        dir.join(call::FILENAME),
        &call::COLUMNS,
        functions,
        &mut read,
    )?;
    let assign: Vec<assign::Record> = read_of(
        dir.join(assign::FILENAME),
        &assign::COLUMNS,
        functions,
        &mut read,
    )?;
    let const_str_assign: Vec<const_str_assign::Record> = read_of(
        dir.join(const_str_assign::FILENAME),
        &const_str_assign::COLUMNS,
        functions,
        &mut read,
    )?;
    let mut summary: Vec<FunctionSummary> = read_of(
        dir.join(fact_summary::FILENAME),
        &fact_summary::COLUMNS,
        functions,
        &mut read,
    )?;

    let mut by_batch: BTreeMap<BatchId, Vec<FunctionId>> = BTreeMap::new();
    for (f, b) in &batch.callees {
        by_batch.entry(*b).or_default().push(*f);
    }
    for (b, callees) in by_batch {
        summary.extend(read_of::<FunctionSummary>(
            batch_file(stage, b, summary::FILENAME),
            &summary::COLUMNS,
            &callees,
            &mut read,
        )?);
    }

    let facts = IndexFacts {
        formal_param: formal_param
            .into_iter()
            .map(|(f, i, ty)| (f, FlowVariable::formal_index(i), ty))
            .collect(),
        actual_param: actual_param
            .into_iter()
            .map(|(f, insn, i, v, p)| (site(f, insn), i, FlowVertex::new(v, p)))
            .collect(),
        call: call
            .into_iter()
            .map(|(f, insn, target)| (site(f, insn), target))
            .collect(),
        assign: assign
            .into_iter()
            .map(|(f, v1, p1, v2, p2)| {
                (
                    function_site(f),
                    FlowVertex::new(v1, p1),
                    FlowVertex::new(v2, p2),
                )
            })
            .collect(),
        const_str_assign: const_str_assign
            .into_iter()
            .map(|(f, v, p, s)| (function_site(f), FlowVertex::new(v, p), s))
            .collect(),
        summary,
        ..Default::default()
    };
    Ok(BatchFacts { facts, read })
}

/// `name`, a file of batch `id`'s outputs.
pub fn batch_file(stage: &StageDir, id: BatchId, name: &str) -> PathBuf {
    stage.batch_dir(id).join(name)
}

/// What a batch derived for its own functions.
#[derive(Debug, Default)]
pub struct BatchOutputs {
    pub summary: Vec<FunctionSummary>,
    /// Destination first, as [`crate::index_engine::IndexResult::assign_like`] holds them.
    pub assign: Vec<AssignLikeRow>,
    /// Its edges of the global frame.
    pub frame: Vec<GlobalFlow>,
}

impl BatchOutputs {
    /// `summary` and `assign_like` rows of a run on `batch`'s input, less those of other
    /// functions: the callee summaries it was seeded with. The global frame's `assign_like`
    /// rows are its frame edges.
    pub fn new(
        batch: &Batch,
        summary: Vec<FunctionSummary>,
        assign_like: Vec<AssignLikeRow>,
    ) -> Self {
        let ours = |f: &FunctionId| batch.functions.binary_search(f).is_ok();
        let mut frame = Vec::new();
        let mut assign = Vec::with_capacity(assign_like.len());
        for row in assign_like {
            let (f, v1, p1, v2, p2) = row;
            if f == GLOBAL_FRAME {
                if let (Some(g1), Some(g2)) = (v1.as_global(), v2.as_global()) {
                    frame.push((g1, p1, g2, p2));
                }
            } else {
                debug_assert!(ours(&f), "a batch derives flow only in its functions");
                assign.push(row);
            }
        }
        Self {
            summary: summary.into_iter().filter(|r| ours(&r.0)).collect(),
            assign,
            frame,
        }
    }

    /// Writes the outputs to batch `id`'s directory, each file sorted by function. Each is
    /// written all at once; the caller marks the batch done after.
    pub fn write(mut self, stage: &StageDir, id: BatchId) -> Result<(), Error> {
        use schema::*;
        self.summary.sort_unstable_by_key(|r| r.0);
        self.assign.sort_unstable_by_key(|r| r.0);
        write_file(
            &batch_file(stage, id, summary::FILENAME),
            &summary::COLUMNS,
            self.summary,
        )?;
        write_file(
            &batch_file(stage, id, assign::FILENAME),
            &assign::COLUMNS,
            self.assign,
        )?;
        write_file(
            &batch_file(stage, id, frame_edge::FILENAME),
            &frame_edge::COLUMNS,
            self.frame,
        )
    }
}

fn write_file<R>(
    file: &FsPath,
    columns: &[&str],
    rows: impl IntoIterator<Item = R>,
) -> Result<(), Error>
where
    DefaultEncoder: EncodeColumns<R>,
{
    write_atomic(file, |tmp| {
        Writer::new(tmp)
            .rows_per_group(STAGE_ROWS_PER_GROUP)
            .write_iter(columns, rows)
    })
}

/// The rows of `files`, one after another, decoded a row group at a time.
fn concat<R: 'static>(
    files: Vec<PathBuf>,
    columns: &'static [&'static str],
) -> impl Iterator<Item = Result<R, Error>>
where
    DefaultDecoder: DecodeColumns<R>,
{
    files.into_iter().flat_map(move |file| {
        let rows: Box<dyn Iterator<Item = Result<R, Error>>> =
            match Reader::new(&file).rows::<R>(columns) {
                Ok(rows) => Box::new(rows),
                Err(e) => Box::new(std::iter::once(Err(e))),
            };
        rows
    })
}

/// Writes `index_dir`'s table `name` from `files`, streamed.
fn assemble_table<R: 'static>(
    index_dir: &FsPath,
    name: &str,
    columns: &'static [&'static str],
    files: Vec<PathBuf>,
) -> Result<(), Error>
where
    DefaultDecoder: DecodeColumns<R>,
    DefaultEncoder: EncodeColumns<R>,
{
    Writer::new(index_dir.join(name)).write_rows(columns, concat::<R>(files, columns))
}

/// The file of the global frame's edges.
pub fn frame_file(stage: &StageDir) -> PathBuf {
    stage.frame_dir().join(schema::frame_edge::FILENAME)
}

/// An edge of the global frame as a row of the frame's `assign`.
fn frame_assign((dst, p1, src, p2): GlobalFlow) -> AssignLikeRow {
    (
        GLOBAL_FRAME,
        FlowVariable::global(dst),
        p1,
        FlowVariable::global(src),
        p2,
    )
}

/// Stage 2, the global frame: the distinct edges of every batch of `plan`, all done, and of the
/// facts' `global_flow`. Writes them to the frame file and marks the stage done. Returns how
/// many there are.
pub fn run_frame(stage: &StageDir, plan: &Plan) -> Result<usize, Error> {
    use schema::*;
    let mut edges: HashSet<GlobalFlow> = HashSet::new();
    let files = plan
        .batches
        .iter()
        .map(|b| batch_file(stage, b.id, frame_edge::FILENAME))
        .chain([stage.facts_dir().join(global_flow::FILENAME)]);
    for file in files {
        Reader::new(file).for_each_batch(&frame_edge::COLUMNS, |rows: Vec<GlobalFlow>| {
            edges.extend(rows);
            Ok(())
        })?;
    }
    let n = edges.len();
    write_file(&frame_file(stage), &frame_edge::COLUMNS, edges)?;
    stage.mark_done(Stage::Frame)?;
    Ok(n)
}

/// The input of stage 3, the intent rules, if any function sends an intent: the `assign` rows
/// of the functions in `intent_frame`, from the batches that ran them, and the frame's edges,
/// from stage 2, which must be done; those functions' `const_str_assign` and `intent_send`;
/// and the other intent facts.
pub fn read_intent_input(
    stage: &StageDir,
    plan: &Plan,
    path_set: PathSetRef,
) -> Result<Option<IntentInput>, Error> {
    use schema::*;
    let facts = stage.facts_dir();
    let intent_frame: Vec<intent_frame::Record> =
        Reader::new(facts.join(intent_frame::FILENAME)).read_vec(&intent_frame::COLUMNS)?;
    let mut functions: Vec<FunctionId> = intent_frame
        .iter()
        .map(|(f,)| *f)
        .filter(|f| *f != GLOBAL_FRAME)
        .collect();
    functions.sort_unstable();
    functions.dedup();
    if functions.is_empty() {
        return Ok(None);
    }
    let mut read = Vec::new();
    let mut assign_like: Vec<AssignLikeRow> = Vec::new();
    for b in &plan.intent_batches {
        assign_like.extend(read_of::<assign::Record>(
            batch_file(stage, *b, assign::FILENAME),
            &assign::COLUMNS,
            &functions,
            &mut read,
        )?);
    }
    Reader::new(frame_file(stage)).for_each_batch(&frame_edge::COLUMNS, |rows| {
        assign_like.extend(rows.into_iter().map(frame_assign));
        Ok(())
    })?;
    Ok(Some(IntentInput {
        assign_like,
        const_str_assign: read_of(
            facts.join(const_str_assign::FILENAME),
            &const_str_assign::COLUMNS,
            &functions,
            &mut read,
        )?,
        intent_send: read_of(
            facts.join(intent_send::FILENAME),
            &intent_send::COLUMNS,
            &functions,
            &mut read,
        )?,
        intent_frame,
        intent_filter: Reader::new(facts.join(intent_filter::FILENAME))
            .read_vec(&intent_filter::COLUMNS)?,
        intent_component: Reader::new(facts.join(intent_component::FILENAME))
            .read_vec(&intent_component::COLUMNS)?,
        path_set,
    }))
}

/// Writes stage 3's outputs, empty when it did not run, and marks the stage done.
pub fn write_intent_output(stage: &StageDir, output: IntentOutput) -> Result<(), Error> {
    use schema::*;
    let dir = stage.intent_dir();
    write_file(
        &dir.join(intent_pair::FILENAME),
        &intent_pair::COLUMNS,
        output.intent_pair,
    )?;
    write_file(
        &dir.join(resolved_call::FILENAME),
        &resolved_call::COLUMNS,
        output.resolved_call,
    )?;
    stage.mark_done(Stage::Intent)
}

/// Writes the index tables to `index_dir` from the stage files of a run of `plan` whose stages
/// are all done, streaming each table a row group at a time:
/// - `summary` is the batches' rows, and `assign` those and the frame's edges;
/// - `paths` is the path set;
/// - `external_function` is the facts';
/// - `global_flow` is the frame's edges;
/// - `resolved_call` and `intent_pair` are stage 3's.
pub fn assemble(stage: &StageDir, plan: &Plan, index_dir: &FsPath) -> Result<(), Error> {
    use schema::*;
    let of_batches = |name: &str| -> Vec<PathBuf> {
        plan.batches
            .iter()
            .map(|b| batch_file(stage, b.id, name))
            .collect()
    };
    assemble_table::<summary::Record>(
        index_dir,
        summary::FILENAME,
        &summary::COLUMNS,
        of_batches(summary::FILENAME),
    )?;
    Writer::new(index_dir.join(assign::FILENAME)).write_rows(
        &assign::COLUMNS,
        concat::<assign::Record>(of_batches(assign::FILENAME), &assign::COLUMNS).chain(
            concat::<frame_edge::Record>(vec![frame_file(stage)], &frame_edge::COLUMNS)
                .map(|r| r.map(frame_assign)),
        ),
    )?;
    assemble_table::<paths::Record>(
        index_dir,
        paths::FILENAME,
        &paths::COLUMNS,
        vec![stage.paths_file()],
    )?;
    assemble_table::<external_function::Record>(
        index_dir,
        external_function::FILENAME,
        &external_function::COLUMNS,
        vec![stage.facts_dir().join(external_function::FILENAME)],
    )?;
    assemble_table::<global_flow::Record>(
        index_dir,
        global_flow::FILENAME,
        &global_flow::COLUMNS,
        vec![frame_file(stage)],
    )?;
    let intent = stage.intent_dir();
    assemble_table::<resolved_call::Record>(
        index_dir,
        resolved_call::FILENAME,
        &resolved_call::COLUMNS,
        vec![intent.join(resolved_call::FILENAME)],
    )?;
    assemble_table::<intent_pair::Record>(
        index_dir,
        intent_pair::FILENAME,
        &intent_pair::COLUMNS,
        vec![intent.join(intent_pair::FILENAME)],
    )
}
