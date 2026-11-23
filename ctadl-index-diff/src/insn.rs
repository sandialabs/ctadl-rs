//! Canonical instructions: an instruction named by its function and its input facts.
//!
//! An instruction's signature is the set of rows of `call`, `actual_param`,
//! `call_target_assign` and `callee_info` that name it, each with the instruction column
//! removed, function ids replaced by names, and a call argument of the instruction itself
//! replaced by [`SigVar::This`]. These tables are written before the fixpoint, so a change in
//! what the index derives never renames an instruction.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::Path as FsPath;

use index_canon::Names;

use ctadl_index::facts::schema::{Table, actual_param, call, call_target_assign, callee_info};
use ctadl_index::facts::{
    CallArgId, CallDispatchKey, FlowVariable, FlowVariableKind, FormalIndex, GlobalId, InsnId,
    Path, Str,
};

use crate::canon::{CanonTarget, FnName};
use crate::names::FunctionNames;
use crate::render::{Col, Cx};
use crate::table::load_raw;
use crate::{Error, Side};

/// A signature's id in [`Signatures`]. Ids are ordered as their signatures are by content: row
/// by row, a proper prefix first.
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

/// Both sides' distinct instruction signatures, each stored once, with the ids
/// [`index_canon::Names`] gives them: equal exactly when the facts are, and ordered as the
/// signatures are by content.
pub struct Signatures {
    rows: Vec<SigRow>,
    /// Signature `id` is `rows[starts[id]..starts[id + 1]]`.
    starts: Vec<usize>,
}

impl Signatures {
    /// The signature of an instruction with no input facts.
    pub const EMPTY: SignatureId = SignatureId(0);

    /// No signatures but the empty one, for a diff of tables that name no instruction.
    pub fn empty() -> Self {
        Signatures {
            rows: Vec::new(),
            starts: vec![0, 0],
        }
    }

    /// Each of `names`' signatures, once.
    fn of(names: &Names<InsnId, SigRow>) -> Self {
        let mut sigs = Signatures {
            rows: Vec::new(),
            starts: vec![0],
        };
        for id in 0..names.len() {
            let id = u32::try_from(id).expect("fewer than 2^32 signatures");
            sigs.rows.extend_from_slice(names.facts_of(id));
            sigs.starts.push(sigs.rows.len());
        }
        sigs
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
}

/// One side's map from instruction id to canonical instruction, sorted by id.
pub(crate) struct InsnMap {
    map: Vec<(InsnId, CanonInsn)>,
}

/// One side's input facts: each instruction's function, and its signature rows, sorted by
/// instruction and then by content.
struct SideFacts {
    functions: Vec<(InsnId, FnName)>,
    rows: Vec<(InsnId, SigRow)>,
}

impl InsnMap {
    pub(crate) fn empty() -> Self {
        InsnMap { map: Vec::new() }
    }

    /// Reads both sides' input tables and names each instruction in them by its facts.
    /// `present[i]` says which tables side `i`'s directory holds.
    pub(crate) fn build_both(
        dirs: [&FsPath; 2],
        present: [&dyn Fn(Table) -> bool; 2],
        fns: [&FunctionNames; 2],
    ) -> Result<(Signatures, [InsnMap; 2]), Error> {
        let a = SideFacts::load(dirs[0], Side::A, present[0], fns[0])?;
        let b = SideFacts::load(dirs[1], Side::B, present[1], fns[1])?;
        let names = Names::build(&a.rows, &b.rows, &InsnId::cmp, &SigRow::content_cmp).ok_or_else(
            || Error::new("naming instructions: input facts out of order or too many"),
        )?;
        let map = |facts: SideFacts, side_b: bool| InsnMap {
            map: facts
                .functions
                .into_iter()
                .map(|(insn, function)| {
                    let signature = SignatureId(names.name(side_b, &insn, &InsnId::cmp));
                    (
                        insn,
                        CanonInsn {
                            function,
                            signature,
                        },
                    )
                })
                .collect(),
        };
        let maps = [map(a, false), map(b, true)];
        // Every instruction's facts are no longer needed, only each signature's, once.
        Ok((Signatures::of(&names), maps))
    }

    /// The canonical instruction `insn`. One in no input table has the empty signature and
    /// `function`, the function of the row that names it.
    pub(crate) fn get(&self, insn: InsnId, function: FnName) -> CanonInsn {
        match self.map.binary_search_by_key(&insn, |e| e.0) {
            Ok(i) => self.map[i].1,
            Err(_) => CanonInsn {
                function,
                signature: Signatures::EMPTY,
            },
        }
    }
}

impl SideFacts {
    /// Reads `dir`'s input tables, one at a time. `present` says which tables the directory
    /// holds.
    fn load(
        dir: &FsPath,
        side: Side,
        present: &dyn Fn(Table) -> bool,
        fns: &FunctionNames,
    ) -> Result<SideFacts, Error> {
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
        // By instruction, then function, so each instruction's rows are together and a second
        // function shows up in them.
        flat.sort_unstable_by(|x, y| x.0.cmp(&y.0).then_with(|| x.1.cmp(&y.1)));

        let mut functions = Vec::new();
        let mut rows = Vec::with_capacity(flat.len());
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
            functions.push((insn, function));
            let start = rows.len();
            rows.extend(group.iter().map(|r| (insn, r.2)));
            rows[start..].sort_unstable_by(|x, y| x.1.content_cmp(&y.1));
        }
        Ok(SideFacts { functions, rows })
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
impl Col for CanonInsn {
    fn col(&self, cx: &Cx<'_>, out: &mut String) {
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
                v.col(cx, out);
                write!(out, "{p}").unwrap();
                sep = ", ";
            }
        }
        out.push(')');
        for r in sig {
            match r {
                SigRow::CalleeInfo(v, p, k) => {
                    out.push_str(" recv ");
                    v.col(cx, out);
                    write!(out, "{p} {k}").unwrap();
                }
                SigRow::CallTargetAssign(v, p, t) => {
                    out.push(' ');
                    v.col(cx, out);
                    write!(out, "{p}=").unwrap();
                    t.col(cx, out);
                }
                SigRow::Call(_) | SigRow::ActualParam(..) => {}
            }
        }
    }
}
