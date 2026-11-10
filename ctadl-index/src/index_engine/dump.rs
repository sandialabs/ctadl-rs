//! `CTADL_DUMP_RELATIONS=<dir>`: writes every relation of the index engine to `<dir>`, one
//! `<relation>.tsv` per relation, once the fixpoint is reached. `scripts/fact-dump-diff.sh` sorts
//! and compares these dumps to check that two builds or engines derive the same sets.
//!
//! A row is one line, its columns separated by tabs. Every column is printed by content, never by
//! an intern id: a function by its name, and a string, path or call target by its text. Rows come out in whatever order the engine holds them,
//! which differs between runs and engines; only the sorted file is comparable.
//!
//! Every relation in the program's `relation_sizes_summary` must be written, or the dump panics
//! naming the ones that were not, so a relation added to `index_rules` and not to
//! [`dump_relations!`](crate::dump_relations) is caught on its first dump.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::error::Error;
use crate::facts::{
    CallDispatchKey, FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, IdMap, InsnId,
    IntentKind, IntentPairKind, PackedInsnSiteId, Path, Str,
};
use crate::index_engine::path_set::PathSetRef;
use crate::index_engine::{IndexConfig, IndexResult, Rows, unpack_site};

/// The environment variable that turns the dump on and names its directory.
pub const DUMP_ENV: &str = "CTADL_DUMP_RELATIONS";

/// What a column needs to print itself by content.
pub struct Cx<'a> {
    id_map: Option<&'a IdMap>,
}

impl Cx<'_> {
    fn function(&self, f: FunctionId, out: &mut String) {
        match self.id_map.and_then(|m| m.get_function(f)) {
            Some(name) => out.push_str(name.0.as_ref()),
            // Only a run without an id map (a unit test) gets here.
            None => write!(out, "#{}", f.id).unwrap(),
        }
    }
}

/// One column of a dumped row.
pub trait DumpCol {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String);
}

impl<T: DumpCol + ?Sized> DumpCol for &T {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        (**self).dump_col(cx, out)
    }
}

macro_rules! dump_display {
    ($($t:ty),*) => {$(
        impl DumpCol for $t {
            fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
                write!(out, "{self}").unwrap()
            }
        }
    )*};
}
dump_display!(
    i64,
    FormalIndex,
    crate::facts::Root,
    crate::facts::GlobalId,
    FlowVariable,
    FlowVertex,
    Path,
    Str,
    CallDispatchKey
);

macro_rules! dump_debug {
    ($($t:ty),*) => {$(
        impl DumpCol for $t {
            fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
                write!(out, "{self:?}").unwrap()
            }
        }
    )*};
}
dump_debug!(FormalType, IntentKind, IntentPairKind);

impl DumpCol for FunctionId {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        cx.function(*self, out)
    }
}

impl DumpCol for InsnId {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        write!(out, "{}", self.id).unwrap()
    }
}

impl DumpCol for PackedInsnSiteId {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        let site = unpack_site(self);
        cx.function(site.func_id, out);
        write!(out, ":{}", site.insn_id.id).unwrap()
    }
}

impl<A: DumpCol, B: DumpCol> DumpCol for (A, B) {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        self.0.dump_col(cx, out);
        out.push(' ');
        self.1.dump_col(cx, out);
    }
}

/// The settings that change what is derived. The engine and storage are left out: they must not
/// change the result, and leaving them out lets two engines' dumps be compared.
impl DumpCol for IndexConfig {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        let IndexConfig {
            alias_rule,
            parallelism: _,
            storage: _,
            deref_paths,
        } = self;
        write!(out, "alias_rule={alias_rule} deref_paths={deref_paths}").unwrap()
    }
}

/// The set's size; its paths are the `paths` relation.
impl DumpCol for PathSetRef {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        write!(out, "{} paths", self.0.len()).unwrap()
    }
}

