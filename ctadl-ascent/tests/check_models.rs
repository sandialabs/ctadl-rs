/*! `ctadl check-models` against a synthetic program, with no store anywhere.

Everything here drives [`cli::check_programs`], which is the half of the command that needs no
store: it takes owned `(ImportScope, ProgramInfo)` items and does the rest. `tests/cli.rs` covers
the store-facing half.
*/
use std::io::Write as _;

use ctadl_ascent::cli::{self, CheckOptions, GeneratorKind};
use ctadl_ascent::models::ImportScope;
use ctadl_ir::mir::ProgramInfo;
use tempfile::NamedTempFile;

/// A native program with one no-parameter function per name.
fn program(names: &[&str]) -> ProgramInfo {
    use ctadl_ir::mir::call::{
        NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
    };
    use ctadl_ir::mir::{
        BasicBlockData, FunctionData, Functions, ParameterType, Program, Statement, StatementKind,
    };

    let functions: Vec<FunctionData> = names
        .iter()
        .map(|name| {
            let mut f = FunctionData::default();
            f.set_name((*name).to_string());
            f.params.parameters.push(ParameterType::ByVal);
            f.params.parameters.push(ParameterType::ByVal);
            let blocks = f.blocks.blocks_mut();
            let body = blocks.push(BasicBlockData::new(None));
            blocks[body].extend(vec![Statement::new_kind(StatementKind::Nop)]);
            f
        })
        .collect();

    ProgramInfo {
        vmt: VirtualMethodTable::Native {
            methods: names
                .iter()
                .map(|name| {
                    (
                        NativeSimpleName((*name).into()),
                        NativeSignature((*name).into()),
                        NativeFunction((*name).into()),
                        NativeQualifiedName((*name).into()),
                    )
                })
                .collect(),
        },
        program: Program::new(Functions::new(functions)),
        ..Default::default()
    }
}

/// Writes `contents` to a temp file with `suffix`, keeping the handle alive for the caller.
fn model_file(suffix: &str, contents: &str) -> NamedTempFile {
    let mut file = NamedTempFile::with_suffix(suffix).unwrap();
    write!(file, "{contents}").unwrap();
    file.flush().unwrap();
    file
}

/// The import identity a scoped generator can name.
fn scope() -> ImportScope {
    ImportScope::new(ctadl_ascent::project::ArtifactLanguage::Pcode, "app")
}

fn check(
    names: &[&str],
    files: &[&NamedTempFile],
    show_matches: Option<usize>,
) -> cli::CheckReport {
    let paths: Vec<std::path::PathBuf> = files.iter().map(|f| f.path().to_path_buf()).collect();
    cli::check_programs(
        std::iter::once(Ok((scope(), program(names)))),
        CheckOptions {
            models: &paths,
            default_models: false,
            show_matches,
        },
    )
    .expect("checking models")
}

/// The three outcomes a generator can have, and the point of the whole command: they must be
/// distinguishable. "Matched nothing" and "was never attempted" look identical from the
/// matching pass alone, because a scoped-out generator leaves no stats entry at all.
#[test]
fn matched_unmatched_and_scoped_out_are_distinct() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}},
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "nosuchfunction"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}},
            {"find": "methods",
             "in": {"language": "lua"},
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}}
        ]}"#,
    );
    let report = check(&["sink"], &[&file], None);
    let generators = &report.files[0].generators;
    assert_eq!(generators.len(), 3);

    // [0] matched.
    assert_eq!(generators[0].functions_total, Some(1));
    assert_eq!(generators[0].applicable_imports, vec!["app".to_string()]);
    assert!(!generators[0].is_dead());

    // [1] matched nothing, and says why.
    assert_eq!(generators[1].functions_total, Some(0));
    assert!(generators[1].is_dead());
    let GeneratorKind::Endpoint { stats, .. } = &generators[1].kinds[0] else {
        panic!(
            "expected an endpoint kind, got {:?}",
            generators[1].kinds[0]
        );
    };
    assert_eq!(stats.endpoints_matched, 0);
    assert_eq!(
        stats.unmatched,
        vec!["no function matched `where`".to_string()]
    );

    // [2] was never attempted -- and must NOT be reported as having matched nothing.
    assert!(generators[2].applicable_imports.is_empty());
    assert!(
        !generators[2].is_dead(),
        "a scoped-out generator is not a dead one"
    );
    assert!(generators[2].scope.is_some());

    assert_eq!(report.dead_generators().len(), 1);
}

