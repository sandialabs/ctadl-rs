use ctadl_ascent::facts as fx;
use ctadl_import::error::Error;

use crate as tree_sitter_c;
use anyhow::Result;
use ctadl_ascent::codegen::{CallResolutionStrategy, RETURN_INDEX, codegen_program};
use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
use ctadl_ascent::index_engine::{FunctionSummary, IndexFacts, taint_index};
// `DirectedGraph`/`Successors` are trait imports: they provide `num_nodes()` (used by
// `check_block_count`) and `successors()` (used by `check_successors`). They look unused but
// removing them breaks method resolution.
use ctadl_ascent::facts::Path;
use ctadl_ir::cir::TerminatorKind;
use ctadl_ir::cir::call::{CallEdges, CallStyle};
use ctadl_ir::cir::{GlobalIdx, Globals, LocalIdx, Locals};
use ctadl_ir::graph::{DirectedGraph, Successors};
use ctadl_ir::{
    AccessPath, BasicBlockIdx, Exp, FunctionData, Idx, Statement, StatementKind, VariableRef, ssa,
};
use ctadl_ir::{ParameterType, PathSegment, Program, ProgramInfo};

pub(crate) fn init_test_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug) // This forces it to Debug
        .is_test(true)
        .try_init();
}

/* Compile a program from a string. */
pub(crate) fn program_from_string(src: &str) -> (Program, String) {
    let result = tree_sitter_c::parse_c_program(src).expect("Failed to parse C program.");
    assert!(
        !result.1,
        "Input Program failed to parse without error from Tree-sitter"
    );
    // A block with no terminator is always a CFG defect, so fail loudly here rather
    // than let a test silently pass on a malformed control-flow graph.
    assert!(
        !result.2.contains("<no terminator>"),
        "Parsed IR contains a block with no terminator:\n{}",
        result.2
    );
    (result.0, result.2)
}

/* Lower several named files, each a translation unit of its own, into ONE program -- what
`import_c` does with a directory, and the only way to write a test whose answer depends on
which FILE a construct came from (`program_from_string` parses one unit with no file to
name). This goes through the import path, so the program also carries the extern stubs
`define_extern_functions` creates. */
pub(crate) fn program_from_files(files: &[(&str, &str)]) -> (Program, String) {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(name, src)| ((*name).to_string(), (*src).to_string()))
        .collect();
    let (program, dump) = tree_sitter_c::parse_c_files(&owned).expect("Failed to parse C program.");
    // A block with no terminator is always a CFG defect, so fail loudly here rather than
    // let a test silently pass on a malformed control-flow graph.
    assert!(
        !dump.contains("<no terminator>"),
        "Parsed IR contains a block with no terminator:\n{dump}"
    );
    (program, dump)
}

/* Common output for when tests fail. */
pub(crate) fn check_fail_str(prog_str: &str, msg: &str) {
    log::warn!("TEST FAIL: {msg}");
    log::warn!("\t{prog_str}");
}

pub(crate) fn check_fail(prog: &Program, msg: &str) {
    let prog_str = prog.to_string();
    check_fail_str(&prog_str, msg);
}

/* A test to check a particular program parsed N functions. */
pub(crate) fn check_function_count(prog: &Program, count: usize) -> bool {
    let len = prog.functions.len();
    if len != count {
        let err = format!("{} functions in parsed program, expected {}.", len, count);
        check_fail(prog, &err);
        return false;
    }
    true
}

pub(crate) fn get_only_function(prog: &Program) -> Option<&FunctionData> {
    if !check_function_count(prog, 1) {
        return None;
    }
    prog.functions.functions.raw.first()
}

/* Returns the function named `name`, or None. The C frontend does not overload, so the name
uniquely identifies a function -- use this (not `get_only_function`) for fixtures with several
functions. */
pub(crate) fn function_named<'a>(prog: &'a Program, name: &str) -> Option<&'a FunctionData> {
    prog.functions.functions.raw.iter().find(|f| f.name == name)
}

/* Renders the local named `local` in function `func` the way the IR dump does (`%L{idx}`), by
looking its interned `LocalIdx` up in the function's locals table. Lets dump-based assertions be
written in terms of the readable source name rather than a hard-coded, hard-to-follow index. Panics
if the function or local does not exist. */
#[track_caller]
pub(crate) fn local_render(prog: &Program, func: &str, local: &str) -> String {
    let f =
        function_named(prog, func).unwrap_or_else(|| panic!("no function named {func:?}\n{prog}"));
    let idx = f
        .locals
        .iter_enumerated()
        .find(|(_, decl)| decl.name.as_str() == local)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| panic!("no local named {local:?} in function {func:?}\n{prog}"));
    format!("%L{}", idx.index())
}

