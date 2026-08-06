/*! What `ctadl query` reports when there is no index, against a synthetic program and no store.

Everything here drives [`cli::check_programs`], which is the half of the check that needs no
store: it takes owned `(ImportScope, ProgramInfo)` items and does the rest. `tests/cli.rs` covers
the store-facing half -- name resolution, and the promise that a query without an index writes
nothing into the store.
*/
use std::io::Write as _;

use ctadl_ascent::cli;
use ctadl_ascent::facts::TaintDirection;
use ctadl_ascent::models::ImportScope;
use ctadl_ascent::project::AnalysisProject;
use ctadl_ascent::query_engine::formatter::{SarifProfile, format_model_check_sarif};
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

fn check(names: &[&str], files: &[&NamedTempFile]) -> cli::ModelCheckOutcome {
    let paths: Vec<std::path::PathBuf> = files.iter().map(|f| f.path().to_path_buf()).collect();
    cli::check_programs(std::iter::once(Ok((scope(), program(names)))), &paths)
        .expect("checking models")
}

/// The three outcomes a generator can have, and the point of the whole check: they must be
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
    let outcome = check(&["sink"], &[&file]);
    let path = file.path().to_path_buf();

    // [0] matched: reported with its count, and its name, under `CTADL0011`.
    let matched = &outcome.check.matched;
    assert_eq!(matched.len(), 1, "{matched:?}");
    assert_eq!(matched[0].index, 0);
    assert_eq!(matched[0].total, Some(1));
    assert_eq!(matched[0].sample, vec!["sink".to_string()]);

    // [1] matched nothing: the same zero-row counters an indexed query turns into `CTADL0004`.
    let stats = &outcome.endpoint_stats[&(path.clone(), 1, TaintDirection::Backward)];
    assert_eq!(stats.endpoints_matched, 0);
    assert_eq!(stats.ports_declared, 1);

    // [2] was never attempted, and must NOT be reported as having matched nothing.
    let excluded = &outcome.check.scope_excluded;
    assert_eq!(excluded.len(), 1, "{excluded:?}");
    assert_eq!(excluded[0].index, 2);
    assert!(excluded[0].scope.is_some());
    assert!(
        !outcome
            .endpoint_stats
            .contains_key(&(path, 2, TaintDirection::Backward)),
        "a scoped-out generator has no counts at all"
    );
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

    let outcome = check(&["sink"], &[&bad, &good]);

    let errors = &outcome.check.file_errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].file.as_deref(), Some(bad.path()));
    assert!(errors[0].message.contains("wehre"));

    let matched_in = |path: &std::path::Path, index: usize| {
        outcome
            .check
            .matched
            .iter()
            .find(|m| m.file == path && m.index == index)
            .unwrap_or_else(|| panic!("no match reported for generator {index}"))
    };
    // The generator after the bad one, in the same batch...
    assert_eq!(matched_in(bad.path(), 1).total, Some(1));
    // ... and the last one, three batches later: the stats the abort-on-error loader discards.
    assert_eq!(matched_in(bad.path(), 2100).total, Some(1));
    // The second file is untouched.
    assert_eq!(matched_in(good.path(), 0).total, Some(1));
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
    let outcome = check(&["caller"], &[&file]);
    let bridges = &outcome.check.bridges;
    assert_eq!(bridges.len(), 1, "{bridges:?}");
    assert_eq!(bridges[0].index, 0);
    assert!(
        bridges[0].message.contains("'to' side matched none"),
        "got: {}",
        bridges[0].message
    );
    assert!(
        !bridges[0].message.contains("pair(s)"),
        "a pair count needs the index: {}",
        bridges[0].message
    );
}

