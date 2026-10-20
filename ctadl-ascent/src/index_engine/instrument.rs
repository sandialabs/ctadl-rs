//! Investigation-only instrumentation of the index fixpoint, kept out of `mod.rs` so this
//! branch rebases onto new main commits with few conflicts. `mod.rs` calls [`after_run!`] once,
//! after the run and before the result is extracted. It logs, at debug, the per-rule derivation
//! counts (`index rule counts:`) and every index's size (`[indexsizes]`) that the patched Ascent
//! macro generates, then dumps the relations.
//!
//! `CTADL_INDEX_DUMP=<dir>` writes the heavy relations as TSV, one file per relation, with a
//! header row. Functions appear as numeric ids; `functions.tsv` maps each id to its name.
//! Variables and paths are written in their `Display` form. Under `CTADL_INDEX_TIMEOUT_SECS`
//! the dump is the partial state at the moment the run stopped.

use std::io::{self, BufWriter, Write};
use std::path::{Path as FsPath, PathBuf};

use ascent::internal::{Freezable, RelIndexReadAll, ToRelIndex};
use streaming_iterator::StreamingIterator;

use super::{DecisionSet, Rows, assign_like_trie, c_assign_like_trie, c_locals_trie, locals_trie};
use crate::facts::{
    CallDispatchKey, CallTargetObject, FlowVariable, FlowVertex, FormalIndex, FunctionId, IdMap,
    InsnId, Path,
};

/// Logs `$prog`'s rule counts and index sizes, then dumps its heavy relations to
/// `$CTADL_INDEX_DUMP`, if it is set. A macro because each engine's program type is local to
/// `taint_index`; the fields are spelled the same in all of them.
macro_rules! after_run {
    ($prog:ident, $id_map:expr) => {
        log::debug!("index rule counts:\n{}", $prog.rule_counts_summary());
        log::debug!("[indexsizes] index storage sizes:\n{}", $prog.index_sizes_summary());
        $crate::index_engine::instrument::dump(
            $crate::index_engine::instrument::Relations {
                locals: &mut $prog.__locals_ind_common,
                assign_like: &mut $prog.__assign_like_ind_common,
                call_target_assign_like: &$prog.call_target_assign_like,
                call_target_assign: &$prog.call_target_assign,
                summary: &$prog.summary,
                call: &$prog.call,
                callee_info: &$prog.callee_info,
                resolved_call: &$prog.resolved_call,
                context_locals: &$prog.context_locals,
            },
            $id_map,
        )
    };
}
pub(crate) use after_run;

type LocalsRow = (FunctionId, FlowVariable, Path, FormalIndex, Path);
type AssignLikeRow = (FunctionId, FlowVariable, Path, FlowVariable, Path);

/// A store-backed relation's rows. `&mut` because a concurrent trie must be frozen to be read.
pub(crate) trait StoreRows<A, B, C, D, E> {
    fn for_each_row(&mut self, visit: &mut dyn FnMut(&A, &B, &C, &D, &E));
}

/// Reads a trie through its full index, the view the rules read it through.
macro_rules! trie_rows {
    ($store:ty, $to:ty, $col3:ty $(, $freeze:ident)?) => {
        impl StoreRows<FunctionId, FlowVariable, Path, $col3, Path> for $store {
            fn for_each_row(
                &mut self,
                visit: &mut dyn FnMut(&FunctionId, &FlowVariable, &Path, &$col3, &Path),
            ) {
                $(Freezable::$freeze(self);)?
                let to = <$to>::default();
                for ((a, b, c, d, e), _) in to.to_rel_index(self).iter_all() {
                    visit(a, b, c, d, e);
                }
            }
        }
    };
}

trie_rows!(
    locals_trie::LocalsIndCommon<FunctionId, FlowVariable, Path, FormalIndex, Path>,
    locals_trie::ToFull<FunctionId, FlowVariable, Path, FormalIndex, Path>,
    FormalIndex
);
trie_rows!(
    c_locals_trie::CLocalsIndCommon<FunctionId, FlowVariable, Path, FormalIndex, Path>,
    c_locals_trie::CToFull<FunctionId, FlowVariable, Path, FormalIndex, Path>,
    FormalIndex,
    freeze
);
trie_rows!(
    assign_like_trie::AssignTrie<FunctionId, FlowVariable, Path, FlowVariable, Path>,
    assign_like_trie::ToFull<FunctionId, FlowVariable, Path, FlowVariable, Path>,
    FlowVariable
);
trie_rows!(
    c_assign_like_trie::CAssignTrie<FunctionId, FlowVariable, Path, FlowVariable, Path>,
    c_assign_like_trie::CToFull<FunctionId, FlowVariable, Path, FlowVariable, Path>,
    FlowVariable,
    freeze
);

impl<const P: u32> StoreRows<FunctionId, FlowVariable, Path, FormalIndex, Path>
    for lsm::ascent_provider::LsmStore<LocalsRow, P>
{
    fn for_each_row(
        &mut self,
        visit: &mut dyn FnMut(&FunctionId, &FlowVariable, &Path, &FormalIndex, &Path),
    ) {
        for (a, b, c, d, e) in self.iter() {
            visit(a, b, c, d, e);
        }
    }
}

impl<const P: u32> StoreRows<FunctionId, FlowVariable, Path, FlowVariable, Path>
    for lsm::ascent_provider::LsmStore<AssignLikeRow, P>
{
    fn for_each_row(
        &mut self,
        visit: &mut dyn FnMut(&FunctionId, &FlowVariable, &Path, &FlowVariable, &Path),
    ) {
        for (a, b, c, d, e) in self.iter() {
            visit(a, b, c, d, e);
        }
    }
}