/* The `VariableRef` of the local named `local` in function `func`, for assertions that compare a
statement's variable directly (a call argument, a store destination) rather than through the dump.
The name-to-`LocalIdx` lookup is `local_render`'s, minus the rendering. Panics if the function or
local does not exist. */
#[track_caller]
pub(crate) fn local_ref(prog: &Program, func: &str, local: &str) -> VariableRef {
    let f =
        function_named(prog, func).unwrap_or_else(|| panic!("no function named {func:?}\n{prog}"));
    let idx = f
        .locals
        .iter_enumerated()
        .find(|(_, decl)| decl.name.as_str() == local)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| panic!("no local named {local:?} in function {func:?}\n{prog}"));
    VariableRef::new_local_idx(idx)
}

/* The argument list of the (first) direct call to `callee` in function `caller`. The narrow
extraction behind argument-shape assertions -- unlike `check_loads` and friends it works on a
multi-function fixture, which any call site necessarily is. Panics if there is no such call. */
#[track_caller]
pub(crate) fn call_args(prog: &Program, caller: &str, callee: &str) -> Vec<Exp> {
    let calls = direct_calls_in(prog, caller);
    calls
        .into_iter()
        .find(|(callees, _)| callees.iter().any(|c| c == callee))
        .map(|(_, args)| args)
        .unwrap_or_else(|| panic!("no direct call to {callee:?} in {caller:?}\n{prog}"))
}

/* Asserts the function `name` has the given return arity (the `N` in the dump's `define name() ->
N`): a value-returning function is arity 1, a `void` function is arity 0. Panics at the caller's
line on mismatch. */
#[track_caller]
pub(crate) fn check_return_arity(prog: &Program, name: &str, arity: u8) {
    let fun =
        function_named(prog, name).unwrap_or_else(|| panic!("no function named {name:?}\n{prog}"));
    assert_eq!(
        fun.return_type.arity, arity,
        "return arity mismatch for {name:?}\n{prog}"
    );
}

/* Asserts the function `name` has a `return` terminator returning exactly the single constant
`value` (e.g. `return (14)` => value "14"). Constants lower to `Exp::Str` of the literal's source
text, so this matches a `Return` whose args are `[Exp::Str(value)]`. On failure prints the actual
return arg-lists found. Panics at the caller's line. */
#[track_caller]
pub(crate) fn check_returns_const(prog: &Program, name: &str, value: &str) {
    let fun =
        function_named(prog, name).unwrap_or_else(|| panic!("no function named {name:?}\n{prog}"));
    let expected = [Exp::new_str(value)];
    let returns: Vec<&[Exp]> = fun
        .blocks
        .iter()
        .filter_map(|b| match &b.terminator {
            Some(t) => match &t.kind {
                TerminatorKind::Return { args } => Some(args.as_slice()),
                _ => None,
            },
            None => None,
        })
        .collect();
    assert!(
        returns.iter().any(|args| *args == expected),
        "expected a `return ({value})` in {name:?}; found returns {returns:?}\n{prog}"
    );
}

/* Extracts every direct call (`CallStyle::DirectCall`) in function `name`, as `(callees, args)`
pairs in source order: `callees` are the resolved callee name(s) on the call edge, `args` the
argument expressions (variables, addresses or constants). Returns an empty Vec if the function has no direct
calls (or doesn't exist). The extraction primitive behind `check_direct_call` / the dex precedent of
"pull out the narrow thing, then assert on it". */
pub(crate) fn direct_calls_in(prog: &Program, name: &str) -> Vec<(Vec<String>, Vec<Exp>)> {
    let Some(fun) = function_named(prog, name) else {
        return Vec::new();
    };
    fun.blocks
        .iter()
        .flat_map(|b| b.statements.iter())
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::CallAssign {
                style: CallStyle::DirectCall { call_edges },
                args,
                ..
            } => {
                let CallEdges::Explicit(edges) = call_edges;
                Some((edges.to_vec(), args.to_vec()))
            }
            _ => None,
        })
        .collect()
}

/* Asserts function `name` contains a direct call to `callee` with exactly the given argument access
paths (DSL strings, same as `check_assign_or_update`'s sources -- e.g. `["@p0"]`). Prefer this over
asserting the dump's `direct-call` text. On failure prints the direct calls actually found. Panics
at the caller's line. */
#[track_caller]
pub(crate) fn check_direct_call<I>(prog: &Program, name: &str, callee: &str, args: I)
where
    I: IntoIterator<Item = &'static str>,
{
    let locals = &function_named(prog, name)
        .expect("expected function to exist")
        .locals;
    let want_args: Vec<Exp> = args
        .into_iter()
        .map(|s| exp_from_str(s, locals, &prog.globals))
        .collect();
    let calls = direct_calls_in(prog, name);
    let found = calls
        .iter()
        .any(|(callees, a)| callees.iter().any(|c| c == callee) && *a == want_args);
    assert!(
        found,
        "expected a direct call to {callee:?} with args {want_args:?} in {name:?}; \
         found direct calls {calls:?}\n{prog}"
    );
}

