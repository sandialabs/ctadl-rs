//! Check the model files ctadl ships against the model generator schema.
//!
//! `ctadl-model-generator.schema.json` is the contract users write generators against: it is
//! what `ctadl` names in the `$schema` of the model files it emits, and what
//! `docs/model-generators.md` sends readers to. Nothing in the analyzer reads it. The loader
//! (`ctadl-ascent/src/models/json.rs`) has its own serde types, so a keyword added there and
//! used in a shipped default file can sit outside the schema indefinitely -- and every editor
//! validating a user's file against that schema then reports *our own* models as errors.
//!
//! This check closes that gap in the direction it can: every generator we ship must validate
//! against the schema we publish. The other direction -- a schema that permits a shape the
//! loader rejects -- is not observable from here; only a model file using that shape would
//! catch it, and the taint cases are what exercise the loader.
//!
//! Files are discovered rather than listed, so a default model file added to the crate is
//! checked the day it lands rather than the day someone remembers to name it here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use boon::{CompileError, Compiler, Draft, SchemaIndex, Schemas};

use crate::regression::Outcome;

/// The published schema, relative to the models directory.
const SCHEMA_FILE: &str = "ctadl-model-generator.schema.json";

/// Where the built-in default model files live, relative to the models directory.
const DEFAULTS_DIR: &str = "defaults";

/// JSON pointer to the subschema a single generator has to satisfy.
///
/// A `jsonl` file is a stream of `model_generators` elements with no envelope around them (see
/// `try_load_jsonl_models`), so the elements are what each line is checked against. If the
/// schema is ever restructured so this pointer no longer resolves, compiling it fails loudly
/// rather than silently checking nothing.
const GENERATOR_POINTER: &str = "#/properties/model_generators/items";

/// How many bad generators to spell out in one file's report. The rest are counted; a file that
/// is wrong in fifty places is wrong for one reason, and the first few show it.
const MAX_REPORTED: usize = 10;

/// How much of one generator's failure to print. A `where` clause is an `anyOf` over every
/// constraint kind, so rejecting one prints why *each* branch rejected it -- around forty lines
/// saying the same thing from ten angles. Both caps announce what they dropped.
const MAX_DETAIL_LINES: usize = 15;

/// Validate every model file under `models_dir` against the schema that lives beside them.
/// Returns named (check, outcome) pairs to fold into the regression report -- one per file, so
/// a break in the Java models does not hide the Lua ones.
pub fn run_checks(models_dir: &Path) -> Result<Vec<(String, Outcome)>> {
    let schema = models_dir.join(SCHEMA_FILE);

    // A schema that will not compile fails as a single entry: there is nothing to check the
    // files against, and reporting the same compile error once per file says no more.
    let (schemas, generator) = match compile(&schema) {
        Ok(compiled) => compiled,
        Err(err) => {
            return Ok(vec![(
                "models:schema".to_string(),
                Outcome::Fail(format!("{}: {err:#}", schema.display())),
            )]);
        }
    };

    let defaults = models_dir.join(DEFAULTS_DIR);
    let files = model_files(&defaults)?;
    if files.is_empty() {
        // Pointing `--models-dir` at the wrong place would otherwise report a clean run over
        // nothing at all.
        return Ok(vec![(
            "models:defaults".to_string(),
            Outcome::Fail(format!(
                "no .jsonl model files under {}",
                defaults.display()
            )),
        )]);
    }

    Ok(files
        .into_iter()
        .map(|path| {
            let name = format!("models:{}", file_name(&path));
            let outcome = check_file(&path, &schemas, generator)
                .unwrap_or_else(|err| Outcome::Fail(format!("{err:#}")));
            (name, outcome)
        })
        .collect())
}

/// Compile the schema and return it alongside the index of the single-generator subschema.
///
/// The draft is pinned rather than inferred. The schema declares `$schema: 2020-12` today, so
/// this only matters if that line is ever dropped -- at which point boon would read the file as
/// whatever draft it defaults to *then*, quietly changing what passes.
fn compile(schema: &Path) -> Result<(Schemas, SchemaIndex), CompileError> {
    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    // boon takes a location, and resolves a bare path against the cwd into a `file:` URL.
    let loc = schema.display().to_string();
    let generator = compiler.compile(&format!("{loc}{GENERATOR_POINTER}"), &mut schemas)?;
    Ok((schemas, generator))
}

/// Every model file in `dir`, in a stable order.
fn model_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    Ok(files)
}

/// Check one `jsonl` model file: every generator in it must satisfy the schema.
fn check_file(path: &Path, schemas: &Schemas, generator: SchemaIndex) -> Result<Outcome> {
    let name = file_name(path);
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let mut problems: Vec<String> = Vec::new();
    let mut generators = 0usize;
    for (index, line) in text.lines().enumerate() {
        // Line numbers, not generator indices: this report is read next to the file in an
        // editor. (`CTADL0004` and the loader's own errors count generators instead, because
        // they are read next to a diagnostic that names one.)
        let line_no = index + 1;
        let trimmed = line.trim_start();
        // Mirrors `try_load_jsonl_models`: blank lines and `//` lines are commentary.
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        generators += 1;

        let value = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => value,
            Err(err) => {
                problems.push(format!("{name}:{line_no}: not JSON: {err}"));
                continue;
            }
        };
        if let Err(err) = schemas.validate(&value, generator) {
            problems.push(problem(&name, line_no, &err.to_string()));
        }
    }

    if generators == 0 {
        return Ok(Outcome::Fail(format!(
            "no model generators in {}",
            path.display()
        )));
    }
    if problems.is_empty() {
        return Ok(Outcome::Pass);
    }

    // The headline counts every bad generator; only the listing is capped, and it says so.
    let bad = problems.len();
    let hidden = bad.saturating_sub(MAX_REPORTED);
    problems.truncate(MAX_REPORTED);
    if hidden > 0 {
        problems.push(format!("... and {hidden} more"));
    }
    Ok(Outcome::Fail(format!(
        "{bad} of {generators} generator(s) do not match {SCHEMA_FILE}:\n{}",
        problems.join("\n"),
    )))
}

