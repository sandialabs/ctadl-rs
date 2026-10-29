/*! [`generator_errors`] against the JSON loader it replaces.

**Remove in phase 2**, with the loader: [`loader_errors`] runs `ModelGeneratorIngest`, which is
the thing being deleted. Until then this is what shows the validator reports what the loader
reports, error for error and in the same order, so the migrator can stop running the loader.

The corpus is every generator the repository has: the shipped defaults, the `tnt` and nightly
model files, and every JSON literal in the loader's, the migrator's and the migration oracle's
tests that parses. Each is checked as written and then perturbed, one node at a time, into the
malformed shapes the loader is there to reject.
*/

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};

use super::generator_errors;
use crate::error::JsonModelError;
use crate::models::spec;

/// What the JSON loader reports against generator `index`: the migrator's former
/// `json_loader_errors`, verbatim. The loader runs over an empty Java program whose import is
/// made up from the generator's own `in`, so that the scope never hides an error.
fn loader_errors(index: usize, generator: &Value) -> Vec<JsonModelError> {
    use crate::models::json::{ModelGeneratorIngest, ModelGeneratorVisitor};
    use crate::models::{ImportScope, ProgramMatchIndex, ProgramModelMatches};
    use ctadl_ir::mir::call::VirtualMethodTable;

    let mut errors = Vec::new();
    let bridge = match generator.pointer("/model/bridge") {
        Some(bridge) => {
            let path = Path::new("<migrate>");
            let spec = spec::parse_bridge(path, index, generator, bridge, &mut errors);
            if !errors.is_empty() {
                return errors;
            }
            spec
        }
        None => None,
    };
    let scope = spec::ProgramScope::parse(generator.get("in"), index, &mut errors);
    if !errors.is_empty() {
        return errors;
    }
    let program = ctadl_ir::mir::ProgramInfo {
        vmt: VirtualMethodTable::Java {
            methods: Vec::new(),
            hierarchy: Default::default(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        },
        ..Default::default()
    };
    let import = ImportScope {
        language: scope.languages.first().copied(),
        import: scope.import,
    };
    let match_index = ProgramMatchIndex::new(&program, import);
    let mut out = ProgramModelMatches::default();
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
    ingest.visit_model_generator(index, generator);
    if let Some(bridge) = bridge {
        ingest.match_where(index, &bridge.to.where_);
    }
    ingest.drain_errors()
}

/// An error list as compared: `Debug` carries the kind, index, field and message, and the
/// underlying regex / integer / path error.
fn render(errors: &[JsonModelError]) -> Vec<String> {
    errors.iter().map(|e| format!("{e:?}")).collect()
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// Test sources whose JSON literals are harvested.
const RUST_SOURCES: &[(&str, &str)] = &[
    (
        "tests/integration/json_error_handling.rs",
        include_str!("../../../tests/integration/json_error_handling.rs"),
    ),
    ("src/models/tests.rs", include_str!("../tests.rs")),
    ("src/models/dsl/tests.rs", include_str!("../dsl/tests.rs")),
    (
        "tests/integration/migration_equivalence.rs",
        include_str!("../../../tests/integration/migration_equivalence.rs"),
    ),
];

/// JSON as written in a Rust test, or `None` if it is not JSON (a DSL source, or a `json!` with a
/// Rust expression in it). JSON5 admits the comments and trailing commas `json!` allows.
fn parse_literal(text: &str) -> Option<Value> {
    serde_json::from_str(text)
        .ok()
        .or_else(|| json5::from_str(text).ok())
}

/// The body of every `json!(…)` and every `r#"…"#` in `src`.
fn literals(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut at = 0;
    while let Some(off) = src[at..].find("json!(") {
        let start = at + off + "json!(".len();
        let mut depth = 1usize;
        let mut i = start;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        out.push(src[start..i - 1].to_string());
        at = i;
    }
    let mut at = 0;
    while let Some(off) = src[at..].find("r#\"") {
        let start = at + off + 3;
        let Some(len) = src[start..].find("\"#") else {
            break;
        };
        out.push(src[start..start + len].to_string());
        at = start + len + 2;
    }
    out
}

/// A where list wrapped in each `find`'s generator, so a constraint literal is checked under
/// every `find` it can meet, and as side B of a bridge.
fn wrap_where(where_: Value) -> Vec<Value> {
    vec![
        json!({
            "find": "methods",
            "where": where_,
            "model": {
                "sources": [{"kind": "Src", "port": "Return"}],
                "sinks": [{"kind": "Snk", "port": "Argument(0)"}],
                "propagation": [{"input": "Argument(0)", "output": "Return"}],
            },
        }),
        json!({
            "find": "callsites",
            "where": where_,
            "model": {"sinks": [{"kind": "Snk", "port": "Argument(0)", "wildcard": false}]},
        }),
        json!({
            "find": "dispatch",
            "where": where_,
            "model": {"propagation": [{"input": "Argument(0)", "output": "Return"}]},
        }),
        json!({
            "find": "methods",
            "in": {"language": "lua"},
            "model": {"bridge": {"to": {"in": {"language": "pcode"}, "where": where_}}},
        }),
    ]
}

/// A `model` wrapped in each `find`'s generator.
fn wrap_model(model: &Value) -> Vec<Value> {
    ["methods", "callsites", "dispatch"]
        .into_iter()
        .map(|find| json!({"find": find, "model": model}))
        .collect()
}

const GENERATOR_KEYS: &[&str] = &["find", "where", "model", "in", "on-unmatched"];
const MODEL_KEYS: &[&str] = &[
    "sources",
    "sinks",
    "propagation",
    "modes",
    "access_paths",
    "bridge",
    "resolve",
    "closure_shaped",
];

/// The generators a parsed literal or model file holds.
fn harvest(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Object(obj) => {
            if let Some(Value::Array(gens)) = obj.get("model_generators") {
                gens.iter().for_each(|g| harvest(g, out));
            } else if obj.keys().any(|k| GENERATOR_KEYS.contains(&k.as_str())) {
                out.push(value.clone());
            } else if obj.contains_key("constraint") {
                out.extend(wrap_where(json!([value])));
            } else if obj.keys().any(|k| MODEL_KEYS.contains(&k.as_str())) {
                out.extend(wrap_model(value));
            }
        }
        Value::Array(items) => {
            if items.iter().any(|v| v.get("constraint").is_some()) {
                out.extend(wrap_where(value.clone()));
            } else {
                items.iter().for_each(|v| harvest(v, out));
            }
        }
        _ => {}
    }
}

/// Every generator of a `.json` / `.json5` / `.jsonl` model file. A file with no
/// `model_generators` (a nightly regression config) holds none.
fn harvest_file(path: &Path, out: &mut Vec<Value>) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if path.extension().is_some_and(|e| e == "jsonl") {
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{}: {e}: {line}", path.display()));
            out.push(value);
        }
        return;
    }
    let Some(root) = parse_literal(&text) else {
        panic!("{}: not JSON or JSON5", path.display());
    };
    if let Some(Value::Array(gens)) = root.get("model_generators") {
        out.extend(gens.iter().cloned());
    }
}

