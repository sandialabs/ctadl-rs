//! Canonical instructions: an instruction named by its function and its input facts.
//!
//! An instruction's signature is the set of rows of `call`, `actual_param`,
//! `call_target_assign` and `callee_info` that name it, each with the instruction column
//! removed, function ids replaced by names, and a call argument of the instruction itself
//! replaced by [`SigVar::This`]. These tables are written before the fixpoint, so a change in
//! what the index derives never renames an instruction.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::hash::BuildHasher;
use std::path::Path as FsPath;

use hashbrown::HashTable;
use rustc_hash::FxBuildHasher;

use crate::facts::schema::{Table, actual_param, call, call_target_assign, callee_info};
use crate::facts::{
    CallArgId, CallDispatchKey, FlowVariable, FlowVariableKind, FormalIndex, GlobalId, InsnId,
    Path, Str,
};
use crate::index_engine::dump::{Cx, DumpCol};

use super::canon::{CanonTarget, FnName};
use super::names::FunctionNames;
use super::table::load_raw;
use super::{Error, Side};

/// A signature's index in its [`SignatureInterner`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SignatureId(u32);

/// An instruction, named by content: its function and its signature. Instructions of one
/// function with equal signatures have the same name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CanonInsn {
    pub function: FnName,
    pub signature: SignatureId,
}

/// A variable in a signature row. A call argument can only be of the instruction the signature
/// is of.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SigVar {
    Uninit,
    Local(Str),
    Formal(FormalIndex),
    /// The argument at this index of the instruction itself.
    This(FormalIndex),
    Global(GlobalId),
}

/// One input fact about an instruction, less the instruction and its function.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SigRow {
    /// A row of `call`: the callee.
    Call(FnName),
    /// A row of `actual_param`: the formal and the vertex passed to it.
    ActualParam(FormalIndex, SigVar, Path),
    /// A row of `call_target_assign`: the vertex and the target written to it.
    CallTargetAssign(SigVar, Path, CanonTarget),
    /// A row of `callee_info`: the receiver vertex and the dispatch key.
    CalleeInfo(SigVar, Path, CallDispatchKey),
}

impl SigVar {
    /// `v` in a row about instruction `insn`. A call argument of another instruction is an error.
    pub(crate) fn new(v: FlowVariable, insn: InsnId) -> Result<SigVar, String> {
        Ok(match v.kind() {
            FlowVariableKind::Uninit => SigVar::Uninit,
            FlowVariableKind::Local(s) => SigVar::Local(s),
            FlowVariableKind::Formal(i) => SigVar::Formal(i),
            FlowVariableKind::Global(g) => SigVar::Global(g),
            FlowVariableKind::CallArg(packed) => {
                let CallArgId { insn_id, formal } = packed
                    .try_into()
                    .map_err(|e| format!("undecodable call argument {packed:?}: {e}"))?;
                if insn_id != insn {
                    return Err(format!(
                        "a call argument of instruction {} in a row about instruction {}",
                        insn_id.id, insn.id
                    ));
                }
                SigVar::This(FormalIndex::new(formal))
            }
        })
    }

    fn rank(&self) -> u8 {
        match self {
            SigVar::Uninit => 0,
            SigVar::Local(_) => 1,
            SigVar::Formal(_) => 2,
            SigVar::This(_) => 3,
            SigVar::Global(_) => 4,
        }
    }

