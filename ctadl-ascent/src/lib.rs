//! CTADL using the Ascent datalog engine.

pub mod cli;
pub mod codegen;
pub mod devguide;
pub mod error;
pub mod facts;
pub mod graphviz;
pub mod index_engine;
pub mod languages;
pub mod lattice;
pub mod models;
/// The CTADL store, which lives in [`ctadl_import`] so that reading an import costs no engine.
/// Re-exported here so `crate::project::…` names it.
pub use ctadl_import::project;
pub mod query_engine;
pub mod stats;

/// Initializes the logger for the shipped `ctadl` binary.
///
/// The default filter is `warn,ctadl=info` -- warnings from every crate, plus this
/// project's status output. Crates that aren't `ctadl*` (the readers, `trie`,
/// `tailshare`, ...) stay at `warn` because they have no status to report. `RUST_LOG`
/// overrides the whole thing, so `RUST_LOG=warn,ctadl=debug` is how to get more and
/// `RUST_LOG=warn` is how to get less.
///
/// Format: `info` is the bare message and `warn` is the message under a `Warning:`
/// heading, both without timestamp or module path, because they are read by a person
/// watching a run. `debug`/`trace`/`error` keep env_logger's default format -- timestamp
/// and module path included -- because they are read while debugging. `error` has no
/// producers by design: a failure propagates through `Result` and is printed once by
/// `anyhow` at the top. If one ever shows up, the verbose format is what points at the
/// bug.
///
/// Idempotent: a second call is a no-op, so a test or example that has already installed
/// a logger keeps it.
pub fn init() {
    use std::io::Write as _;

    // env_logger's default record format, used verbatim for the levels that keep it.
    let verbose = env_logger::fmt::ConfigurableFormat::default();
    env_logger::Builder::new()
        .parse_env(env_logger::Env::default().default_filter_or("warn,ctadl=info"))
        .format(move |buf, record| match record.level() {
            // The heading is prepended once, to the front of the record: messages with
            // embedded newlines (e.g. the graph-dump legends in `cli`) pass through
            // verbatim rather than being re-indented per line.
            log::Level::Warn => {
                let style = buf.default_level_style(log::Level::Warn);
                writeln!(buf, "{style}warning:{style:#} {}", record.args())
            }
            log::Level::Info => writeln!(buf, "{}", record.args()),
            _ => verbose.format(buf, record),
        })
        .try_init()
        .ok();
}
