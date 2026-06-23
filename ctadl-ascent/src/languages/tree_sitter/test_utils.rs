// `test_utils` is included as an unconditional `mod` (not `#[cfg(test)]`), so in non-test builds
// these helpers have no callers and trip `dead_code` under `-D warnings`. Allowing it here keeps
// `cargo clippy -- -D warnings` green. Cleaner fix when someone can touch mod.rs: make the
// declaration `#[cfg(test)] mod test_utils;` and drop this allow.
#![allow(dead_code)]

use crate::error::Error;
use crate::facts as fx;

use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{FunctionSummary, IndexFacts, taint_index};
use crate::{
    codegen::{CallResolutionStrategy, RETURN_INDEX, codegen_program},
    languages::tree_sitter,
};
use anyhow::{Context, Result};
// `DirectedGraph`/`Successors` are trait imports: they provide `num_nodes()` (used by
// `check_block_count`) and `successors()` (used by `check_successors`). They look unused but
// removing them breaks method resolution.
use crate::facts::Path;
use ctadl_ir::graph::{DirectedGraph, Successors};
use ctadl_ir::mir::TerminatorKind;
use ctadl_ir::mir::call::{CallEdges, CallStyle};
use ctadl_ir::{
    AccessPath, BasicBlockIdx, Exp, FunctionData, Idx, StatementKind, VariableRef, ssa,
};
use ctadl_ir::{FieldAccess, ParameterType, Program, ProgramInfo};

pub(crate) fn init_test_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug) // This forces it to Debug
        .is_test(true)
        .try_init();
}

pub(crate) fn get_full_path(filename: &str) -> Result<std::path::PathBuf> {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Now just append the folders from the crate root
    path.push("tests");
    path.push("c");
    path.push(filename);
    Ok(path)
}