/// Model files under `dir`, recursively, in a stable order.
fn model_files(dir: &Path, suffix: &str, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            model_files(&path, suffix, out);
        } else if path.to_string_lossy().ends_with(suffix) {
            out.push(path);
        }
    }
}

/// Every generator in the repository, deduplicated, in a stable order.
fn corpus() -> Vec<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut gens = Vec::new();
    for (_, src) in RUST_SOURCES {
        for text in literals(src) {
            if let Some(value) = parse_literal(&text) {
                harvest(&value, &mut gens);
            } else {
                // A `.jsonl` fixture written as a raw string.
                for line in text.lines() {
                    if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                        harvest(&value, &mut gens);
                    }
                }
            }
        }
    }
    let from_literals = gens.len();
    let mut files = Vec::new();
    model_files(&root.join("src/models/defaults"), ".jsonl", &mut files);
    model_files(&root.join("tests/tnt"), ".models.jsonl", &mut files);
    let nightly = root.join("../nightly/tests");
    for suffix in [".json", ".json5", ".jsonl"] {
        model_files(&nightly, suffix, &mut files);
    }
    assert!(
        files.len() > 100,
        "expected the defaults, tnt and nightly model files, found {}",
        files.len()
    );
    for path in &files {
        harvest_file(path, &mut gens);
    }
    eprintln!(
        "{from_literals} generators from test literals, {} from {} model files",
        gens.len() - from_literals,
        files.len()
    );
    let mut seen = BTreeSet::new();
    gens.retain(|g| seen.insert(g.to_string()));
    gens
}

