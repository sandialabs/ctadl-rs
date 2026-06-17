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
// `DirectedGraph` is a trait import: it provides `num_nodes()` used by `check_block_count`.
// It looks unused but removing it breaks method resolution.
use ctadl_ir::graph::DirectedGraph;
use ctadl_ir::{AccessPath, Exp, FunctionData, StatementKind, VariableRef, ssa};
use ctadl_ir::{Program, ProgramInfo, ParameterType, FieldAccess};
use crate::facts::Path;

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

/* A test to check a function has a certain number and type of params. */
pub(crate) fn check_params(prog: &Program, params: &[ParameterType]) -> bool {
    let Some(fun) = get_only_function(prog) else {
        return false;
    };

    if params.len() != fun.params.parameters.len() {
        let err = format!(
            "Wrong number of params, expected {} got {}",
            params.len(),
            fun.params.parameters.len()
        );
        check_fail(prog, &err);
        return false;
    }

    for (i, (actual, expected)) in params
        .iter()
        .zip(fun.params.parameters.iter())
        .enumerate()
    {
        if expected != actual {
            let err = format!(
                "Wrong param type at index {}, expected {:?} got {:?}",
                i,
                expected,
                actual
            );
            check_fail(prog, &err);
            return false;
        }
    }

    true
}

/* Checks that count number of blocks exist in the program.
  Implicitly checks for the existance of only 1 functions.
*/
pub(crate) fn check_block_count(prog: &Program, count: usize) -> bool {
    let Some(fun) = get_only_function(prog) else {
        return false;
    };
    let len = fun.blocks.num_nodes();

    if len != count {
        let err = format!("{} blocks in parsed function, expected {}.", len, count);
        check_fail(prog, &err);
        return false;
    }
    true
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

// TODO: offset paths. This only produces `FieldAccess::Symbol`, so subscript/offset segments like
// `[3]` are not parsed into `FieldAccess::Offset` and access paths with numeric offsets won't
// round-trip through this helper yet.
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

        return AccessPath::new(
            VariableRef::new_parameter(n.into()),
            parse_fields(suffix),
        );
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

/* This function is intended to run on Program objects generated by
parsing a single function and will search all blocks for a
particular assignment or update if the dest requires an update. */
pub(crate) fn check_assign_or_update<I>(
    prog: &Program,
    dst: &str,
    src_strs: I,
    block_id: Option<usize>,
) -> bool
where
    I: IntoIterator<Item = &'static str>,
{
    let srcs: Vec<Exp> = src_strs
        .into_iter()
        .map(|s| Exp::from(access_path_from_str(s)))
        .collect();

    let dst_ap = access_path_from_str(dst);

    let expected = if dst_ap.path.is_empty() {
        StatementKind::assign(dst_ap.variable_ref, srcs)
    } else {
        if srcs.len() != 1 {
            panic!("Update destination requires exactly one source expression");
        }
        StatementKind::update(dst_ap, srcs.into_iter().next().unwrap())
    };

    if !check_function_count(prog, 1) {
        return false;
    }
    let Some(fun) = prog.functions.functions.raw.first() else {
        return false;
    };

    for (i, block) in fun.blocks.iter().enumerate() {
        if let Some(req_block) = block_id
            && i != req_block
        {
            continue;
        }

        for stmt in block.statements.iter() {
            if stmt.kind == expected {
                return true;
            }
        }
    }

    let err = format!(
        "Could not find '{}' in function {}.",
        expected,
        fun.name
    );
    check_fail(prog, &err);
    false
}

pub(crate) fn check_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        return true;
    }
    check_fail_str(prog_str, &format!("expected {}", needle));
    false
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
        r.1 == fx::FormalIndex::new(to_index) &&
        r.2 == to_path &&
        r.3 == fx::FormalIndex::new(from_index) &&
        r.4 == from_path
    })
}

pub(crate) fn summary_returns_param(
    summary: &[FunctionSummary],
    param_num: i16,
    param_path: &str
) -> bool {
    summary_search(summary, param_num, param_path, RETURN_INDEX, "")
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
}