/// The relations [`dump`] writes, borrowed from whichever engine's program ran.
pub(crate) struct Relations<'a> {
    pub locals: &'a mut dyn StoreRows<FunctionId, FlowVariable, Path, FormalIndex, Path>,
    pub assign_like: &'a mut dyn StoreRows<FunctionId, FlowVariable, Path, FlowVariable, Path>,
    pub call_target_assign_like: &'a dyn Rows<(FunctionId, FlowVariable, Path, CallTargetObject)>,
    pub call_target_assign: &'a dyn Rows<(FunctionId, FlowVertex, CallTargetObject)>,
    pub summary: &'a dyn Rows<(FunctionId, FormalIndex, Path, FormalIndex, Path)>,
    pub call: &'a dyn Rows<(FunctionId, InsnId, FunctionId)>,
    pub callee_info: &'a dyn Rows<(FunctionId, InsnId, FlowVariable, Path, CallDispatchKey)>,
    pub resolved_call: &'a dyn Rows<(FunctionId, InsnId, FunctionId)>,
    pub context_locals:
        &'a dyn Rows<(FunctionId, FlowVariable, Path, FormalIndex, Path, DecisionSet)>,
}

/// Writes `rels` under `$CTADL_INDEX_DUMP`, if it is set. A failure is logged, not returned:
/// the dump is a diagnostic and must not fail the index.
pub(crate) fn dump(rels: Relations<'_>, id_map: Option<&IdMap>) {
    let Some(dir) = std::env::var_os("CTADL_INDEX_DUMP").map(PathBuf::from) else {
        return;
    };
    match write_all(&dir, rels, id_map) {
        Ok(()) => log::info!("dumped index relations to {}", dir.display()),
        Err(e) => log::warn!("dumping index relations to {}: {e}", dir.display()),
    }
}

fn write_all(dir: &FsPath, rels: Relations<'_>, id_map: Option<&IdMap>) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let open = |name: &str, header: &str| -> io::Result<BufWriter<std::fs::File>> {
        let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(dir.join(name))?);
        writeln!(w, "{header}")?;
        Ok(w)
    };

    if let Some(m) = id_map {
        let mut w = open("functions.tsv", "f\tname")?;
        for (i, n) in m.get_id_to_name_map() {
            writeln!(w, "{i}\t{n}")?;
        }
        w.flush()?;
    }

    // The store visitors cannot return an error, so the first one is kept and the rest skipped.
    let mut w = open("locals.tsv", "f\tv\tp\tformal\tfp")?;
    let mut res = Ok(());
    rels.locals.for_each_row(&mut |f, v, p, m, fp| {
        if res.is_ok() {
            res = writeln!(w, "{}\t{v}\t{p}\t{m}\t{fp}", f.id);
        }
    });
    res?;
    w.flush()?;

    let mut w = open("assign_like.tsv", "f\tvd\tpd\tvs\tps")?;
    let mut res = Ok(());
    rels.assign_like.for_each_row(&mut |f, vd, pd, vs, ps| {
        if res.is_ok() {
            res = writeln!(w, "{}\t{vd}\t{pd}\t{vs}\t{ps}", f.id);
        }
    });
    res?;
    w.flush()?;

    table(
        open("call_target_assign_like.tsv", "f\tv\tp\ttarget")?,
        rels.call_target_assign_like,
        |w, (f, v, p, t)| writeln!(w, "{}\t{v}\t{p}\t{t}", f.id),
    )?;
    table(
        open("call_target_assign.tsv", "f\tv\tp\ttarget")?,
        rels.call_target_assign,
        |w, (f, FlowVertex(v, p), t)| writeln!(w, "{}\t{v}\t{p}\t{t}", f.id),
    )?;
    table(
        open("summary.tsv", "f\tn1\tp1\tn2\tp2")?,
        rels.summary,
        |w, (f, n1, p1, n2, p2)| writeln!(w, "{}\t{n1}\t{p1}\t{n2}\t{p2}", f.id),
    )?;
    table(open("call.tsv", "f\tinsn\ttgt")?, rels.call, |w, (f, i, t)| {
        writeln!(w, "{}\t{}\t{}", f.id, i.id, t.id)
    })?;
    table(
        open("callee_info.tsv", "f\tinsn\tv\tp\tkey")?,
        rels.callee_info,
        |w, (f, i, v, p, k)| writeln!(w, "{}\t{}\t{v}\t{p}\t{k}", f.id, i.id),
    )?;
    table(
        open("resolved_call.tsv", "f\tinsn\ttgt")?,
        rels.resolved_call,
        |w, (f, i, t)| writeln!(w, "{}\t{}\t{}", f.id, i.id, t.id),
    )?;
    // `nset`: how many decisions the row's context holds, -1 for top.
    table(
        open("context_locals.tsv", "f\tv\tp\tformal\tfp\tnset")?,
        rels.context_locals,
        |w, (f, v, p, m, fp, ds)| {
            let n = if ds.is_top() { -1 } else { ds.ids().count() as i64 };
            writeln!(w, "{}\t{v}\t{p}\t{m}\t{fp}\t{n}", f.id)
        },
    )?;
    Ok(())
}

fn table<Row>(
    mut w: BufWriter<std::fs::File>,
    rows: &dyn Rows<Row>,
    mut line: impl FnMut(&mut BufWriter<std::fs::File>, &Row) -> io::Result<()>,
) -> io::Result<()> {
    let mut it = rows.stream();
    while let Some(row) = it.next() {
        line(&mut w, row)?;
    }
    w.flush()
}