/// A shape error in one file must not suppress the other file's report -- nor the rest of its
/// own file's, including generators in *later* batches. The loader processes 1024 generators at
/// a time, and the abort-on-error entry point never reaches the batch after a bad one.
#[test]
fn an_error_in_one_file_does_not_suppress_the_others() {
    // > 1024 generators, so the last one is in the third batch, well past the error in the
    // first. The typo (`wehre`) is a hard error the matcher collects rather than a parse
    // failure, so everything after it was still visited.
    let mut generators = vec![
        r#"{"find": "methods", "wehre": [], "model": {"sinks": [{"kind": "k", "port": "Return"}]}}"#
            .to_string(),
    ];
    for _ in 0..2100 {
        generators.push(
            r#"{"find": "methods", "where": [{"constraint": "signature_match", "name": "sink"}],
                "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}}"#
                .to_string(),
        );
    }
    let bad = model_file(
        ".json",
        &format!(r#"{{"model_generators": [{}]}}"#, generators.join(",")),
    );
    let good = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sources": [{"kind": "k", "port": "Return"}]}}
        ]}"#,
    );

    let report = check(&["sink"], &[&bad, &good], None);

    let bad_report = &report.files[0];
    assert_eq!(bad_report.errors.len(), 1, "{:?}", bad_report.errors);
    assert!(bad_report.errors[0].contains("wehre"));
    assert_eq!(bad_report.generators.len(), 2101);
    // The generator after the bad one, in the same batch.
    assert_eq!(bad_report.generators[1].functions_total, Some(1));
    // ... and the last one, three batches later: the stats the abort-on-error loader discards.
    assert_eq!(bad_report.generators[2100].functions_total, Some(1));

    // The second file is untouched.
    let good_report = &report.files[1];
    assert!(good_report.errors.is_empty());
    assert_eq!(good_report.generators[0].functions_total, Some(1));
}

/// A bridge whose `to` side matches nothing gets `diagnose`'s verdict, and no pair count:
/// pairing needs the fact base.
#[test]
fn an_unmatched_bridge_side_is_diagnosed() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "caller"}],
             "model": {"bridge": {"to": {
                 "where": [{"constraint": "signature_match", "name": "nosuchimpl"}]}}}}
        ]}"#,
    );
    let report = check(&["caller"], &[&file], None);
    let GeneratorKind::Bridge {
        from_matched,
        to_matched,
        diagnosis,
    } = &report.files[0].generators[0].kinds[0]
    else {
        panic!("expected a bridge kind");
    };
    assert_eq!(*from_matched, 1);
    assert_eq!(*to_matched, 0);
    let diagnosis = diagnosis.as_ref().expect("a verdict");
    assert!(
        diagnosis.contains("'to' side matched none"),
        "got: {diagnosis}"
    );
    assert!(
        !diagnosis.contains("pair(s) bridged"),
        "a pair count needs the index: {diagnosis}"
    );
}

