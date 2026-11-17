//! Each table's canonical row, and the comparison of one table's two sides.

use std::any::Any;
use std::cmp::Ordering;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path as FsPath;

use multiset_diff::{Delta, multiset_diff};
use rustc_hash::FxHashMap;

use ctadl_index::facts::schema::{self, Table};
use ctadl_index::facts::{
    CallArgId, CallDispatchKey, CallTargetObject, FlowEdge, FlowVariable, FlowVariableKind,
    FormalIndex, FunctionId, GlobalId, InsnId, IntentPairKind, PackedInsnSiteId, Path,
    QueryEndpoint, Root, Str, TaintState,
};
use ctadl_index::index_engine::unpack_site;

use crate::canon::{CanonEdge, CanonEndpoint, CanonTarget, CanonVar, ContentOrd, FType, FnName};
use crate::insn::{CanonInsn, InsnMap};
use crate::names::{FunctionNames, ImportNames};
use crate::render::{Cx, Row};
use crate::report::{Sample, Samples, TableReport};
use crate::{Error, Side, error_chain};

/// Loads `table` from `dir` with `load`, its schema module's `try_load`. An error, or a panic in
/// the decoder, becomes an [`Error`] naming the table and the side.
pub(crate) fn load_raw<R>(
    dir: &FsPath,
    side: Side,
    table: Table,
    load: impl FnOnce(&FsPath) -> Result<Vec<R>, ctadl_index::error::Error>,
) -> Result<Vec<R>, Error> {
    let context = || format!("loading {} in {side} ({})", table.name(), dir.display());
    match catch_unwind(AssertUnwindSafe(|| load(dir))) {
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(e)) => Err(Error::new(format!("{}: {}", context(), error_chain(&e)))),
        Err(payload) => Err(Error::new(format!(
            "{}: the decoder panicked: {}",
            context(),
            panic_message(&*payload)
        ))),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s
    } else {
        "a panic with no message"
    }
}

/// What one side's rows are made canonical with.
pub(crate) struct LoadCx<'a> {
    pub(crate) side: Side,
    pub(crate) fns: &'a FunctionNames,
    pub(crate) imports: &'a ImportNames,
    pub(crate) insns: &'a InsnMap,
}

impl LoadCx<'_> {
    fn func(&self, f: FunctionId) -> Result<FnName, String> {
        self.fns.get(f)
    }

    /// Instruction `insn` of function `f`.
    fn insn(&self, f: FnName, insn: InsnId) -> CanonInsn {
        self.insns.get(insn, f)
    }

    /// `v`, a variable of function `f`.
    fn var(&self, f: FnName, v: FlowVariable) -> Result<CanonVar, String> {
        Ok(match v.kind() {
            FlowVariableKind::Uninit => CanonVar::Uninit,
            FlowVariableKind::Local(s) => CanonVar::Local(s),
            FlowVariableKind::Formal(i) => CanonVar::Formal(i),
            FlowVariableKind::Global(g) => CanonVar::Global(g),
            FlowVariableKind::CallArg(packed) => {
                let arg: CallArgId = packed
                    .try_into()
                    .map_err(|e| format!("undecodable call argument {packed:?}: {e}"))?;
                CanonVar::CallArg(self.insn(f, arg.insn_id), arg.formal())
            }
        })
    }

    fn target(&self, t: CallTargetObject) -> Result<CanonTarget, String> {
        CanonTarget::from_object(t, |f| self.func(f))
    }

    fn site(&self, s: PackedInsnSiteId) -> Result<CanonInsn, String> {
        let site = unpack_site(&s);
        Ok(self.insn(self.func(site.func_id)?, site.insn_id))
    }

    fn edge(&self, e: FlowEdge) -> Result<CanonEdge, String> {
        Ok(match e {
            FlowEdge::Intra => CanonEdge::Intra,
            FlowEdge::Global => CanonEdge::Global,
            FlowEdge::Call(s) => CanonEdge::Call(self.site(s)?),
            FlowEdge::Return(s) => CanonEdge::Return(self.site(s)?),
        })
    }

    fn endpoint(&self, e: &QueryEndpoint) -> Result<CanonEndpoint, String> {
        let infunc = self.func(e.infunc)?;
        Ok(CanonEndpoint {
            infunc,
            var: self.var(infunc, e.vertex.0)?,
            path: e.vertex.1,
            label: e.label.0,
            direction: e.direction,
            call_site: e.call_site.map(|s| self.site(s)).transpose()?,
            saturating: e.saturating,
        })
    }
}