/* Asserts function `name` contains a direct call to `callee`, without constraining the arguments.
Use this when the argument is an incidental temp (e.g. a nested call's result) that shouldn't be
pinned. Panics at the caller's line; prints the direct calls found on failure. */
#[track_caller]
pub(crate) fn check_has_direct_call(prog: &Program, name: &str, callee: &str) {
    let calls = direct_calls_in(prog, name);
    let found = calls
        .iter()
        .any(|(callees, _)| callees.iter().any(|c| c == callee));
    assert!(
        found,
        "expected a direct call to {callee:?} in {name:?}; found direct calls {calls:?}\n{prog}"
    );
}

/* Asserts a function has exactly the given parameter types. Panics (at the caller's line, via
`#[track_caller]`) with an expected-vs-actual diff on mismatch. */
#[track_caller]
pub(crate) fn check_params(prog: &Program, expected: &[ParameterType]) {
    let fun = get_only_function(prog).expect("expected exactly one function");
    assert_eq!(
        fun.params.parameters.raw.as_slice(),
        expected,
        "parameter mismatch\n{prog}"
    );
}

/* Asserts the (single) function has exactly `count` basic blocks. Implicitly asserts exactly one
function. Panics at the caller's line on mismatch. */
#[track_caller]
pub(crate) fn check_block_count(prog: &Program, count: usize) {
    let fun = get_only_function(prog).expect("expected exactly one function");
    assert_eq!(
        fun.blocks.num_nodes(),
        count,
        "block count mismatch\n{prog}"
    );
}

/* Asserts block `block` of the (single) function has exactly `expected` as its successor block ids
(order-insensitive). An empty slice means a terminal block (e.g. a `return`). Asserts on the real
terminator graph, not the dump. Panics at the caller's line on mismatch. */
#[track_caller]
pub(crate) fn check_successors(prog: &Program, block: usize, expected: &[usize]) {
    let fun = get_only_function(prog).expect("expected exactly one function");
    let mut got: Vec<usize> = fun
        .blocks
        .successors(BasicBlockIdx::new(block))
        .map(|b| b.index())
        .collect();
    got.sort_unstable();
    let mut want = expected.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "block {block} successor mismatch\n{prog}");
}

// A debugging aid for inspecting parsed blocks.
/// Parses the path tail of a DSL access-path string with the one canonical grammar
/// ([`ctadl_ir::cir::path_syntax`]), so a fixture means exactly what the same text means in a
/// model port, a `.flowy` file, or a fact column.
///
/// Panics on a malformed path — that is a broken fixture, and failing loudly beats the old
/// behavior of silently dropping empty segments.
#[track_caller]
fn parse_path(s: &str) -> Vec<PathSegment> {
    ctadl_ir::cir::parse_segments(s)
        .unwrap_or_else(|e| panic!("malformed access path {s:?} in test DSL: {e}"))
}

/// Resolves a local's DSL name to a `VariableRef` using the same `Locals` table the parser built,
/// so the interned `LocalIdx` matches the IR under test. If the name was never interned (e.g. an
/// assertion for a local that does not exist), a one-past-the-end index is used so the resulting
/// ref compares unequal to every real local rather than panicking.
fn resolve_local(locals: &Locals, name: &str) -> VariableRef {
    let idx = locals
        .iter_enumerated()
        .find(|(_, decl)| decl.name.as_str() == name)
        .map(|(idx, _)| idx)
        .unwrap_or_else(|| LocalIdx::new(locals.len()));
    VariableRef::new_local_idx(idx)
}