/// A whole row: a tuple of columns.
pub trait DumpRow {
    fn dump_row(&self, cx: &Cx<'_>, out: &mut String);
}

macro_rules! dump_row_tuple {
    ($first:ident $(, $rest:ident)*) => {
        impl<$first: DumpCol $(, $rest: DumpCol)*> DumpRow for ($first, $($rest,)*) {
            #[allow(non_snake_case)]
            fn dump_row(&self, cx: &Cx<'_>, out: &mut String) {
                let ($first, $($rest,)*) = self;
                push_col($first, cx, out);
                $(
                    out.push('\t');
                    push_col($rest, cx, out);
                )*
            }
        }
    };
}
dump_row_tuple!(A);
dump_row_tuple!(A, B);
dump_row_tuple!(A, B, C);
dump_row_tuple!(A, B, C, D);
dump_row_tuple!(A, B, C, D, E);
dump_row_tuple!(A, B, C, D, E, F);
dump_row_tuple!(A, B, C, D, E, F, G);
dump_row_tuple!(A, B, C, D, E, F, G, H);

/// Prints `col`, escaping what would break the line-per-row, tab-per-column format.
fn push_col(col: &impl DumpCol, cx: &Cx<'_>, out: &mut String) {
    let start = out.len();
    col.dump_col(cx, out);
    if out[start..].contains(['\\', '\t', '\n', '\r']) {
        let raw = out.split_off(start);
        for c in raw.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
    }
}

/// A table of an index directory: what an index run keeps, as opposed to the scratch relations
/// of its fixpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTable {
    Summary,
    Assign,
    Paths,
    ExternalFunction,
    GlobalFlow,
    ResolvedCall,
    IntentPair,
}

/// Where output tables are read from: a result in memory, or an index directory.
#[derive(Clone, Copy)]
pub enum Outputs<'a> {
    Result(&'a IndexResult),
    Dir(&'a std::path::Path),
}

impl OutputTable {
    pub const ALL: [OutputTable; 7] = [
        OutputTable::Summary,
        OutputTable::Assign,
        OutputTable::Paths,
        OutputTable::ExternalFunction,
        OutputTable::GlobalFlow,
        OutputTable::ResolvedCall,
        OutputTable::IntentPair,
    ];

    /// The table's file name in an index directory, without the extension.
    pub fn name(self) -> &'static str {
        match self {
            OutputTable::Summary => "summary",
            OutputTable::Assign => "assign",
            OutputTable::Paths => "paths",
            OutputTable::ExternalFunction => "external_function",
            OutputTable::GlobalFlow => "global_flow",
            OutputTable::ResolvedCall => "resolved_call",
            OutputTable::IntentPair => "intent_pair",
        }
    }

    /// Renders each row of this table in `from` as a dump line, without its newline, and passes
    /// it to `f`, in the order the table holds them.
    pub fn for_each_row(
        self,
        from: Outputs<'_>,
        id_map: Option<&IdMap>,
        mut f: impl FnMut(&str),
    ) -> Result<(), Error> {
        use crate::facts::schema;
        fn render<R: DumpRow>(rows: &[R], cx: &Cx<'_>, f: &mut impl FnMut(&str)) {
            let mut line = String::new();
            for row in rows {
                line.clear();
                row.dump_row(cx, &mut line);
                f(&line);
            }
        }
        let cx = Cx { id_map };
        // A table read from a directory is loaded whole, as `IndexResult::try_load` loads it.
        macro_rules! table {
            ($field:ident, $schema:ident) => {
                match from {
                    Outputs::Result(r) => render(&r.$field, &cx, &mut f),
                    Outputs::Dir(dir) => render(&schema::$schema::try_load(dir)?, &cx, &mut f),
                }
            };
        }
        match self {
            OutputTable::Summary => table!(summary, summary),
            OutputTable::Assign => table!(assign_like, assign),
            OutputTable::Paths => table!(paths, paths),
            OutputTable::ExternalFunction => table!(external_function, external_function),
            OutputTable::GlobalFlow => table!(global_flow, global_flow),
            OutputTable::ResolvedCall => table!(resolved_call, resolved_call),
            OutputTable::IntentPair => table!(intent_pair, intent_pair),
        }
        Ok(())
    }
}

/// The dump of one run, in progress.
pub struct RelationDump<'a> {
    dir: PathBuf,
    cx: Cx<'a>,
    written: Vec<String>,
}

/// The file of one relation, in progress.
pub struct RelationWriter<'c, 'a> {
    cx: &'c Cx<'a>,
    out: std::io::BufWriter<std::fs::File>,
    line: String,
    path: PathBuf,
}

impl RelationWriter<'_, '_> {
    pub fn row(&mut self, row: &impl DumpRow) {
        self.line.clear();
        row.dump_row(self.cx, &mut self.line);
        self.line.push('\n');
        self.out
            .write_all(self.line.as_bytes())
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: writing {}: {e}", self.path.display()));
    }
}

