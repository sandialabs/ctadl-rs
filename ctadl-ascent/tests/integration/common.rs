//! What the tests in this binary share.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ctadl_ascent::project::init_store_path;
use tempfile::TempDir;

/// The store every test in this binary uses, set up on first call.
///
/// `init_store_path` sets a process-global root and fails if called twice, so all the tests in
/// the binary share one store. They keep out of each other's way by giving their imports and
/// projects distinct names. The directory is never deleted: a static is not dropped at exit.
pub fn store_dir() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("temp store");
        init_store_path(Some(dir.path())).expect("init store");
        dir
    })
    .path()
}

/// A copy of the index directory `src`, its files only, as `index` in a temporary directory.
pub fn copy_index(src: &Path) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let copy = tmp.path().join("index");
    std::fs::create_dir(&copy).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
        }
    }
    (tmp, copy)
}

/// Every `warn`-level log message any test in this binary has emitted since the first call,
/// in order.
///
/// The logger is process-global and the tests run in parallel, so a test reads only the
/// messages that name something of its own — a file path, an import name.
pub fn warnings() -> Vec<String> {
    log_capture()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The capture [`warnings`] reads, installed on first call. Nothing else in this binary
/// installs a logger, so this one always wins.
fn log_capture() -> &'static std::sync::Mutex<Vec<String>> {
    static CAPTURE: Capture = Capture(std::sync::Mutex::new(Vec::new()));
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        log::set_logger(&CAPTURE).expect("no other logger in this test binary");
        log::set_max_level(log::LevelFilter::Warn);
    });
    &CAPTURE.0
}

struct Capture(std::sync::Mutex<Vec<String>>);

impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
            held.push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// Installs the capture [`warnings`] reads, so what follows is recorded.
pub fn capture_warnings() {
    log_capture();
}

/// Loads `generators`, the generators of one JSON model file, against `index`, appending what
/// they matched to `out`: migrated to the DSL and run on its engine, which is how every JSON
/// model file is loaded.
pub fn load_json(
    index: &ctadl_ascent::models::ProgramMatchIndex<'_>,
    generators: impl IntoIterator<Item = serde_json::Value>,
    out: &mut ctadl_ascent::models::ProgramModelMatches,
) -> Result<ctadl_ascent::models::ModelLoadReport, ctadl_ascent::error::Error> {
    use ctadl_ascent::models::{
        ModelLoadReport, PortcullisFile, PortcullisMatcher, PortcullisModelSet, PortcullisPhase,
    };
    let generators: Vec<serde_json::Value> = generators.into_iter().collect();
    let file = PortcullisFile::from_json_generators("<models>", &generators)?;
    let set = PortcullisModelSet { files: vec![file] };
    let mut matcher = PortcullisMatcher::new(&set, PortcullisPhase::All);
    matcher.observe_import(index, out);
    matcher.finish(out).map(ModelLoadReport::from_portcullis)
}

/// `e` without the context it was wrapped in on the way out: the error the context is about,
/// such as the [`Error::JsonModel`](ctadl_ascent::error::Error::JsonModel) the migrator rejected
/// a model file with.
pub fn without_context(e: ctadl_ascent::error::Error) -> ctadl_ascent::error::Error {
    use ctadl_ascent::error::Error;
    match e {
        Error::Context { context, source } => match source.downcast::<Error>() {
            Ok(inner) => without_context(*inner),
            Err(source) => Error::Context { context, source },
        },
        e => e,
    }
}

/// [`cli::query`](ctadl_ascent::cli::query), then, when it wrote `output`, the check of every
/// step of every code flow in it ([`step_properties_problem`]). Every test in this binary
/// queries through this, so a step that says the wrong thing fails the test that wrote it.
pub fn query(
    project: &ctadl_ascent::project::AnalysisProject,
    models: &[PathBuf],
    output: &Path,
    profile: ctadl_ascent::query_engine::formatter::SarifProfile,
    dump_taint_graph: Option<&Path>,
) -> Result<ctadl_ascent::cli::QueryStatus, ctadl_ascent::error::Error> {
    let status = ctadl_ascent::cli::query(project, models, output, profile, dump_taint_graph);
    if output.is_file() {
        let text = std::fs::read_to_string(output).expect("reading the SARIF query wrote");
        let doc: serde_json::Value =
            serde_json::from_str(&text).expect("parsing the SARIF query wrote");
        if let Some(problem) = step_properties_problem(&doc) {
            panic!("{}: {problem}", output.display());
        }
    }
    status
}

/// What is wrong with the first code-flow step of `sarif` that does not say what it should,
/// or `None` when every step does. A step has a string `taintVertex`, and its message is
/// `<word> <taintVertex> in <function>`. A source or sink step, told by its message's first
/// word, has neither `callee` nor `summarized`; every other step has exactly one: `callee`, a
/// string, or `summarized`, `true`.
///
/// `xtask`'s regression suite has the same check (`check_step_properties`), for the flows it
/// queries.
pub fn step_properties_problem(sarif: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let items = |v: &Value, key: &str| -> Vec<Value> {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for (r, run) in items(sarif, "runs").iter().enumerate() {
        for (n, result) in items(run, "results").iter().enumerate() {
            let rule = result.get("ruleId").and_then(Value::as_str).unwrap_or("");
            for (c, flow) in items(result, "codeFlows").iter().enumerate() {
                for (t, thread) in items(flow, "threadFlows").iter().enumerate() {
                    for (s, step) in items(thread, "locations").iter().enumerate() {
                        let message = step
                            .pointer("/location/message/text")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(problem) = step_problem(step, message) {
                            return Some(format!(
                                "run {r}, result {n} ({rule}), code flow {c}, thread flow {t}, \
                                 step {s} ({message:?}): {problem}"
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

/// What is wrong with one step whose message is `message`, or `None`.
fn step_problem(step: &serde_json::Value, message: &str) -> Option<String> {
    use serde_json::Value;
    let props = step.get("properties");
    let prop = |key: &str| props.and_then(|p| p.get(key));
    let Some(vertex) = prop("taintVertex").and_then(Value::as_str) else {
        return Some("no string taintVertex".to_string());
    };
    let Some((word, rest)) = message.split_once(' ') else {
        return Some("the message has no vertex".to_string());
    };
    if !rest
        .strip_prefix(vertex)
        .is_some_and(|tail| tail.starts_with(" in "))
    {
        return Some(format!(
            "the message does not name taintVertex {vertex:?} as `{word} {vertex} in ...`"
        ));
    }
    let callee = prop("callee");
    let summarized = prop("summarized");
    if matches!(word, "source" | "sink") {
        if callee.is_some() || summarized.is_some() {
            return Some(format!("a {word} step has callee or summarized"));
        }
        return None;
    }
    match (callee, summarized) {
        (Some(Value::String(_)), None) | (None, Some(Value::Bool(true))) => None,
        (Some(_), Some(_)) => Some("both callee and summarized".to_string()),
        (None, None) => Some("neither callee nor summarized".to_string()),
        (Some(c), None) => Some(format!("callee {c} is not a string")),
        (None, Some(s)) => Some(format!("summarized {s} is not true")),
    }
}