/* Compile a program from a string. */
pub(crate) fn program_from_string(src: &str) -> (Program, String) {
    let result = tree_sitter::parse_c_program(src).expect("Failed to parse C program.");
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

/* Compile a program from a file. */
pub(crate) fn program_from_file<P: AsRef<std::path::Path>>(filename: P) -> Result<Program> {
    let path = filename.as_ref();

    // Read the file, and if it fails, attach a helpful message before returning
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to load source file: {}", path.display()))?;
    let program = tree_sitter::parse_c_program(&contents)?;
    Ok(program.0)
}

/* Common output for when tests fail. */
pub(crate) fn check_fail_str(prog_str: &str, msg: &str) {
    log::error!("TEST FAIL: {msg}");
    log::error!("\t{prog_str}");
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
argument expressions (access paths / constants). Returns an empty Vec if the function has no direct
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
    let want_args: Vec<Exp> = args.into_iter().map(exp_from_str).collect();
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
pub(crate) fn debug_output_blocks(prog: &Program) {
    let Some(fun) = prog.functions.functions.raw.first() else {
        log::warn!("No functions in program");
        return;
    };
    for (idx, block) in fun.blocks.iter().enumerate() {
        log::info!("BLOCK {}: {}", idx, block);
    }
}

// Every segment becomes a `FieldAccess::Symbol`, including subscripts like `[3]`. This matches the
// C frontend, which currently lowers a subscript to `FieldAccess::Symbol("[N]")` (see
// `flatten_subscript` in mod.rs) rather than a real `FieldAccess::Offset`. So `"f.[3]"` round-trips
// to `[Symbol("f"), Symbol("[3]")]`, exactly what the parser emits. (Real `Offset` lowering is an
// aspirational frontend change; revisit this helper if/when that lands.)
fn parse_fields(s: &str) -> impl Iterator<Item = FieldAccess> + '_ {
    s.split('.')
        .filter(|part| !part.is_empty())
        .map(|f| FieldAccess::Symbol(f.into()))
}

fn access_path_from_str(s: &str) -> AccessPath {
    if let Some(rest) = s.strip_prefix("$globals") {
        return AccessPath::new(
            VariableRef::new_global(),
            parse_fields(rest.strip_prefix('.').unwrap_or("")),
        );
    }

    if let Some(rest) = s.strip_prefix("@p") {
        let (n_str, suffix) = if let Some((n, tail)) = rest.split_once('.') {
            (n, tail)
        } else {
            (rest, "")
        };

        let n: u32 = n_str.parse().expect("invalid parameter index in @pN");

        return AccessPath::new(VariableRef::new_parameter(n.into()), parse_fields(suffix));
    }

    let (base, suffix) = if let Some((base, tail)) = s.split_once('.') {
        (base, tail)
    } else {
        (s, "")
    };

    AccessPath::new(
        VariableRef::new_local(base.to_string()),
        parse_fields(suffix),
    )
}

/* Builds a source expression from the DSL. A `#`-prefixed string is a constant literal (`"#7"` =>
`Exp::Str("7")`, matching how the C frontend lowers a literal — see `flatten_expr` in mod.rs);
anything else is an access path (variable / param / global / field). */
fn exp_from_str(s: &str) -> Exp {
    match s.strip_prefix('#') {
        Some(lit) => Exp::new_str(lit),
        None => Exp::from(access_path_from_str(s)),
    }
}

/* Asserts the (single) function contains the given assignment or update (an update is expected when
`dst` has a field path). If `block_id` is given, only that block is searched. Sources are DSL strings
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
    let srcs: Vec<Exp> = src_strs.into_iter().map(exp_from_str).collect();

    let dst_ap = access_path_from_str(dst);

    let expected = if dst_ap.path.is_empty() {
        StatementKind::assign(dst_ap.variable_ref, srcs)
    } else {
        assert_eq!(
            srcs.len(),
            1,
            "update destination requires exactly one source expression"
        );
        StatementKind::update(dst_ap, srcs.into_iter().next().unwrap())
    };

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

pub(crate) fn check_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        return true;
    }
    check_fail_str(prog_str, &format!("expected {}", needle));
    false
}

/// Inverse of [`check_match`]: passes (returns true) when `needle` is ABSENT, and
/// only logs a failure when it is unexpectedly present. Use this for negative
/// assertions so a passing test doesn't emit a misleading "expected ..." line.
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
    );
    let result = taint_index(facts);
    Ok((result.summary, source_info))
}

pub(crate) fn summary_count(summary: &[FunctionSummary], count: usize) -> bool {
    summary.len() == count
}

pub(crate) fn summary_search(
    summary: &[FunctionSummary],
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) -> bool {
    let from_path: Path = from_path.parse().unwrap();
    let to_path: Path = to_path.parse().unwrap();
    summary.iter().any(|r| {
        r.1 == fx::FormalIndex::new(to_index)
            && r.2 == to_path
            && r.3 == fx::FormalIndex::new(from_index)
            && r.4 == from_path
    })
}

pub(crate) fn summary_returns_param(
    summary: &[FunctionSummary],
    param_num: i16,
    param_path: &str,
) -> bool {
    summary_search(summary, param_num, param_path, RETURN_INDEX, "")
}

// Renders a summary endpoint for failure messages, e.g. `@p0.f1` or `return`.
fn fmt_endpoint(index: i16, path: &str) -> String {
    let base = if index == RETURN_INDEX {
        "return".to_string()
    } else {
        format!("@p{index}")
    };
    if path.is_empty() {
        base
    } else {
        format!("{base}.{path}")
    }
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

// Unit tests for the access-path string DSL itself. These helpers contain real parsing logic, so a
// bug here would silently weaken every test that relies on them.
#[cfg(test)]
mod ap_tests {
    use super::*;

    #[test]
    fn local_no_fields() {
        let ap = access_path_from_str("b");
        assert_eq!(ap.variable_ref, VariableRef::new_local("b".to_string()));
        assert!(ap.path.is_empty());
    }

    #[test]
    fn param_with_field() {
        assert_eq!(
            access_path_from_str("@p1.f2"),
            AccessPath::new(
                VariableRef::new_parameter(1u32.into()),
                [FieldAccess::Symbol("f2".into())],
            ),
        );
    }

    #[test]
    fn global_with_field() {
        assert_eq!(
            access_path_from_str("$globals.a"),
            AccessPath::new(VariableRef::new_global(), [FieldAccess::Symbol("a".into())]),
        );
    }

    #[test]
    fn nested_fields() {
        assert_eq!(
            access_path_from_str("v.f1.f2"),
            AccessPath::new(
                VariableRef::new_local("v".to_string()),
                [
                    FieldAccess::Symbol("f1".into()),
                    FieldAccess::Symbol("f2".into()),
                ],
            ),
        );
    }

    #[test]
    fn parse_fields_skips_empty_segments() {
        // Leading/trailing/double dots must not produce empty field segments.
        let fields: Vec<FieldAccess> = parse_fields(".a..b.").collect();
        assert_eq!(
            fields,
            vec![
                FieldAccess::Symbol("a".into()),
                FieldAccess::Symbol("b".into()),
            ],
        );
    }

    #[test]
    fn subscript_is_symbol_segment() {
        // The C frontend lowers `f[3]` to `FieldAccess::Symbol("[3]")` (not a real Offset), so the
        // DSL must too — `"f.[3]"` becomes `[Symbol("f"), Symbol("[3]")]`.
        assert_eq!(
            access_path_from_str("f.[3]"),
            AccessPath::new(
                VariableRef::new_local("f".to_string()),
                [FieldAccess::Symbol("[3]".into())],
            ),
        );
    }

    #[test]
    fn constant_source() {
        // A `#`-prefixed source is a constant literal, not a variable.
        assert_eq!(exp_from_str("#7"), Exp::new_str("7"));
        // ...while a bare name is still an access-path variable.
        assert_eq!(exp_from_str("b"), Exp::from(access_path_from_str("b")));
    }
}