    fn content_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SigVar::Local(a), SigVar::Local(b)) => a.content_cmp(b),
            (SigVar::Formal(a), SigVar::Formal(b)) | (SigVar::This(a), SigVar::This(b)) => a.cmp(b),
            (SigVar::Global(a), SigVar::Global(b)) => a.content_cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl SigRow {
    fn rank(&self) -> u8 {
        match self {
            SigRow::Call(_) => 0,
            SigRow::ActualParam(..) => 1,
            SigRow::CallTargetAssign(..) => 2,
            SigRow::CalleeInfo(..) => 3,
        }
    }

    /// Orders by content. A signature row holds no instruction, so this needs no interner.
    pub fn content_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (SigRow::Call(a), SigRow::Call(b)) => a.0.content_cmp(&b.0),
            (SigRow::ActualParam(i, v, p), SigRow::ActualParam(j, w, q)) => i
                .cmp(j)
                .then_with(|| v.content_cmp(w))
                .then_with(|| p.content_cmp(q)),
            (SigRow::CallTargetAssign(v, p, t), SigRow::CallTargetAssign(w, q, u)) => v
                .content_cmp(w)
                .then_with(|| p.content_cmp(q))
                .then_with(|| t.content_cmp(u)),
            (SigRow::CalleeInfo(v, p, k), SigRow::CalleeInfo(w, q, l)) => v
                .content_cmp(w)
                .then_with(|| p.content_cmp(q))
                .then_with(|| k.content_cmp(l)),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

/// The signatures of both sides' instructions, each stored once. Not a process-wide interner:
/// it is freed with the diff.
///
/// A signature is stored sorted by content, so two signatures are equal exactly when their
/// slices are, and are ordered by content by comparing the slices row by row.
pub struct SignatureInterner {
    rows: Vec<SigRow>,
    /// Signature `i` is `rows[starts[i]..starts[i + 1]]`.
    starts: Vec<usize>,
    table: HashTable<u32>,
    hasher: FxBuildHasher,
}

impl Default for SignatureInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl SignatureInterner {
    /// The signature of an instruction with no input facts.
    pub const EMPTY: SignatureId = SignatureId(0);

    pub fn new() -> Self {
        let mut s = SignatureInterner {
            rows: Vec::new(),
            starts: vec![0],
            table: HashTable::new(),
            hasher: FxBuildHasher,
        };
        let empty = s.intern(&[]);
        debug_assert_eq!(empty, Self::EMPTY);
        s
    }

    /// The id of `sig`, which must be sorted by content and hold no repeats.
    pub fn intern(&mut self, sig: &[SigRow]) -> SignatureId {
        debug_assert!(sig.is_sorted_by(|a, b| a.content_cmp(b) == Ordering::Less));
        let hash = self.hasher.hash_one(sig);
        let (rows, starts) = (&self.rows, &self.starts);
        let slice = |id: u32| &rows[starts[id as usize]..starts[id as usize + 1]];
        if let Some(&id) = self.table.find(hash, |&id| slice(id) == sig) {
            return SignatureId(id);
        }
        let id = u32::try_from(self.starts.len() - 1).expect("fewer than 2^32 signatures");
        self.rows.extend_from_slice(sig);
        self.starts.push(self.rows.len());
        let (rows, starts, hasher) = (&self.rows, &self.starts, &self.hasher);
        self.table.insert_unique(hash, id, |&id| {
            hasher.hash_one(&rows[starts[id as usize]..starts[id as usize + 1]])
        });
        SignatureId(id)
    }

    /// The rows of signature `id`, sorted by content.
    pub fn get(&self, id: SignatureId) -> &[SigRow] {
        let i = id.0 as usize;
        &self.rows[self.starts[i]..self.starts[i + 1]]
    }

    /// How many distinct signatures there are, the empty one included.
    pub fn len(&self) -> usize {
        self.starts.len() - 1
    }

    /// Always false: the empty signature is always there.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Orders two signatures by content: row by row, a proper prefix first.
    pub fn content_cmp(&self, a: SignatureId, b: SignatureId) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let (a, b) = (self.get(a), self.get(b));
        for (x, y) in a.iter().zip(b) {
            match x.content_cmp(y) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        a.len().cmp(&b.len())
    }
}

/// One side's map from instruction id to canonical instruction, sorted by id.
pub(crate) struct InsnMap {
    map: Vec<(InsnId, CanonInsn)>,
}

impl InsnMap {
    pub(crate) fn empty() -> Self {
        InsnMap { map: Vec::new() }
    }

    /// Reads `dir`'s input tables, one at a time, and names each instruction in them.
    /// `present` says which tables the directory holds.
    pub(crate) fn build(
        dir: &FsPath,
        side: Side,
        present: impl Fn(Table) -> bool,
        fns: &FunctionNames,
        sigs: &mut SignatureInterner,
    ) -> Result<InsnMap, Error> {
        let mut flat: Vec<(InsnId, FnName, SigRow)> = Vec::new();
        if present(Table::Call) {
            let rows = load_raw(dir, side, Table::Call, |d| call::try_load(d))?;
            for (i, row @ (f, insn, target)) in rows.iter().enumerate() {
                let canon = || Ok((fns.get(*f)?, SigRow::Call(fns.get(*target)?)));
                let (f, r) = row_context(Table::Call, side, i, row, canon())?;
                flat.push((*insn, f, r));
            }
        }
        if present(Table::ActualParam) {
            let rows = load_raw(dir, side, Table::ActualParam, |d| actual_param::try_load(d))?;
            for (i, row @ (f, insn, formal, var, path)) in rows.iter().enumerate() {
                let canon = || {
                    Ok((
                        fns.get(*f)?,
                        SigRow::ActualParam(*formal, SigVar::new(*var, *insn)?, *path),
                    ))
                };
                let (f, r) = row_context(Table::ActualParam, side, i, row, canon())?;
                flat.push((*insn, f, r));
            }
        }
        if present(Table::CallTargetAssign) {
            let rows = load_raw(dir, side, Table::CallTargetAssign, |d| {
                call_target_assign::try_load(d)
            })?;
            for (i, row @ (f, insn, var, path, target)) in rows.iter().enumerate() {
                let canon = || {
                    Ok((
                        fns.get(*f)?,
                        SigRow::CallTargetAssign(
                            SigVar::new(*var, *insn)?,
                            *path,
                            CanonTarget::from_object(*target, |f| fns.get(f))?,
                        ),
                    ))
                };
                let (f, r) = row_context(Table::CallTargetAssign, side, i, row, canon())?;
                flat.push((*insn, f, r));
            }
        }
        if present(Table::CalleeInfo) {
            let rows = load_raw(dir, side, Table::CalleeInfo, |d| callee_info::try_load(d))?;
            for (i, row @ (f, insn, var, path, key)) in rows.iter().enumerate() {
                let canon = || {
                    Ok((
                        fns.get(*f)?,
                        SigRow::CalleeInfo(SigVar::new(*var, *insn)?, *path, *key),
                    ))
                };
                let (f, r) = row_context(Table::CalleeInfo, side, i, row, canon())?;
                flat.push((*insn, f, r));
            }
        }
        flat.sort_unstable();
        flat.dedup();

        let mut map = Vec::new();
        let mut sig = Vec::new();
        for group in flat.chunk_by(|x, y| x.0 == y.0) {
            let (insn, function, _) = group[0];
            if let Some((_, other, _)) = group.iter().find(|r| r.1 != function) {
                return Err(Error::new(format!(
                    "instruction {} is in two functions in {side}'s input tables: {} and {}",
                    insn.id,
                    function.as_str(),
                    other.as_str()
                )));
            }
            sig.clear();
            sig.extend(group.iter().map(|r| r.2));
            sig.sort_unstable_by(SigRow::content_cmp);
            let signature = sigs.intern(&sig);
            map.push((
                insn,
                CanonInsn {
                    function,
                    signature,
                },
            ));
        }
        Ok(InsnMap { map })
    }

    /// The canonical instruction `insn`. One in no input table has the empty signature and
    /// `function`, the function of the row that names it.
    pub(crate) fn get(&self, insn: InsnId, function: FnName) -> CanonInsn {
        match self.map.binary_search_by_key(&insn, |e| e.0) {
            Ok(i) => self.map[i].1,
            Err(_) => CanonInsn {
                function,
                signature: SignatureInterner::EMPTY,
            },
        }
    }
}

/// `result`, with an error placed at row `i` of `table` on `side`.
fn row_context<T>(
    table: Table,
    side: Side,
    i: usize,
    row: &impl std::fmt::Debug,
    result: Result<T, String>,
) -> Result<T, Error> {
    result.map_err(|msg| {
        Error::new(format!(
            "{} in {side}, row {i} {row:?}: {msg}",
            table.filename()
        ))
    })
}

/// Prints the instruction's function, then `@`, then its signature in short: the callees,
/// then the actual parameters in parentheses, then any receiver and call targets.
impl DumpCol for CanonInsn {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        out.push_str(self.function.as_str());
        out.push('@');
        let sig = cx.signatures().get(self.signature);
        if sig.is_empty() {
            out.push_str("<no facts>");
            return;
        }
        let mut callees = sig.iter().filter_map(|r| match r {
            SigRow::Call(f) => Some(f.as_str()),
            _ => None,
        });
        match callees.next() {
            None => out.push('?'),
            Some(first) => {
                out.push_str(first);
                for f in callees {
                    write!(out, "|{f}").unwrap();
                }
            }
        }
        out.push('(');
        let mut sep = "";
        for r in sig {
            if let SigRow::ActualParam(i, v, p) = r {
                write!(out, "{sep}{i}:").unwrap();
                v.dump_col(cx, out);
                write!(out, "{p}").unwrap();
                sep = ", ";
            }
        }
        out.push(')');
        for r in sig {
            match r {
                SigRow::CalleeInfo(v, p, k) => {
                    out.push_str(" recv ");
                    v.dump_col(cx, out);
                    write!(out, "{p} {k}").unwrap();
                }
                SigRow::CallTargetAssign(v, p, t) => {
                    out.push(' ');
                    v.dump_col(cx, out);
                    write!(out, "{p}=").unwrap();
                    t.dump_col(cx, out);
                }
                SigRow::Call(_) | SigRow::ActualParam(..) => {}
            }
        }
    }
}