/// Splits a DSL string at the first `.` into the variable prefix and the path, *keeping*
/// the dot on the path — `"v.f1.f2"` -> `("v", ".f1.f2")`, `"v"` -> `("v", "")`. The path half is
/// then in the canonical grammar, where every segment carries its leading dot.
fn split_variable_prefix(s: &str) -> (&str, &str) {
    match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// `$name.p` is the global `name` of `globals`, in the canonical grammar
/// ([`ctadl_ir::cir::path_syntax::parse_global_access_path`]). A name `globals` does not declare
/// is a global no program has, so a check for it finds nothing.
pub(crate) fn access_path_from_str(s: &str, locals: &Locals, globals: &Globals) -> AccessPath {
    if let Some((name, path)) = ctadl_ir::cir::path_syntax::parse_global_access_path(s)
        .unwrap_or_else(|e| panic!("bad global access path {s:?}: {e}"))
    {
        let idx = globals.lookup(&name).unwrap_or(GlobalIdx::new(0xffff_ff00));
        return AccessPath::new(VariableRef::global(idx), path);
    }

    if let Some(rest) = s.strip_prefix("@p") {
        let (n_str, suffix) = split_variable_prefix(rest);
        let n: u32 = n_str.parse().expect("invalid parameter index in @pN");
        return AccessPath::new(VariableRef::new_parameter(n.into()), parse_path(suffix));
    }

    let (base, suffix) = split_variable_prefix(s);
    AccessPath::new(resolve_local(locals, base), parse_path(suffix))
}

/* Builds a source expression from the DSL. A `#`-prefixed string is a constant literal (`"#7"` =>
`Exp::Str("7")`, matching how the C frontend lowers a literal — see `flatten_expr` in mod.rs);
`value(p)` is `Exp::Value` of the access path `p`; anything else is an access path (variable /
param / global / field). */
pub(crate) fn exp_from_str(s: &str, locals: &Locals, globals: &Globals) -> Exp {
    if let Some(inner) = s.strip_prefix("value(").and_then(|r| r.strip_suffix(')')) {
        return Exp::value(access_path_from_str(inner, locals, globals));
    }
    match s.strip_prefix('#') {
        Some(lit) => Exp::new_str(lit),
        None => {
            let dsl = access_path_from_str(s, locals, globals);
            assert!(
                dsl.is_pathless(),
                "DSL source with a path is not directly expressible; use check_loads: {s}"
            );
            Exp::variable(dsl.base)
        }
    }
}

/* Asserts the (single) function contains the given assignment or update (an update is expected when
`dst` has a non-empty path). If `block_id` is given, only that block is searched. Sources are DSL strings
(access paths, or `#literal` for a constant). Intended for Programs parsed from a single function.
Panics at the caller's line if not found. */
#[track_caller]
pub(crate) fn check_assign_or_update<I>(
    prog: &Program,
    dst: &str,
    src_strs: I,
    block_id: Option<usize>,
) where
    I: IntoIterator<Item = &'static str>,
{
    let locals = &get_only_function(prog)
        .expect("expected exactly one function")
        .locals;
    let srcs: Vec<Exp> = src_strs
        .into_iter()
        .map(|s| exp_from_str(s, locals, &prog.globals))
        .collect();

    let dst_ap = access_path_from_str(dst, locals, &prog.globals);

    if !dst_ap.is_pathless() {
        assert_eq!(
            srcs.len(),
            1,
            "update destination requires exactly one source expression"
        );
    }
    let expected = StatementKind::assign(dst_ap, srcs);

    let fun = get_only_function(prog).expect("expected exactly one function");

    let found = fun.blocks.iter().enumerate().any(|(i, block)| {
        if let Some(req_block) = block_id
            && i != req_block
        {
            return false;
        }
        block.statements.iter().any(|stmt| stmt.kind == expected)
    });

    assert!(
        found,
        "could not find `{expected}` in function {}\n{prog}",
        fun.name
    );
}

/* Asserts the (single) function contains a load -- a copy into a variable -- that reads
`source_str` (an access-path DSL string like `f.[3].deref` or `$a.deref`). Field reads lower to
loads through a temporary, so this is the load-based complement to `check_assign_or_update`.
Panics if not found. */
#[track_caller]
pub(crate) fn check_loads(prog: &Program, source_str: &str) {
    let locals = &get_only_function(prog)
        .expect("expected exactly one function")
        .locals;
    let ap = access_path_from_str(source_str, locals, &prog.globals);
    let found = statements_of(prog).any(|s| {
        let StatementKind::Assign { dest, sources } = &s.kind else {
            return false;
        };
        dest.is_pathless() && sources.iter().any(|e| e.access_path() == Some(&ap))
    });
    assert!(found, "could not find a load of `{source_str}`\n{prog}");
}

/* Iterates every statement of the (single) function, in block-then-statement order. The shared walk
behind the destination-focused helpers below, and a handy primitive for ad-hoc structural assertions
that need to scan statements (cf. the dex frontend's "pull out the narrow thing, then assert on it").
Panics if the program isn't a single function. */
pub(crate) fn statements_of(prog: &Program) -> impl Iterator<Item = &Statement> {
    let fun = get_only_function(prog).expect("expected exactly one function");
    fun.blocks.iter().flat_map(|b| b.statements.iter())
}

/* True if `kind` writes to `dst`, comparing only the destination -- the source(s) are ignored. A
bare `dst` (no path) matches an `Assign` to that variable; a `dst` with a path matches a `Store`
to it. This is the same assign-vs-update split as `check_assign_or_update`'s
destination, minus the source constraint. */
fn writes_dest(kind: &StatementKind, dst: &AccessPath) -> bool {
    matches!(kind, StatementKind::Assign { dest, .. } if dest == dst)
}

/* Counts the statements of the (single) function that write to `dst`, ignoring the source
expression. Destination DSL is the same as `check_assign_or_update`'s `dst` (`x`, `@p0.x`, ...). */
pub(crate) fn count_writes_to(prog: &Program, dst: &str) -> usize {
    let locals = &get_only_function(prog)
        .expect("expected exactly one function")
        .locals;
    let dst_ap = access_path_from_str(dst, locals, &prog.globals);
    statements_of(prog)
        .filter(|s| writes_dest(&s.kind, &dst_ap))
        .count()
}

/* Asserts the (single) function writes to `dst` exactly `count` times, ignoring the written value.
The source-agnostic complement to `check_assign_or_update`: use it when the value written is an
incidental flatten temp that shouldn't be pinned -- e.g. `x++` lowers to `x = <t0>` and `p->x++` to
an update of `@p0.x`, where the structural fact under test is *that* the write happens, not what
feeds it. Panics at the caller's line on mismatch. */
#[track_caller]
pub(crate) fn check_writes_to(prog: &Program, dst: &str, count: usize) {
    let got = count_writes_to(prog, dst);
    assert_eq!(
        got, count,
        "expected {count} write(s) to {dst:?}, found {got}\n{prog}"
    );
}

/* Every field name the program's assignments read or write, across ALL functions, and every
global's name, which the index stores as a string beside the paths (a vertex of its own, written
`$name` in the path grammar).

The invariant it exists for is that a field name is non-empty. `facts::Path` serializes an access path as its segments joined by `.` and its parser
rejects an empty segment, so ONE empty field name anywhere in an import makes the whole index
unreadable: the next reader panics on the parquet `ctadl index` just wrote.
Whole-program rather than one function because only tree-sitter's parse recovery can produce an
empty name, and an input that provokes the recovery is never a tidy one-function program. */
pub(crate) fn field_symbols(prog: &Program) -> Vec<&str> {
    let globals = prog.globals.iter_enumerated().map(|(_, g)| &*g.name);
    globals
        .chain(
            prog.functions
                .functions
                .raw
                .iter()
                .flat_map(|f| f.blocks.iter())
                .flat_map(|b| b.statements.iter())
                .flat_map(|s| match &s.kind {
                    StatementKind::Assign { dest, sources } => std::iter::once(dest)
                        .chain(sources.iter().filter_map(Exp::access_path))
                        .flat_map(|ap| ap.segments.iter())
                        .filter_map(|seg| match seg {
                            PathSegment::Field(name) => Some(&**name),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                }),
        )
        .collect()
}

pub(crate) fn check_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        return true;
    }
    check_fail_str(prog_str, &format!("expected {}", needle));
    false
}

/* Inverse of `check_match`: returns `true` when `needle` is ABSENT from the program dump string,
and logs a failure via `check_fail_str` when it is unexpectedly present.

**Footgun warning:** this returns `bool`, not a panic. A bare call in a test body silently no-ops
the "must be absent" assertion if the return value is dropped. Always wrap in `assert!`:

    assert!(check_no_match(&prog_str, "ERR 78"), "…");

Prefer IR-level negatives (`check_no_flow`, `check_does_not_return_param`) over dump-string checks
where possible -- those assert on the real dataflow, not the pretty-printer output. Reserve this
for cases where the IR query is not expressive enough, e.g. asserting that a particular temp name
or keyword never appears in the lowering (useful for debugging unexpected code-generation artifacts). */
pub(crate) fn check_no_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        check_fail_str(prog_str, &format!("did not expect {}", needle));
        return false;
    }
    true
}