// ---------------------------------------------------------------------------
// Perturbation
// ---------------------------------------------------------------------------

/// What a node is replaced with: every JSON type, and the strings that change what the loader
/// checks (each `find`, a `Variable` port, an unparsable port, an invalid regex).
fn replacements() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(7),
        json!("x"),
        json!([]),
        json!({}),
        json!([7]),
        json!({"constraint": "bogus"}),
        json!("methods"),
        json!("callsites"),
        json!("dispatch"),
        json!("Variable(v)"),
        json!("Argument(0).[*]"),
        json!("Argument(99999)"),
        json!("["),
        json!("skip-analysis"),
    ]
}

/// Every generator one edit away from `generator`: each node replaced by each of
/// [`replacements`], each object key removed, and an unknown key added to each object.
fn perturbations(generator: &Value) -> Vec<Value> {
    fn pointers(value: &Value, at: String, out: &mut Vec<String>) {
        match value {
            Value::Object(obj) => {
                for (k, v) in obj {
                    let k = k.replace('~', "~0").replace('/', "~1");
                    pointers(v, format!("{at}/{k}"), out);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    pointers(v, format!("{at}/{i}"), out);
                }
            }
            _ => {}
        }
        out.push(at);
    }
    let mut paths = Vec::new();
    pointers(generator, String::new(), &mut paths);
    let replacements = replacements();
    let mut out = Vec::new();
    for path in &paths {
        if !path.is_empty() {
            for r in &replacements {
                let mut g = generator.clone();
                *g.pointer_mut(path).unwrap() = r.clone();
                out.push(g);
            }
            let (parent, key) = path.rsplit_once('/').unwrap();
            let mut g = generator.clone();
            match g.pointer_mut(parent).unwrap() {
                Value::Object(obj) => {
                    obj.remove(&key.replace("~1", "/").replace("~0", "~"));
                }
                Value::Array(items) => {
                    items.remove(key.parse::<usize>().unwrap());
                }
                _ => unreachable!(),
            }
            out.push(g);
        }
        let mut g = generator.clone();
        if let Some(obj) = g.pointer_mut(path).unwrap().as_object_mut() {
            obj.insert("bogus".to_string(), json!(1));
            out.push(g);
        }
    }
    out
}

/// Asserts the two paths agree on `generator`, naming it if they do not.
fn assert_agree(index: usize, generator: &Value) -> bool {
    let old = render(&loader_errors(index, generator));
    let new = render(&generator_errors(index, generator));
    assert_eq!(
        old, new,
        "loader (left) and validator (right) disagree on {generator}"
    );
    !old.is_empty()
}

/// The corpus as written, many of the harvested literals malformed on purpose.
#[test]
fn validator_matches_the_loader_on_the_corpus() {
    let corpus = corpus();
    assert!(corpus.len() > 900, "corpus has only {}", corpus.len());
    let mut rejected = 0;
    for (i, g) in corpus.iter().enumerate() {
        rejected += usize::from(assert_agree(i, g));
    }
    eprintln!("{} generators, {rejected} rejected by both", corpus.len());
    assert!(rejected > 0, "the corpus exercises no error at all");
}

/// Every one-edit perturbation of the corpus. This is what reaches each of the loader's error
/// sites, under each `find`.
#[test]
fn validator_matches_the_loader_on_perturbations() {
    let corpus = corpus();
    let mut checked = 0usize;
    let mut rejected = 0usize;
    let mut kinds = BTreeSet::new();
    for g in &corpus {
        for p in perturbations(g) {
            checked += 1;
            let errors = generator_errors(3, &p);
            for e in &errors {
                kinds.insert(
                    format!("{e:?}")
                        .split([' ', '{', '('])
                        .next()
                        .unwrap()
                        .to_string(),
                );
            }
            if assert_agree(3, &p) {
                rejected += 1;
            }
        }
    }
    eprintln!("{checked} perturbations, {rejected} rejected by both; kinds: {kinds:?}");
    for kind in [
        "MissingField",
        "FieldNotString",
        "FieldNotArray",
        "UnexpectedField",
        "UnexpectedConstraint",
        "InvalidRegex",
        "InvalidInteger",
        "InvalidArgumentFormat",
        "InvalidAccessPath",
    ] {
        assert!(kinds.contains(kind), "no perturbation raised {kind}");
    }
}