/// The matched names are a capped *sample*; the count beside them stays exact.
#[test]
fn the_name_sample_is_capped_but_the_count_is_not() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature", "pattern": "^f"}],
             "model": {"sources": [{"kind": "k", "port": "Return"}]}}
        ]}"#,
    );
    let names = ["f1", "f2", "f3", "f4", "f5", "f6", "f7"];
    let outcome = check(&names, &[&file]);
    let matched = &outcome.check.matched[0];
    assert_eq!(matched.total, Some(names.len()));
    assert!(
        matched.sample.len() < names.len(),
        "the sample is capped: {:?}",
        matched.sample
    );
}

/// A `find: callsites` generator is reported as matching *callees*: the call-site fan-out is
/// Stage 2, so a count of matched callees is not a count of call sites.
#[test]
fn a_callsites_generator_reports_its_find() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "callsites",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(*)"}]}}
        ]}"#,
    );
    let outcome = check(&["sink"], &[&file]);
    assert_eq!(outcome.check.matched[0].find.as_deref(), Some("callsites"));
}

/// A propagation that matched nothing is dead too, and the phase that would have consumed it is
/// `ctadl index` -- which is exactly the run the reader is about to start.
#[test]
fn a_dead_propagation_is_reported() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "nosuchfunction"}],
             "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}},
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]}}
        ]}"#,
    );
    let outcome = check(&["sink"], &[&file]);
    let dead = &outcome.check.index_time_dead;
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].index, 0);
    assert_eq!(dead[0].kind, "propagation");
}

/// An import that will not load is recorded, and the loop continues.
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
    let outcome = cli::check_programs(programs, &[file.path().to_path_buf()]).expect("checking");

    assert_eq!(outcome.check.imports.len(), 1, "one import did load");
    assert!(outcome.has_file_errors());
    // The import that did load was still matched.
    assert_eq!(outcome.check.matched[0].total, Some(1));
}

/// The whole point of routing this through the SARIF writer: what the check found comes out in
/// the notification vocabulary an indexed query already uses, in a file a consumer reads the
/// same way.
#[test]
fn the_check_is_reported_as_sarif() {
    let file = model_file(
        ".json",
        r#"{"model_generators": [
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "sink"}],
             "model": {"sinks": [{"kind": "k", "port": "Argument(0)"}]}},
            {"find": "methods",
             "where": [{"constraint": "signature_match", "name": "nosuchfunction"}],
             "model": {"sources": [{"kind": "k", "port": "Return"}]}}
        ]}"#,
    );
    let outcome = check(&["sink"], &[&file]);
    let diagnostics = outcome.into_diagnostics();
    let out = NamedTempFile::with_suffix(".sarif").unwrap();

    // `format_model_check_sarif` requires a project handle, but only uses the
    // project name, so an empty-imports ephemeral project is sufficient.
    let project_name = "test_project";
    let project = AnalysisProject::ephemeral(project_name, &[] as &[&str]);

    let successful =
        format_model_check_sarif(&project, out.path(), SarifProfile::Machine, &diagnostics)
            .unwrap();
    assert!(
        !successful,
        "a run that could not answer the query is not a success"
    );

    let sarif: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(out.path()).unwrap()).unwrap();

    assert_eq!(
        sarif["properties"]["project_name"].as_str(),
        Some(project_name)
    );

    let invocation = &sarif["runs"][0]["invocations"][0];
    let ids: Vec<&str> = invocation["toolConfigurationNotifications"]
        .as_array()
        .expect("configuration notifications")
        .iter()
        .map(|n| n["descriptor"]["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.iter().any(|id| id.starts_with("CTADL0008")),
        "the file must say there was no index: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.starts_with("CTADL0004")),
        "the dead source generator: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.starts_with("CTADL0011")),
        "the live sink generator: {ids:?}"
    );
    assert_eq!(invocation["executionSuccessful"], false);

    // Path search did not run, and the file says which of the two reasons that is: not
    // applicable, rather than "ran to completion and found nothing".
    let results = sarif["runs"][0]["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], "notApplicable");
    assert!(
        results[0]["message"]["text"]
            .as_str()
            .unwrap()
            .contains("no index"),
        "{}",
        results[0]["message"]["text"]
    );
}