/// A table of the schema, as the diff sees it.
pub(crate) trait DiffTable {
    const TABLE: Table;
    /// Whether a row can name an instruction, so that the instruction maps are needed.
    const INSNS: bool;
    type Record: Debug;
    type Canon: Ord + ContentOrd + Row;
    fn try_load(dir: &FsPath) -> Result<Vec<Self::Record>, ctadl_index::error::Error>;
    fn canon(cx: &LoadCx<'_>, row: &Self::Record) -> Result<Self::Canon, String>;
    /// The function the row is about, for the function filter.
    fn function(row: &Self::Canon) -> Option<FnName>;
}

macro_rules! diff_table {
    (
        $ty:ident, $module:ident, insns: $insns:expr,
        |$cx:ident, $row:pat_param| -> $canon:ty $body:block,
        function: |$c:ident| $func:expr
    ) => {
        pub(crate) struct $ty;

        impl DiffTable for $ty {
            const TABLE: Table = Table::$ty;
            const INSNS: bool = $insns;
            type Record = schema::$module::Record;
            type Canon = $canon;

            fn try_load(dir: &FsPath) -> Result<Vec<Self::Record>, ctadl_index::error::Error> {
                schema::$module::try_load(dir)
            }

            fn canon($cx: &LoadCx<'_>, $row: &Self::Record) -> Result<Self::Canon, String> {
                $body
            }

            fn function($c: &Self::Canon) -> Option<FnName> {
                $func
            }
        }
    };
}

/// One type per table, named as its [`Table`] variant.
mod tables {
    use super::*;

    diff_table!(FormalParam, formal_param, insns: false,
        |cx, (f, i, t)| -> (FnName, FormalIndex, FType) {
            Ok((cx.func(*f)?, *i, FType(*t)))
        },
        function: |r| Some(r.0)
    );