pub(crate) fn get_summary(
    program: Program,
) -> Result<(Vec<FunctionSummary>, IndexSourceInfo), Error> {
    let mut program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    program_info.program.verify()?;
    let mut facts = IndexFacts::default();
    ssa::transform_program(&mut program_info.program, true);
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        Default::default(),
        &Default::default(),
    );
    let result = taint_index(facts);
    Ok((result.summary, source_info))
}

/* Searches a summary slice for a specific flow edge, scanning records across ALL functions in the
summary (matches if *any* function has the edge). Several tests rely on this: they build a
multi-function fixture and disambiguate by choosing a (param-index, path) endpoint only one
function could produce (e.g. routing taint through param 1 so a `return <- @p1` edge can only come
from the wrapper). When you instead need to pin the assertion to a *named* function -- because more
than one could match, or to guard against matching the wrong one -- use the per-function variants
`flow_present_in` / `check_flow_in` / `check_returns_param_in` etc. */
pub(crate) fn summary_search(
    summary: &[FunctionSummary],
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) -> bool {
    // Fixtures are written in the canonical path grammar: every segment carries its
    // leading dot (`.f2.f3`), and `""` is the empty path.
    let parse = |s: &str| {
        Path::parse(s).unwrap_or_else(|e| panic!("test access path {s:?} does not parse: {e}"))
    };
    let from_path = parse(from_path);
    let to_path = parse(to_path);
    summary.iter().any(|r| {
        r.1 == fx::FormalIndex::new(to_index).into()
            && r.2 == to_path
            && r.3 == fx::FormalIndex::new(from_index).into()
            && r.4 == from_path
    })
}