/// `--show-matches` lists names up to its cap, and the exact total stays exact.
#[test]
fn show_matches_caps_the_names_but_not_the_count() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature", "pattern": "^f"}],
             "model": {"sources": [{"kind": "k", "port": "Return"}]}}
        ]}"#,
    );
    let names = ["f1", "f2", "f3", "f4"];

    let counts_only = check(&names, &[&file], None);
    assert_eq!(counts_only.files[0].generators[0].functions_total, Some(4));
    assert!(counts_only.files[0].generators[0].functions.is_empty());

    let capped = check(&names, &[&file], Some(2));
    assert_eq!(capped.files[0].generators[0].functions.len(), 2);
    assert_eq!(capped.files[0].generators[0].functions_total, Some(4));

    let all = check(&names, &[&file], Some(0));
    assert_eq!(all.files[0].generators[0].functions.len(), 4);
}

/// With no program at all the command is a file lint: it reports the inventory and says so,
/// and calls nothing dead.
#[test]
fn with_no_program_nothing_is_called_dead() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "nosuchfunction"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}}
        ]}"#,
    );
    let report = cli::check_programs(
        std::iter::empty(),
        CheckOptions {
            models: &[file.path().to_path_buf()],
            default_models: false,
            show_matches: None,
        },
    )
    .expect("checking models");

    assert!(report.imports.is_empty());
    assert_eq!(report.files[0].generators.len(), 1);
    assert_eq!(
        report.files[0].generators[0].find.as_deref(),
        Some("methods")
    );
    // Absent, not zero: nothing was matched, so there is no count to report.
    assert_eq!(report.files[0].generators[0].functions_total, None);
    assert!(report.dead_generators().is_empty());
}

/// A `find: callsites` generator carries the caveat that its count is of callees, not of call
/// sites -- the fan-out is Stage 2.
#[test]
fn callsites_carry_their_caveat() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "callsites",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(*)"}]}}
        ]}"#,
    );
    let report = check(&["sink"], &[&file], None);
    let caveats = &report.files[0].generators[0].caveats;
    assert!(
        caveats.contains(&cli::Caveat::CallsiteFanout),
        "{caveats:?}"
    );
    assert!(caveats.contains(&cli::Caveat::AnyArgument), "{caveats:?}");
}

/// An import that will not load is recorded against that import, and the loop continues.
#[test]
fn a_failed_import_does_not_end_the_run() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sources": [{"kind": "k", "port": "Return"}]}}
        ]}"#,
    );
    let programs = vec![
        Err(ctadl_ascent::error::Error::Model {
            message: "no such import".to_string(),
        }),
        Ok((scope(), program(&["sink"]))),
    ];
    let report = cli::check_programs(
        programs,
        CheckOptions {
            models: &[file.path().to_path_buf()],
            default_models: false,
            show_matches: None,
        },
    )
    .expect("checking models");

    assert_eq!(report.imports.len(), 2);
    assert!(report.imports[0].error.is_some());
    assert!(report.imports[1].error.is_none());
    // The import that did load was still matched.
    assert_eq!(report.files[0].generators[0].functions_total, Some(1));
}

/// `--format json` is the same data the text renderer reads, so it must round-trip.
#[test]
fn the_report_round_trips_through_serde_json() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}],
                       "propagation": [{"input": "Argument(0)", "output": "Return"}]}}
        ]}"#,
    );
    let report = check(&["sink"], &[&file], Some(0));
    let text = serde_json::to_string(&report).expect("serializing");
    let value: serde_json::Value = serde_json::from_str(&text).expect("re-parsing");

    assert_eq!(value["imports"][0]["name"], "app");
    assert_eq!(value["imports"][0]["language"], "pcode");
    assert_eq!(value["files"][0]["generators"][0]["functions_total"], 1);
    assert_eq!(value["files"][0]["generators"][0]["functions"][0], "sink");
    let kinds = value["files"][0]["generators"][0]["kinds"]
        .as_array()
        .expect("kinds");
    assert_eq!(kinds[0]["kind"], "endpoint");
    assert_eq!(kinds[0]["direction"], "sinks");
    assert_eq!(kinds[1]["kind"], "propagation");
    assert_eq!(kinds[1]["rows"], 1);
}