impl<'a> RelationDump<'a> {
    /// The dump `CTADL_DUMP_RELATIONS` asks for, if it is set. Creates the directory.
    pub fn from_env(id_map: Option<&'a IdMap>) -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os(DUMP_ENV)?);
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: creating {}: {e}", dir.display()));
        log::info!("dumping the index relations to {}", dir.display());
        Some(RelationDump {
            dir,
            cx: Cx { id_map },
            written: Vec::new(),
        })
    }

    /// Writes relation `name`, whose rows `fill` passes to the writer. Refuses to overwrite an
    /// earlier dump, so a second index run in one process cannot silently replace the first.
    pub fn relation(&mut self, name: &str, fill: impl FnOnce(&mut RelationWriter<'_, 'a>)) {
        let path = self.dir.join(format!("{name}.tsv"));
        let file = std::fs::File::create_new(&path)
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: creating {}: {e}", path.display()));
        let mut w = RelationWriter {
            cx: &self.cx,
            out: std::io::BufWriter::with_capacity(1 << 20, file),
            line: String::new(),
            path,
        };
        fill(&mut w);
        w.out
            .flush()
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: writing {}: {e}", w.path.display()));
        self.written.push(name.to_string());
    }

    /// Checks that every relation in `relation_sizes` (the program's `relation_sizes_summary`)
    /// was written.
    pub fn finish(self, relation_sizes: &str) {
        let missing: Vec<_> = super::profile::parse_relation_sizes(relation_sizes)
            .into_keys()
            .filter(|name| !self.written.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "{DUMP_ENV}: relations not dumped: {}; list them in `dump_relations!`",
            missing.join(", ")
        );
        log::info!(
            "dumped {} relations to {}",
            self.written.len(),
            self.dir.display()
        );
    }
}

/// How a relation's field is dumped. A field holding its rows writes them. A store-backed
/// relation's field holds no rows, so it writes nothing, and [`EngineStores::dump_stores`]
/// writes the relation from its store instead.
///
/// [`EngineStores::dump_stores`]: super::EngineStores::dump_stores
pub trait DumpField<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str);
}

pub fn dump_rows<Row: DumpRow>(rows: &dyn Rows<Row>, dump: &mut RelationDump<'_>, name: &str) {
    dump.relation(name, |w| {
        let mut it = rows.stream();
        while let Some(row) = it.next() {
            w.row(row);
        }
    });
}

impl<Row: DumpRow> DumpField<Row> for Vec<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str) {
        dump_rows::<Row>(self, dump, name)
    }
}

impl<Row: DumpRow> DumpField<Row> for ascent::boxcar::Vec<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str) {
        dump_rows::<Row>(self, dump, name)
    }
}

macro_rules! store_backed {
    ($($t:ty),*) => {$(
        impl<Row> DumpField<Row> for $t {
            fn dump_field(&self, _: &mut RelationDump<'_>, _: &str) {}
        }
    )*};
}
store_backed!(
    super::locals_trie::CountingVec<Row>,
    super::c_locals_trie::CCountingVec<Row>,
    lsm::ascent_provider::CountingVec<Row>,
    lsm::ascent_provider::SeedVec<Row>
);

/// Dumps every relation of `index_rules` held in its program field; see [`DumpField`].
#[macro_export]
macro_rules! dump_relations {
    ($dump:expr, $prog:expr) => {
        $crate::dump_relations!(@each $dump, $prog, [
            formal_param, actual_param, call, const_str_assign, intent_send, intent_filter,
            intent_component, paths, path_set, summary, config, intent_frame, locals,
            assign_like, prog_store, alias_of_formal, const_reaches, resolved_call, intent_pair,
            reach_vp, locals_key, locals_key_wild, locals_wild, assign_wild, edge_split,
            edge_split_wild, ext_dst, ext_fml, global_mention, alias_flow, global_read,
            global_label_key, global_label_read
        ])
    };
    (@each $dump:expr, $prog:expr, [$($rel:ident),* $(,)?]) => {
        $(
            $crate::index_engine::dump::DumpField::dump_field(
                &$prog.$rel,
                $dump,
                stringify!($rel),
            );
        )*
    };
}