    diff_table!(ActualParam, actual_param, insns: true,
        |cx, (f, insn, i, v, p)| -> (FnName, CanonInsn, FormalIndex, CanonVar, Path) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), *i, cx.var(f, *v)?, *p))
        },
        function: |r| Some(r.0)
    );

    diff_table!(Call, call, insns: true,
        |cx, (f, insn, t)| -> (FnName, CanonInsn, FnName) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.func(*t)?))
        },
        function: |r| Some(r.0)
    );

    diff_table!(Assign, assign, insns: true,
        |cx, (f, dv, dp, sv, sp)| -> (FnName, CanonVar, Path, CanonVar, Path) {
            let f = cx.func(*f)?;
            Ok((f, cx.var(f, *dv)?, *dp, cx.var(f, *sv)?, *sp))
        },
        function: |r| Some(r.0)
    );

    diff_table!(CallTargetAssign, call_target_assign, insns: true,
        |cx, (f, insn, v, p, t)| -> (FnName, CanonInsn, CanonVar, Path, CanonTarget) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.var(f, *v)?, *p, cx.target(*t)?))
        },
        function: |r| Some(r.0)
    );

    diff_table!(CalleeInfo, callee_info, insns: true,
        |cx, (f, insn, v, p, k)| -> (FnName, CanonInsn, CanonVar, Path, CallDispatchKey) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.var(f, *v)?, *p, *k))
        },
        function: |r| Some(r.0)
    );

    diff_table!(CalleeResolvents, callee_resolvents, insns: false,
        |cx, (o, k, t)| -> (CanonTarget, CallDispatchKey, FnName) {
            Ok((cx.target(*o)?, *k, cx.func(*t)?))
        },
        function: |r| Some(r.2)
    );

    diff_table!(ResolvedCall, resolved_call, insns: true,
        |cx, (f, insn, t)| -> (FnName, CanonInsn, FnName) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.func(*t)?))
        },
        function: |r| Some(r.0)
    );

    diff_table!(Summary, summary, insns: false,
        |cx, (f, d, dp, s, sp)| -> (FnName, Root, Path, Root, Path) {
            Ok((cx.func(*f)?, *d, *dp, *s, *sp))
        },
        function: |r| Some(r.0)
    );

    diff_table!(Globals, globals, insns: false,
        |cx, (g, import, _span)| -> (GlobalId, Str) {
            Ok((*g, cx.imports.get(*import)?))
        },
        function: |_r| None
    );

    diff_table!(GlobalFlow, global_flow, insns: false,
        |_cx, (d, dp, s, sp)| -> (GlobalId, Path, GlobalId, Path) {
            Ok((*d, *dp, *s, *sp))
        },
        function: |_r| None
    );

    diff_table!(Paths, paths, insns: false,
        |_cx, (p,)| -> (Path,) {
            Ok((*p,))
        },
        function: |_r| None
    );

    diff_table!(Taint, taint, insns: true,
        |cx, (f, s, v, p, e)| -> (FnName, TaintState, CanonVar, Path, CanonEndpoint) {
            let f = cx.func(*f)?;
            Ok((f, *s, cx.var(f, *v)?, *p, cx.endpoint(e)?))
        },
        function: |r| Some(r.0)
    );

    diff_table!(TaintEdge, taint_edge, insns: true,
        |cx, (e, sf, sv, sp, df, dv, dp)|
            -> (CanonEdge, FnName, CanonVar, Path, FnName, CanonVar, Path) {
            let (sf, df) = (cx.func(*sf)?, cx.func(*df)?);
            Ok((cx.edge(*e)?, sf, cx.var(sf, *sv)?, *sp, df, cx.var(df, *dv)?, *dp))
        },
        function: |r| Some(r.1)
    );

    diff_table!(IndexSourceMap, index_source_map, insns: true,
        |cx, (f, insn, _span, import)| -> (FnName, CanonInsn, Str) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.imports.get(*import)?))
        },
        function: |r| Some(r.0)
    );

    diff_table!(ImportId, import_id, insns: false,
        |_cx, (_id, name)| -> (String,) {
            Ok((name.clone(),))
        },
        function: |_r| None
    );

    diff_table!(FunctionId, function_id, insns: false,
        |_cx, (_id, name)| -> (FnName,) {
            Ok((FnName(name.0),))
        },
        function: |r| Some(r.0)
    );

    diff_table!(ExternalFunction, external_function, insns: false,
        |cx, (f,)| -> (FnName,) {
            Ok((cx.func(*f)?,))
        },
        function: |r| Some(r.0)
    );

    diff_table!(IntentPair, intent_pair, insns: true,
        |cx, (f, insn, r, k)| -> (FnName, CanonInsn, FnName, IntentPairKind) {
            let f = cx.func(*f)?;
            Ok((f, cx.insn(f, *insn), cx.func(*r)?, *k))
        },
        function: |r| Some(r.0)
    );

    // The manifest tables' node ids are positions in one manifest, and these tables are written to
    // import directories, never to an index; their rows are compared as they are.
    diff_table!(ManifestNode, manifest_node, insns: false,
        |_cx, (n, tag)| -> (u32, String) {
            Ok((*n, tag.clone()))
        },
        function: |_r| None
    );

    diff_table!(ManifestNodeChild, manifest_node_child, insns: false,
        |_cx, (p, c)| -> (u32, u32) {
            Ok((*p, *c))
        },
        function: |_r| None
    );

    diff_table!(ManifestNodeAttr, manifest_node_attr, insns: false,
        |_cx, (n, k, v)| -> (u32, String, String) {
            Ok((*n, k.clone(), v.clone()))
        },
        function: |_r| None
    );
}