// Renders a summary endpoint for failure messages, e.g. `@p0.f1` or `return`.
fn fmt_endpoint(index: i16, path: &str) -> String {
    let base = if index == RETURN_INDEX {
        "return".to_string()
    } else {
        format!("@p{index}")
    };
    // `path` is already in the canonical grammar, leading dot and all.
    format!("{base}{path}")
}

/* Asserting wrappers around the summary predicates above. Unlike a bare `assert!(summary_*(...))`,
these are `#[track_caller]` and print the actual summary on failure, so a Category B test shows what
flows *were* present rather than just "assertion failed". The `bool` predicates are kept for
composition — `check_no_flow` is the asserting form for the absence case (Flowy's `</-`). */

#[track_caller]
pub(crate) fn check_summary_count(summary: &[FunctionSummary], count: usize) {
    assert_eq!(
        summary.len(),
        count,
        "summary count mismatch\nsummary: {summary:#?}"
    );
}

#[track_caller]
pub(crate) fn check_flow(
    summary: &[FunctionSummary],
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) {
    assert!(
        summary_search(summary, from_index, from_path, to_index, to_path),
        "expected flow {} -> {}, but it is absent.\nsummary: {summary:#?}",
        fmt_endpoint(from_index, from_path),
        fmt_endpoint(to_index, to_path),
    );
}

#[track_caller]
pub(crate) fn check_no_flow(
    summary: &[FunctionSummary],
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) {
    assert!(
        !summary_search(summary, from_index, from_path, to_index, to_path),
        "unexpected flow {} -> {} is present.\nsummary: {summary:#?}",
        fmt_endpoint(from_index, from_path),
        fmt_endpoint(to_index, to_path),
    );
}

#[track_caller]
pub(crate) fn check_returns_param(summary: &[FunctionSummary], param_num: i16, param_path: &str) {
    check_flow(summary, param_num, param_path, RETURN_INDEX, "");
}

/* Asserts the given param reaches the return's value only (`return.#value`), as an operand of
arithmetic or a comparison does, and not the return itself. */
#[track_caller]
pub(crate) fn check_returns_param_value(
    summary: &[FunctionSummary],
    param_num: i16,
    param_path: &str,
) {
    check_flow(summary, param_num, param_path, RETURN_INDEX, ".#value");
    check_no_flow(summary, param_num, param_path, RETURN_INDEX, "");
}

/* Asserts the given param does NOT reach the return -- the negative complement of
`check_returns_param` (Flowy's `</-` for a return endpoint). Use it to pin that a value is *not*
returned, e.g. a block-scoped shadow that must not escape. Prints the actual summary on failure. */
#[track_caller]
pub(crate) fn check_does_not_return_param(
    summary: &[FunctionSummary],
    param_num: i16,
    param_path: &str,
) {
    check_no_flow(summary, param_num, param_path, RETURN_INDEX, "");
}

/* Per-function variant of `summary_search`: only matches summary records belonging to function
`name`. The plain `summary_search` scans every function's summary, so on a multi-function fixture it
can match a flow in the *wrong* function (e.g. assert "caller returns its param" but match the
callee). This resolves `name` to its `FunctionId` via the source_info `sites` map and filters on it.
Panics if `name` is not a known function -- a not-found function must fail loudly, never silently
pass a `check_no_flow_in`. */
#[track_caller]
fn flow_present_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) -> bool {
    flow_present_in_roots(
        summary,
        source_info,
        name,
        (from_index.into(), from_path.parse().unwrap()),
        (to_index.into(), to_path.parse().unwrap()),
    )
}

/* `flow_present_in` between two roots, formal or global. */
#[track_caller]
fn flow_present_in_roots(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    from: (fx::Root, Path),
    to: (fx::Root, Path),
) -> bool {
    let func_id = source_info
        .sites
        .get_function_id(fx::Function(fx::Str::from(name)))
        .unwrap_or_else(|| panic!("no function named {name:?} in index summary"));
    summary
        .iter()
        .any(|r| r.0 == func_id && (r.1, r.2) == to && (r.3, r.4) == from)
}

