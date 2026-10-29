//! What the tests in this binary share.

use std::path::Path;
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
    use ctadl_ascent::models::{DslFile, DslMatcher, DslModelSet, DslPhase, ModelLoadReport};
    let generators: Vec<serde_json::Value> = generators.into_iter().collect();
    let file = DslFile::from_json_generators("<models>", &generators)?;
    let set = DslModelSet { files: vec![file] };
    let mut matcher = DslMatcher::new(&set, DslPhase::All);
    matcher.observe_import(index, out);
    matcher.finish(out).map(ModelLoadReport::from_dsl)
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