/// Whether `table`'s rows can name an instruction.
pub(crate) fn needs_insns(table: Table) -> bool {
    match table {
        Table::FormalParam => tables::FormalParam::INSNS,
        Table::ActualParam => tables::ActualParam::INSNS,
        Table::Call => tables::Call::INSNS,
        Table::Assign => tables::Assign::INSNS,
        Table::CallTargetAssign => tables::CallTargetAssign::INSNS,
        Table::CalleeInfo => tables::CalleeInfo::INSNS,
        Table::CalleeResolvents => tables::CalleeResolvents::INSNS,
        Table::ResolvedCall => tables::ResolvedCall::INSNS,
        Table::Summary => tables::Summary::INSNS,
        Table::Globals => tables::Globals::INSNS,
        Table::GlobalFlow => tables::GlobalFlow::INSNS,
        Table::Paths => tables::Paths::INSNS,
        Table::Taint => tables::Taint::INSNS,
        Table::TaintEdge => tables::TaintEdge::INSNS,
        Table::IndexSourceMap => tables::IndexSourceMap::INSNS,
        Table::ImportId => tables::ImportId::INSNS,
        Table::FunctionId => tables::FunctionId::INSNS,
        Table::ExternalFunction => tables::ExternalFunction::INSNS,
        Table::IntentPair => tables::IntentPair::INSNS,
        Table::ManifestNode => tables::ManifestNode::INSNS,
        Table::ManifestNodeChild => tables::ManifestNodeChild::INSNS,
        Table::ManifestNodeAttr => tables::ManifestNodeAttr::INSNS,
    }
}

/// Keeps the rows whose function's name contains a substring, deciding once per function.
pub(crate) struct FunctionFilter<'a> {
    substr: Option<&'a str>,
    seen: FxHashMap<FnName, bool>,
}

impl<'a> FunctionFilter<'a> {
    pub(crate) fn new(substr: Option<&'a str>) -> Self {
        FunctionFilter {
            substr,
            seen: FxHashMap::default(),
        }
    }

    /// With no filter, every row; with one, the rows about a function that matches. A row
    /// about no function never matches.
    fn keeps(&mut self, function: Option<FnName>) -> bool {
        let Some(substr) = self.substr else {
            return true;
        };
        let Some(f) = function else {
            return false;
        };
        *self
            .seen
            .entry(f)
            .or_insert_with(|| f.as_str().contains(substr))
    }
}

/// Where and how the two sides of a table are compared.
pub(crate) struct TablePair<'a> {
    pub(crate) dirs: [&'a FsPath; 2],
    pub(crate) present: [bool; 2],
    pub(crate) load: [LoadCx<'a>; 2],
    pub(crate) cx: &'a Cx<'a>,
    pub(crate) limit: usize,
}

/// One side's canonical rows of `T`, filtered: empty if the side has no such table.
fn load_canon<T: DiffTable>(
    pair: &TablePair<'_>,
    side: usize,
    filter: &mut FunctionFilter<'_>,
) -> Result<Vec<T::Canon>, Error> {
    if !pair.present[side] {
        return Ok(Vec::new());
    }
    let cx = &pair.load[side];
    let raw = load_raw(pair.dirs[side], cx.side, T::TABLE, T::try_load)?;
    let mut rows = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        let c = T::canon(cx, r).map_err(|msg| {
            Error::new(format!(
                "{} in {}, row {i} {r:?}: {msg}",
                T::TABLE.filename(),
                cx.side
            ))
        })?;
        if filter.keeps(T::function(&c)) {
            rows.push(c);
        }
    }
    drop(raw);
    rows.shrink_to_fit();
    Ok(rows)
}

/// The first `limit` of the values pushed, by `cmp`, kept in a buffer of at most `2 * limit`.
struct Smallest<'r, T> {
    limit: usize,
    buf: Vec<Delta<'r, T>>,
    total: usize,
}