/* The global and path a test names as `.g.p`: the first segment is the global's name, the rest
its path, so `.g.deref` is `$g.deref` (escaped like a field: `.count$one\.c.deref`). */
#[track_caller]
fn global_path(field: &str) -> (fx::Root, Path) {
    let p: Path = field.parse().unwrap();
    let Some(ctadl_ir::cir::PathSegment::Field(name)) = p.head() else {
        panic!("a global path {field:?} starts with the global's name");
    };
    (
        fx::Root::global(fx::GlobalId(fx::Str::from(name.clone()))),
        p.tail(),
    )
}

/* Asserting per-function flow (presence), the multi-function analogue of `check_flow`. Resolves
`name` to its function and asserts the flow is present *in that function's* summary. Prints the full
summary on failure. */
#[track_caller]
pub(crate) fn check_flow_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) {
    assert!(
        flow_present_in(
            summary,
            source_info,
            name,
            from_index,
            from_path,
            to_index,
            to_path
        ),
        "expected flow {} -> {} in {name:?}, but it is absent.\nsummary: {summary:#?}",
        fmt_endpoint(from_index, from_path),
        fmt_endpoint(to_index, to_path),
    );
}

/* Asserting per-function flow (absence), the multi-function analogue of `check_no_flow`. */
#[track_caller]
pub(crate) fn check_no_flow_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) {
    assert!(
        !flow_present_in(
            summary,
            source_info,
            name,
            from_index,
            from_path,
            to_index,
            to_path
        ),
        "unexpected flow {} -> {} in {name:?} is present.\nsummary: {summary:#?}",
        fmt_endpoint(from_index, from_path),
        fmt_endpoint(to_index, to_path),
    );
}

/* `check_returns_param` for a named function in a multi-function fixture: asserts param `param_num`
(path `param_path`) reaches the return *of that function*. */
#[track_caller]
pub(crate) fn check_returns_param_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    param_num: i16,
    param_path: &str,
) {
    check_flow_in(
        summary,
        source_info,
        name,
        param_num,
        param_path,
        RETURN_INDEX,
        "",
    );
}

/* `check_returns_param_value` for a named function in a multi-function fixture. */
#[track_caller]
pub(crate) fn check_returns_param_value_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    param_num: i16,
    param_path: &str,
) {
    let (from, to) = (param_num, RETURN_INDEX);
    check_flow_in(summary, source_info, name, from, param_path, to, ".#value");
    check_no_flow_in(summary, source_info, name, from, param_path, to, "");
}

/* `check_does_not_return_param` for a named function in a multi-function fixture: asserts param
`param_num` (path `param_path`) does NOT reach the return *of that function*. The per-function
return-endpoint negative -- use it for non-interference-through-a-call cases. */
#[track_caller]
pub(crate) fn check_does_not_return_param_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    param_num: i16,
    param_path: &str,
) {
    check_no_flow_in(
        summary,
        source_info,
        name,
        param_num,
        param_path,
        RETURN_INDEX,
        "",
    );
}

/* Asserts function `name` writes parameter `param_num` into the global path `field` -- the summary
flow `@pN -> $g.deref` for `field` `.g.deref` (e.g. `void set(int src){ g = src; }` summarizes
`@p0 -> $g.deref`), a row rooted at the global. Pairs
with `check_returns_global_in` to pin a cross-function flow-through-a-global as two summary
halves. */
#[track_caller]
pub(crate) fn check_param_into_global_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    param_num: i16,
    field: &str,
) {
    assert!(
        flow_present_in_roots(
            summary,
            source_info,
            name,
            (param_num.into(), Path::empty()),
            global_path(field),
        ),
        "expected flow @p{param_num} -> ${} in {name:?}, but it is absent.\nsummary: {summary:#?}",
        &field[1..],
    );
}

/* Asserts function `name` returns the global path `field` -- the summary flow `$g.deref -> return`
for `field` `.g.deref` (e.g. `int get(){ return g; }`). The other half of a
flow-through-a-global; see `check_param_into_global_in`. */
#[track_caller]
pub(crate) fn check_returns_global_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    field: &str,
) {
    assert!(
        flow_present_in_roots(
            summary,
            source_info,
            name,
            global_path(field),
            (RETURN_INDEX.into(), Path::empty()),
        ),
        "expected flow ${} -> return in {name:?}, but it is absent.\nsummary: {summary:#?}",
        &field[1..],
    );
}