/// One validation failure, addressed as `file:line`.
///
/// boon leads with `jsonschema validation failed with <schema url>#<pointer>`, the same sentence
/// for every failure here and already implied by the check's name, so the location replaces it.
/// What follows is the part that says which keyword rejected what -- one line for a flat
/// mismatch, a tree of them when an `anyOf` branch fails.
fn problem(name: &str, line_no: usize, rendered: &str) -> String {
    let details = rendered.split_once('\n').map_or("", |(_, rest)| rest);
    let mut lines = details.lines();
    let first = lines
        .next()
        .unwrap_or("")
        .trim_start()
        .trim_start_matches("- ");
    let mut out = format!("{name}:{line_no}: {first}");
    // The headline line counts as one, so the tail gets the rest of the budget.
    let rest: Vec<&str> = lines.collect();
    let hidden = rest.len().saturating_sub(MAX_DETAIL_LINES - 1);
    for line in rest.iter().take(MAX_DETAIL_LINES - 1) {
        out.push('\n');
        out.push_str(line);
    }
    if hidden > 0 {
        out.push_str(&format!("\n  ... and {hidden} more line(s)"));
    }
    out
}

/// The file's own name, for report entries and messages. Falls back to the full path if it has
/// no final component, which a directory entry always does.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{check_file, compile, model_files, run_checks, SCHEMA_FILE};
    use crate::regression::Outcome;
    use std::path::{Path, PathBuf};

    /// The real models directory, which is what the check exists to guard.
    fn models_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctadl-ascent/src/models")
    }

    fn outcome_reason(outcome: &Outcome) -> String {
        match outcome {
            Outcome::Pass => "PASS".to_string(),
            Outcome::Skip(why) => format!("SKIP {why}"),
            Outcome::Fail(why) | Outcome::HardFail(why) => format!("FAIL {why}"),
            Outcome::Xfail(why) => format!("XFAIL {why}"),
        }
    }

    /// The check's whole point: what ctadl ships validates against what ctadl publishes.
    #[test]
    fn the_shipped_models_match_the_shipped_schema() {
        let results = run_checks(&models_dir()).unwrap();
        assert!(!results.is_empty(), "no model files were checked");
        for (name, outcome) in &results {
            assert!(
                matches!(outcome, Outcome::Pass),
                "{name}: {}",
                outcome_reason(outcome)
            );
        }
        // One entry per default file; the loader selects between them by frontend.
        let names: Vec<&String> = results.iter().map(|(name, _)| name).collect();
        assert!(
            names.contains(&&"models:java-index.jsonl".to_string()),
            "{names:?}"
        );
    }

    /// Write `text` as a model file in a scratch dir and check it. `name` keeps concurrently
    /// running tests off each other's directory.
    fn check_text(name: &str, text: &str) -> Outcome {
        let dir = std::env::temp_dir().join(format!("xtask_models_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.jsonl"));
        std::fs::write(&path, text).unwrap();

        let (schemas, generator) = compile(&models_dir().join(SCHEMA_FILE)).unwrap();
        let outcome = check_file(&path, &schemas, generator).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        outcome
    }

    const GENERATOR: &str = r#"{"find":"methods","where":[{"constraint":"signature_match","name":"toString"}],"model":{"propagation":[{"input":"Argument(0)","output":"Return"}]}}"#;

    #[test]
    fn commentary_is_not_a_generator() {
        // Blank and `//` lines are skipped exactly as the loader skips them, so a file whose
        // header explains itself still checks its one real generator.
        let outcome = check_text("commentary", &format!("// why\n\n{GENERATOR}\n"));
        assert!(
            matches!(outcome, Outcome::Pass),
            "{}",
            outcome_reason(&outcome)
        );

        // A file that is *only* commentary has nothing to validate, which must not read as a
        // clean run.
        let outcome = check_text("empty", "// nothing here\n");
        assert!(
            outcome_reason(&outcome).contains("no model generators"),
            "{}",
            outcome_reason(&outcome)
        );
    }

    #[test]
    fn a_key_the_schema_does_not_know_fails_with_its_line() {
        // The drift this check exists for: a generator using something the published schema
        // has never heard of.
        let outcome = check_text(
            "unknown_key",
            &format!(
                "{GENERATOR}\n{}\n",
                GENERATOR.replace("\"where\"", "\"wheres\"")
            ),
        );
        let why = outcome_reason(&outcome);
        assert!(why.contains("1 of 2 generator(s)"), "{why}");
        assert!(why.contains("unknown_key.jsonl:2:"), "{why}");
        assert!(why.contains("wheres"), "{why}");
    }

    #[test]
    fn a_malformed_line_is_reported_as_json_rather_than_schema() {
        let outcome = check_text("malformed", "{\"find\": \n");
        let why = outcome_reason(&outcome);
        assert!(why.contains("malformed.jsonl:1: not JSON"), "{why}");
    }

    #[test]
    fn only_model_files_are_picked_up() {
        // The schema itself sits one directory up from the defaults, and nothing but `.jsonl`
        // is a model file, so discovery cannot pick up a README or a stray `.json` sample.
        let files = model_files(&models_dir().join("defaults")).unwrap();
        assert!(!files.is_empty());
        assert!(
            files
                .iter()
                .all(|p| p.extension().is_some_and(|e| e == "jsonl")),
            "{files:?}"
        );
    }
}