impl<'r, T> Smallest<'r, T> {
    fn new(limit: usize) -> Self {
        Smallest {
            limit,
            buf: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, d: Delta<'r, T>, cmp: impl Fn(&T, &T) -> Ordering) {
        self.total += 1;
        if self.limit == 0 {
            return;
        }
        self.buf.push(d);
        if self.buf.len() >= self.limit.saturating_mul(2) {
            self.buf
                .select_nth_unstable_by(self.limit, |x, y| cmp(x.row, y.row));
            self.buf.truncate(self.limit);
        }
    }

    fn finish(
        mut self,
        cmp: impl Fn(&T, &T) -> Ordering,
        render: impl Fn(&T) -> String,
    ) -> Samples {
        self.buf.sort_unstable_by(|x, y| cmp(x.row, y.row));
        self.buf.truncate(self.limit);
        Samples {
            not_shown: self.total - self.buf.len(),
            rows: self
                .buf
                .iter()
                .map(|d| Sample {
                    row: render(d.row),
                    count_a: d.count_a,
                    count_b: d.count_b,
                })
                .collect(),
        }
    }
}

/// Compares the two sides of `T`.
fn diff_table<T: DiffTable>(
    pair: &TablePair<'_>,
    filter: &mut FunctionFilter<'_>,
) -> Result<TableReport, Error> {
    let mut a = load_canon::<T>(pair, 0, filter)?;
    a.sort_unstable();
    let mut b = load_canon::<T>(pair, 1, filter)?;
    b.sort_unstable();

    let cx = pair.cx;
    let cmp = |x: &T::Canon, y: &T::Canon| x.content_order(y, cx);
    let (mut only_in_a, mut only_in_b, mut count_differs) = (0, 0, 0);
    let mut more_in_a = Smallest::new(pair.limit);
    let mut more_in_b = Smallest::new(pair.limit);
    for d in multiset_diff(&a, &b, Ord::cmp) {
        match (d.count_a, d.count_b) {
            (_, 0) => only_in_a += 1,
            (0, _) => only_in_b += 1,
            _ => count_differs += 1,
        }
        if d.count_a > d.count_b {
            more_in_a.push(d, cmp);
        } else {
            more_in_b.push(d, cmp);
        }
    }
    let render = |row: &T::Canon| {
        let mut s = String::new();
        row.row(cx, &mut s);
        s
    };
    Ok(TableReport {
        table: T::TABLE.name().to_string(),
        in_a: pair.present[0],
        in_b: pair.present[1],
        rows_a: a.len(),
        rows_b: b.len(),
        only_in_a,
        only_in_b,
        count_differs,
        more_in_a: more_in_a.finish(cmp, render),
        more_in_b: more_in_b.finish(cmp, render),
    })
}

/// Compares the two sides of `table`. Each table's rows are freed before this returns.
pub(crate) fn diff_one(
    table: Table,
    pair: &TablePair<'_>,
    filter: &mut FunctionFilter<'_>,
) -> Result<TableReport, Error> {
    match table {
        Table::FormalParam => diff_table::<tables::FormalParam>(pair, filter),
        Table::ActualParam => diff_table::<tables::ActualParam>(pair, filter),
        Table::Call => diff_table::<tables::Call>(pair, filter),
        Table::Assign => diff_table::<tables::Assign>(pair, filter),
        Table::CallTargetAssign => diff_table::<tables::CallTargetAssign>(pair, filter),
        Table::CalleeInfo => diff_table::<tables::CalleeInfo>(pair, filter),
        Table::CalleeResolvents => diff_table::<tables::CalleeResolvents>(pair, filter),
        Table::ResolvedCall => diff_table::<tables::ResolvedCall>(pair, filter),
        Table::Summary => diff_table::<tables::Summary>(pair, filter),
        Table::Globals => diff_table::<tables::Globals>(pair, filter),
        Table::GlobalFlow => diff_table::<tables::GlobalFlow>(pair, filter),
        Table::Paths => diff_table::<tables::Paths>(pair, filter),
        Table::Taint => diff_table::<tables::Taint>(pair, filter),
        Table::TaintEdge => diff_table::<tables::TaintEdge>(pair, filter),
        Table::IndexSourceMap => diff_table::<tables::IndexSourceMap>(pair, filter),
        Table::ImportId => diff_table::<tables::ImportId>(pair, filter),
        Table::FunctionId => diff_table::<tables::FunctionId>(pair, filter),
        Table::ExternalFunction => diff_table::<tables::ExternalFunction>(pair, filter),
        Table::IntentPair => diff_table::<tables::IntentPair>(pair, filter),
        Table::ManifestNode => diff_table::<tables::ManifestNode>(pair, filter),
        Table::ManifestNodeChild => diff_table::<tables::ManifestNodeChild>(pair, filter),
        Table::ManifestNodeAttr => diff_table::<tables::ManifestNodeAttr>(pair, filter),
    }
}