/* Asserts function `name` does NOT write parameter `param_num` into the global path `field`. */
#[track_caller]
pub(crate) fn check_no_param_into_global_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    param_num: i16,
    field: &str,
) {
    assert!(
        !flow_present_in_roots(
            summary,
            source_info,
            name,
            (param_num.into(), Path::empty()),
            global_path(field),
        ),
        "unexpected flow @p{param_num} -> ${} in {name:?}.\nsummary: {summary:#?}",
        &field[1..],
    );
}

/* Asserts function `name` returns, at `ret_path`, the global path `field`: `check_returns_global_in`
for a return path other than the whole value (`.#value` for arithmetic on the global). */
#[track_caller]
pub(crate) fn check_returns_global_at_in(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    name: &str,
    field: &str,
    ret_path: &str,
) {
    assert!(
        flow_present_in_roots(
            summary,
            source_info,
            name,
            global_path(field),
            (RETURN_INDEX.into(), ret_path.parse().unwrap()),
        ),
        "expected flow ${} -> return{ret_path} in {name:?}, but it is absent.\nsummary: {summary:#?}",
        &field[1..],
    );
}

// Unit tests for the access-path string DSL itself. These helpers contain real parsing logic, so a
// bug here would silently weaken every test that relies on them.
#[cfg(test)]
mod ap_tests {
    use super::*;

    #[test]
    fn local_pathless() {
        let mut locals = Locals::default();
        let b = locals.get_or_intern("b");
        let ap = access_path_from_str("b", &locals, &Globals::default());
        assert_eq!(ap.base, VariableRef::new_local_idx(b));
        assert!(ap.is_pathless());
    }

    #[test]
    fn param_with_field() {
        assert_eq!(
            access_path_from_str("@p1.f2", &Locals::default(), &Globals::default()),
            AccessPath::new(
                VariableRef::new_parameter(1u32.into()),
                [PathSegment::field("f2")]
            ),
        );
    }

    #[test]
    fn global_with_field() {
        let mut globals = Globals::default();
        globals.declare("x", None);
        let a = globals.declare("a$util.c", None);
        assert_eq!(
            access_path_from_str(r"$a$util\.c.deref.f", &Locals::default(), &globals),
            AccessPath::new(
                VariableRef::global(a),
                [PathSegment::Deref, PathSegment::field("f")]
            ),
        );
    }

    #[test]
    fn nested_fields() {
        let mut locals = Locals::default();
        let v = locals.get_or_intern("v");
        assert_eq!(
            access_path_from_str("v.f1.f2", &locals, &Globals::default()),
            AccessPath::new(
                VariableRef::new_local_idx(v),
                [PathSegment::field("f1"), PathSegment::field("f2")]
            ),
        );
    }

    #[test]
    #[should_panic(expected = "empty access-path segment")]
    fn parse_path_rejects_empty_segments() {
        // Silently dropping empties would make `.a..b.` and `.a.b` the same fixture.
        parse_path(".a..b.");
    }

    #[test]
    fn subscript_is_symbol_segment() {
        // A bracketed *name* is a symbol, not an offset, and the DSL says so by escaping the
        // leading bracket: `"f.\[3]"` is `Symbol("[3]")`. (The C frontend lowers `f[3]` to
        // `Offset(3), Symbol("deref")` -- see `subscript_access_paths` -- but lua still emits a
        // bracketed symbol, and either way this spelling has to stay writable.)
        let mut locals = Locals::default();
        let f = locals.get_or_intern("f");
        assert_eq!(
            access_path_from_str(r"f.\[3]", &locals, &Globals::default()),
            AccessPath::new(VariableRef::new_local_idx(f), [PathSegment::field("[3]")]),
        );
    }

    #[test]
    fn unescaped_subscript_is_an_offset() {
        // ... and the unescaped spelling now means a real offset, as it does everywhere else.
        // Before, both spellings produced `Symbol("[3]")` and the distinction was unwritable.
        let mut locals = Locals::default();
        let f = locals.get_or_intern("f");
        assert_eq!(
            access_path_from_str("f.[3]", &locals, &Globals::default()),
            AccessPath::new(VariableRef::new_local_idx(f), [PathSegment::offset(3)]),
        );
    }

    #[test]
    fn constant_source() {
        let mut locals = Locals::default();
        locals.get_or_intern("b");
        // A `#`-prefixed source is a constant literal, not a variable.
        assert_eq!(
            exp_from_str("#7", &locals, &Globals::default()),
            Exp::new_str("7")
        );
        // ...while a bare name is still an access-path variable.
        assert_eq!(
            exp_from_str("b", &locals, &Globals::default()),
            Exp::variable(access_path_from_str("b", &locals, &Globals::default()).base)
        );
    }
}
