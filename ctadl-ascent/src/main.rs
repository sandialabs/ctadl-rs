use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};

use ctadl_ascent::cli;
use ctadl_ascent::codegen::{CallPolicy, CallResolutionStrategy, DispatchOrder};
use ctadl_ascent::index_engine::{ContextJoin, HybridContext, Parallelism, Storage};
use ctadl_ascent::project;
use ctadl_ascent::query_engine::formatter::SarifProfile;
use ctadl_ascent::report::ReportFormat;

/// ctadl: import artifacts, index programs, and run/query analyses.
#[derive(Debug, Parser)]
#[command(name = "ctadl", version, about)]
pub struct Cli {
    /// Directory to use as the CTADL store. When set, this directory is used
    /// directly as the store root; unlike `XDG_STATE_HOME`, no `ctadl`
    /// subdirectory is appended. When omitted, the store defaults to
    /// `$XDG_STATE_HOME/ctadl`.
    #[arg(long, global = true, value_name = "DIR")]
    pub store: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Import a single artifact (dex, jar, .class, directory of .c files, etc.)
    Import(ImportArgs),

    /// Index artifacts. (See 'import' to import artifacts)
    ///
    /// Indexes a set of artifacts, such as Java programs along with shared libraries.
    /// The index is stored under the project name.
    Index(IndexArgs),

    /// Run a taint analysis query and format the results as SARIF. (See 'index' for prerequisites)
    ///
    /// Given `--models` and no index -- because the project was never indexed, or is being
    /// indexed right now -- this reports what those model files match in the imported
    /// program(s) instead of failing. Model matching happens in two stages and only the second
    /// needs an index, so a model file can be made ready while the index is still building.
    Query(QueryArgs),

    /// One-shot: import artifacts, index them under name, query, and format output
    Go(GoArgs),

    /// Generate a template Portcullis model file to help write custom analysis models. Analysis
    /// models are used to specify sources and sinks (see the 'query' command) as well as
    /// specifying external function behavior (see the 'index' command)
    InitModel(InitModelArgs),

    /// Rewrite a JSON/JSON5/JSONL model-generator file in Portcullis.
    ///
    /// Portcullis is the format the engine executes; a JSON file is translated through this and
    /// then run, so migrating one changes nothing about what it matches. What it buys is a file
    /// you can read: the migrator also reports every construct that does not carry across,
    /// which a silent translation would hide.
    MigrateModels(MigrateModelsArgs),

    /// Inspect the CTADL store
    Inspect(InspectArgs),

    /// Generate detailed report on taint-analysis relevant program properties. (See 'import')
    ///
    /// Needs only an import. Every number comes from the imported IR and one class-hierarchy
    /// analysis; no index is read, and none is written.
    Report(ReportArgs),
}

#[derive(Debug, Args)]
pub struct InitModelArgs {
    /// Path where the template model file will be written (defaults to model.ctadl)
    #[arg(default_value = "model.ctadl")]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct MigrateModelsArgs {
    /// The JSON / JSON5 / JSONL model file to translate.
    pub input: PathBuf,

    /// Where to write the Portcullis file. Defaults to the input with a `.ctadl` extension; `-`
    /// writes to standard output.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Check the translated file loads, and report what it says, without writing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// A language the file is for (repeatable), as a generator's `in` names one. A rule that only
    /// another language could match is left out.
    #[arg(long = "language", value_name = "LANGUAGE")]
    pub languages: Vec<String>,
}

impl Command {
    fn import_artifact(&self) -> &PathBuf {
        match self {
            Command::Import(args) => &args.artifact,
            Command::Go(args) => &args.artifacts[0],
            _ => panic!("command does not have an artifact"),
        }
    }
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Artifact to import (file or directory)
    ///
    /// Examples: foo.dex, lib.jar, Bar.class, ./c_sources/
    pub artifact: PathBuf,

    /// Name for the artifact. Uses filename by default
    #[arg(long, short)]
    pub name: Option<String>,

    /// Language/IR family for the artifact, or auto to detect it
    #[arg(long, short, value_enum, default_value_t = ImportLanguage::Auto)]
    pub language: ImportLanguage,

    /// Re-import even when an up-to-date import already exists.
    ///
    /// By default, an import is skipped when an import of the same name already
    /// exists whose stored artifact path and content hash match the artifact being
    /// imported, and likewise each of an APK's native libraries whose bytes are
    /// unchanged. This avoids re-doing the (potentially expensive) translation, above
    /// all the disassembly of native libraries. Pass this to redo it anyway, e.g. to
    /// pick up import-time output that a newer ctadl produces.
    #[arg(long)]
    pub force: bool,

    /// Do not import the native libraries packaged inside an APK.
    ///
    /// By default, importing an APK also disassembles the `.so` files under
    /// `lib/<abi>/` and imports each as its own program, so that Java `native`
    /// methods link to their implementations. Pass this to import only the Dex.
    #[arg(long)]
    pub no_native_libs: bool,

    /// Import an APK's native libraries for this ABI (e.g. armeabi-v7a).
    ///
    /// An APK usually ships the same library built for several ABIs; they are copies
    /// of one program, so only one is imported. The default picks the first available
    /// of arm64-v8a, armeabi-v7a, armeabi, x86_64, x86.
    #[arg(long, value_name = "ABI")]
    pub native_abi: Option<String>,

    /// Import a .NET directory's framework and BCL assemblies too (`System.*`,
    /// `Microsoft.Extensions.*`, `mscorlib`, ...).
    ///
    /// By default only the application's own assemblies are imported, and the framework's
    /// are read for their metadata alone: base types, interfaces, type forwarders. A
    /// self-contained publish ships some 200 of them.
    #[arg(long)]
    pub include_framework: bool,

    /// Number of threads Ghidra may use to analyze and decompile native code.
    ///
    /// `0`, the default, or `-j` with no value, leaves it to Ghidra, which uses every core the
    /// OS reports. Any other value caps Ghidra, and the JVM it runs in, at that many.
    #[arg(
        short = 'j',
        long,
        num_args = 0..=1,
        default_value_t = 0,
        default_missing_value = "0",
        value_name = "N"
    )]
    pub jobs: usize,
}

#[derive(Debug, Clone, ValueEnum, Copy)]
pub enum ImportLanguage {
    /// Treat as JVM bytecode inputs (e.g., .class)
    Jvm,
    /// Treat as JVM bytecode JAR inputs
    Jar,
    /// Treat as Android DEX inputs (e.g., .dex)
    Dex,
    /// Treat as Android APK inputs
    Apk,
    /// Treat as an Android app bundle (`.xapk`), a ZIP of split APKs. Each split is imported
    /// through the APK path, so the Dex in the base and the libraries in `config.<abi>.apk`
    /// are co-indexed by naming the bundle.
    Xapk,
    /// Treat as C files
    C,
    /// Treat as Lua source files (parsed with the tree-sitter Lua grammar)
    Lua,
    /// Export pcode via Ghidra. The artifact may be a binary to import, an existing
    /// local Ghidra project (`<name>.gpr` or its directory), or a Ghidra Server
    /// repository URL (`ghidra://…`).
    Pcode,
    /// Treat as Flowy file
    Flowy,
    /// Treat as a .NET assembly (`.dll`/`.exe`), or a directory of them (a published app)
    Cil,
    /// Infer from extension/content
    Auto,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Imported artifact name, index name, or a path to a file in the store.
    pub name: Option<String>,

    /// Instead of summary statistics, pretty-print the imported IR. Prints every function
    /// unless `--function` narrows the set. Requires an imported artifact name.
    #[arg(long, requires = "name")]
    pub dump_ir: bool,

    /// With `--dump-ir`, only print functions whose name contains this substring.
    #[arg(long, value_name = "SUBSTR", requires = "dump_ir")]
    pub function: Option<String>,

    /// Write the index (assign-like) graph of this project to a Graphviz DOT file. Requires a
    /// project name; conflicts with `--dump-ir`.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with = "dump_ir",
        requires = "name"
    )]
    pub dump_index_graph: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Analysis project or imported program name. Naming a project reports on every program
    /// in it, sub-imports included -- an APK's splits and native libraries each get their own
    /// section, because the class hierarchy is per program.
    pub name: String,

    /// Output file path. Defaults to `-`, meaning stdout.
    #[arg(long, short, default_value = "-")]
    pub output: PathBuf,

    /// Output format: `text` to read, `json` to track the numbers across runs.
    #[arg(long, short, value_enum, default_value_t = ReportFormat::Text)]
    pub format: ReportFormat,

    /// How many rows the worst-signature and most-called-method lists carry.
    #[arg(long, default_value_t = 10, value_name = "N")]
    pub top: usize,

    /// Skip the expensive recursion and strongly-connected-component section.
    #[arg(long)]
    pub no_recursion: bool,

    /// Load additional models from one or more model files (`.ctadl`, or the deprecated JSON
    /// formats), as `ctadl index` would. The report reads nothing but writes the `policy` section
    /// against them, so this is how to see what a `dispatch` model would cover before re-indexing.
    #[arg(long, short, action = clap::ArgAction::Append, value_name = "FILE")]
    pub models: Vec<PathBuf>,

    /// Suppress the built-in default models, leaving `--models` as the complete set.
    #[arg(long)]
    pub no_default_models: bool,

    /// The call-resolution policy the `policy` section simulates. Give the same values as
    /// `ctadl index` and the two agree. See `ctadl index --help`.
    #[arg(long, default_value_t = ctadl_ascent::codegen::DEFAULT_CHA_THRESHOLD)]
    pub cha_threshold: usize,

    /// `--cha-threshold` for interface-dispatched sites. Defaults to `--cha-threshold`.
    #[arg(long)]
    pub cha_threshold_interface: Option<usize>,

    /// Whether a matched dispatch model beats the threshold or the other way round.
    #[arg(long, value_enum, default_value_t = DispatchOrder::ModelFirst)]
    pub dispatch_order: DispatchOrder,
}

/// The call-resolution strategy, and the ladder flags `--strategy mixed` reads.
#[derive(Debug, Clone, Args)]
pub struct CallPolicyArgs {
    /// Call resolution strategy: cha, hi, mixed, legacy-mixed
    ///
    /// `mixed` is the ladder: an `invoke-super` resolves to its one real target, then a
    /// dispatch model, then the threshold below, then hybrid inlining. `legacy-mixed` is CHA
    /// when the site has exactly one target and hybrid inlining otherwise.
    #[arg(long, value_enum, default_value_t = CallResolutionStrategy::Mixed)]
    pub strategy: CallResolutionStrategy,

    /// Resolve a call site with CHA when it has at most this many targets, and defer it to
    /// hybrid inlining above that. `--strategy mixed` only.
    ///
    /// `0` resolves nothing with CHA; a very large value never defers. Raising it leaves less
    /// to hybrid inlining but costs the index engine: on a program with a large recursive
    /// strongly connected component the CHA edges it adds are superlinear in the fixpoint. The
    /// default is where that cost starts; going higher is worth it only when the extra
    /// precision is.
    #[arg(long, default_value_t = ctadl_ascent::codegen::DEFAULT_CHA_THRESHOLD)]
    pub cha_threshold: usize,

    /// `--cha-threshold` for interface-dispatched sites, which are a different population:
    /// about a tenth of them resolve to a single target against four fifths of ordinary
    /// virtual calls. Defaults to `--cha-threshold`.
    #[arg(long)]
    pub cha_threshold_interface: Option<usize>,

    /// Ignore `find: "dispatch"` models, so every site takes the threshold instead.
    #[arg(long)]
    pub no_dispatch_models: bool,

    /// Ignore `find: "dispatch"` models at interface-dispatched sites only.
    #[arg(long)]
    pub no_dispatch_models_interface: bool,

    /// Whether a matched dispatch model beats the threshold or the other way round.
    ///
    /// `model-first` takes a modelled signature out of the analysis whatever its target count.
    /// `threshold-first` resolves a signature with few enough targets exactly even when a model
    /// matches it, for a precision-sensitive run.
    #[arg(long, value_enum, default_value_t = DispatchOrder::ModelFirst)]
    pub dispatch_order: DispatchOrder,
}

impl CallPolicyArgs {
    /// The ladder flags as one policy. `--cha-threshold-interface` defaults to
    /// `--cha-threshold`, so setting one flag gives one threshold and the second exists only
    /// for the population that behaves differently.
    ///
    /// Warns when a ladder flag is given with a strategy that does not read it: the policy is
    /// recorded in the index config as given either way, so a silent no-op would be
    /// indistinguishable from a policy that took effect.
    pub fn call_policy(&self) -> CallPolicy {
        let policy = CallPolicy {
            cha_threshold: self.cha_threshold,
            cha_threshold_interface: self.cha_threshold_interface.unwrap_or(self.cha_threshold),
            dispatch_models: !self.no_dispatch_models,
            dispatch_models_interface: !self.no_dispatch_models
                && !self.no_dispatch_models_interface,
            order: self.dispatch_order,
        };
        if self.strategy != CallResolutionStrategy::Mixed && policy != CallPolicy::default() {
            log::warn!(
                "the call-policy flags apply to --strategy mixed; --strategy {:?} ignores them",
                self.strategy
            );
        }
        policy
    }
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Name for the analysis project (index name)
    pub name: String,

    /// One or more imported program names to co-index (from import step). If none given, assumes
    /// the project name also refers to the import.
    pub progs: Vec<String>,

    /// Load summaries from one or more previously indexed projects and map them into the current project.
    /// The summaries are filtered to functions that exist in the current project, including native
    /// targets linked by the JNI bridge: a summary project's native libraries (their symbol and
    /// `RegisterNatives` tables, not their code) are available to link this project's Java
    /// `native` methods against. Can be specified multiple times to load from multiple projects.
    #[arg(long, short, action = clap::ArgAction::Append, id = "NAME")]
    pub summary: Vec<String>,

    /// Do not co-index the native libraries imported out of an APK or XAPK.
    ///
    /// The index-time counterpart of `ctadl import --no-native-libs`: naming an APK normally
    /// also indexes every `.so` imported out of it, and this drops those (the `pcode`
    /// sub-imports) while keeping an XAPK's split APKs. An import named explicitly is always
    /// kept. Use it with `--summary` to index the Java half of an app against a project that
    /// indexed one of its libraries on its own:
    ///
    ///   ctadl index xproj app__arm64-v8a__libX
    ///   ctadl index appproj app --no-native-libs --summary xproj
    #[arg(long)]
    pub no_native_libs: bool,

    /// Load additional models from one or more files. Can be specified multiple times to load
    /// multiple model files. This option is used primarily to provide propagation models, which
    /// provide function summaries for indexing external or hard-to-analyze code.
    ///
    /// A `.ctadl` (or `.dl`) file is written in Portcullis, which is what the engine runs;
    /// `.json`, `.json5` and `.jsonl` are the older model-generator format, deprecated: such a
    /// file is migrated to Portcullis as it is loaded. `ctadl migrate-models` converts one for
    /// good.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// Do not load the built-in default propagation models for the imported language.
    ///
    /// CTADL ships one default model file per frontend family (Java, native, Lua) and loads the
    /// one matching each import. Pass this to index against `--models` alone -- for an A/B
    /// measurement of what the defaults add, or when a model file is meant to be the complete
    /// story.
    #[arg(long)]
    pub no_default_models: bool,

    /// Do not link Java `native` methods to their native implementations.
    ///
    /// When a Java/Dex artifact is co-indexed with native code, CTADL joins each `native` method
    /// to the `Java_…` symbol implementing it, mapping arguments across the JNI ABI's two-slot
    /// shift so taint flows both ways. Pass this to measure what the bridge contributes. See
    /// `ctadl_ascent::languages::jni`.
    #[arg(long)]
    pub no_jni_bridge: bool,

    /// Ignore the `RegisterNatives` tables recovered from a native library at import time.
    ///
    /// Most Android apps bind their natives at run time, from `JNI_OnLoad`, rather than through
    /// the `Java_…` symbol convention; CTADL recovers those bindings out of the library's data
    /// sections when it imports it. Pass this to link by symbol name alone -- an A/B measurement
    /// of what the registry contributes, with no re-import needed. Implied by
    /// `--no-jni-bridge`. See `ctadl_ascent::languages::jni::registry`.
    #[arg(long)]
    pub no_jni_registry: bool,

    #[command(flatten)]
    pub call: CallPolicyArgs,

    /// Prune unreachable CFG nodes before SSA transformation.
    ///
    /// On by default: SSA/dominator construction requires every block to be
    /// reachable from entry, and real disassembled binaries routinely contain
    /// unreachable blocks. Pass `--prune-unreachable-cfg-nodes=false` to disable
    /// pruning explicitly (e.g. for inputs known to be fully connected).
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub prune_unreachable_cfg_nodes: Option<bool>,

    /// Enable the aliasing summary rule during indexing.
    ///
    /// On by default. The rule turns aliased stores into summaries that re-enter as assignments,
    /// which can cause combinatorial blowup of the `locals` relation on pointer-heavy binaries.
    /// Pass `--alias-rule=false` to disable it.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub alias_rule: Option<bool>,

    /// How the flows of a resolved indirect or virtual call are kept apart by caller.
    ///
    /// `decision` (the default) tags each flow with the set of decisions -- which formal held
    /// which target -- it holds under, so every caller that passed that target gets the flow
    /// and no other caller does. `collapse` does the same but gives up the set the moment two
    /// decisions meet at a flow, sharing that flow among every caller that passed any target:
    /// coarser, and far cheaper on a function that thousands of targets reach. `none` shares
    /// the resolved callee's flows among all callers, which is the cheapest and the least
    /// precise.
    #[arg(long, default_value = "decision")]
    pub hybrid_context: HybridContext,

    /// How the index engine pairs a function's conditional summaries with the calls that
    /// established a decision at it (rule 3.2 of hybrid inlining). `sets` (the default) and
    /// `unfold` join on the decision exactly; `scan` joins on the function and filters, which
    /// is quadratic on functions that many decisions reach. Same result under all three.
    #[arg(long, default_value = "sets")]
    pub context_join: ContextJoin,

    /// Number of threads to compute the flow relation with.
    ///
    /// `1`, the default, runs the serial index engine. Any larger value runs the parallel
    /// engine on exactly that many threads. `0`, or `-j` with no value, uses every core the OS
    /// reports. Both engines evaluate the same rules and derive the same relations. The IR
    /// passes that run before the engine (SSA and its cleanups) use the same number of threads.
    #[arg(short = 'j', long, num_args = 0..=1, default_missing_value = "0", value_name = "N")]
    pub jobs: Option<usize>,

    /// Experimental: keep the largest relations in memory-mapped LSM files instead of in-memory
    /// tries.
    ///
    /// The relations that dominate the index's memory (`locals`, `assign_like` and the
    /// propagation relations built from them) then live mostly in run files under `$TMPDIR`,
    /// read through the page cache, with only a write buffer per index in memory: a run that
    /// would not fit in RAM can finish, at some cost in time. Runs the serial engine, so it
    /// cannot be combined with `-j`.
    #[arg(long, conflicts_with = "jobs")]
    pub lsm: bool,

    /// With `--lsm`, how many entries each index buffers in memory before writing them out as
    /// a run file. Larger buffers mean fewer, larger runs (faster lookups) and more memory.
    #[arg(long, requires = "lsm", value_name = "ENTRIES")]
    pub lsm_memtable_limit: Option<usize>,

    /// With `--lsm`, the most memory the LSM's write buffers may hold at once, all of them
    /// together, e.g. `512M` or `4G` (powers of 1024; a bare number is bytes).
    ///
    /// Over the budget, a buffer that takes a new tuple is written out as a run file, so a
    /// machine with little RAM can index a program that would otherwise not fit. The runs
    /// themselves are read through the page cache, which the OS shrinks under pressure. The
    /// budget does not cover the rest of the index: the relations kept in memory and the
    /// program's facts still take what they take.
    #[arg(long, requires = "lsm", value_name = "SIZE", value_parser = parse_size)]
    pub lsm_memory_budget: Option<usize>,

    /// How stack slots of native code are promoted to locals before SSA.
    ///
    /// A slot whose address escapes (passed to a call, stored, copied, returned) stays in
    /// memory, and the policy says how far an escaped address reaches: `window:N` (the next N
    /// slots; `window:8` is the default), `exact` (the slot alone) or `above` (every slot above
    /// it). `off` promotes nothing.
    ///
    /// Experimental: `full` promotes every slot, escaped or not, in a function with more than
    /// 1000 direct stack accesses (`full:N` for another threshold), and `window:8` elsewhere. It
    /// cuts the index of a large native function by a large factor, but a callee or a copy of
    /// the address no longer sees what the function stored directly. Each function where this
    /// may lose flows is named in the index's `notifications.sarif` and in the query's SARIF. A
    /// model's `modes: ["mem2reg-full"]` or `["mem2reg-window"]` forces or forbids it for one
    /// function.
    #[arg(
        long,
        value_name = "POLICY",
        default_value = "window:8",
        value_parser = parse_mem2reg
    )]
    pub mem2reg: Mem2RegFlag,

    /// How native code's writes to its stack frame reach SSA.
    ///
    /// The pcode frontend writes a stack slot as a functional update of the frame, which SSA can
    /// version. `update` (the default) keeps them, so SSA versions the frame for the slots
    /// `--mem2reg` leaves in memory, and a write to a slot overwrites what it held. `store`
    /// coarsens every such update into a store, so every load of a slot sees every store to it.
    /// A model's `modes: ["frame-store"]` or `["frame-update"]` picks one for one function.
    #[arg(
        long,
        value_name = "WRITES",
        default_value = "update",
        value_parser = parse_frame
    )]
    pub frame: ctadl_ir::ssa::FrameWrites,
}

/// The value of `--mem2reg`: `None` is `off`.
#[derive(Debug, Clone, Copy)]
pub struct Mem2RegFlag(Option<ctadl_ir::ssa::Mem2Reg>);

fn parse_mem2reg(s: &str) -> Result<Mem2RegFlag, String> {
    if s == "off" {
        return Ok(Mem2RegFlag(None));
    }
    ctadl_ir::ssa::Mem2Reg::parse(s)
        .map(|m| Mem2RegFlag(Some(m)))
        .ok_or_else(|| {
            format!("`{s}` is not a policy; use window:N, exact, above, full, full:N or off")
        })
}

fn parse_frame(s: &str) -> Result<ctadl_ir::ssa::FrameWrites, String> {
    ctadl_ir::ssa::FrameWrites::parse(s)
        .ok_or_else(|| format!("`{s}` is not a frame write; use store or update"))
}

/// Parses a size like `512M`, `1.5G` or `4096`: a number with an optional `K`, `M`, `G` or
/// `T` suffix (powers of 1024, any case, optionally followed by `B` or `iB`).
fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.trim();
    let digits = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(digits);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("`{s}` is not a size, like 512M or 4G"))?;
    let unit = unit.trim().to_ascii_uppercase();
    let unit = unit
        .strip_suffix("IB")
        .or_else(|| unit.strip_suffix('B'))
        .unwrap_or(&unit);
    let scale: u64 = match unit {
        "" => 1,
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        _ => return Err(format!("unknown size unit in `{s}`; use K, M, G or T")),
    };
    Ok((number * scale as f64) as usize)
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Analysis project (index) name
    pub name: String,

    /// The query to run, or load additional models from one or more files. Can be specified
    /// multiple times to load multiple model files. A `.ctadl` (or `.dl`) file is written in
    /// Portcullis; `.json`, `.json5` and `.jsonl` are the older model-generator format,
    /// deprecated, and migrated to Portcullis as they are loaded.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// Output file path (defaults to results.sarif)
    #[arg(long, short, default_value = "results.sarif")]
    pub output: PathBuf,

    /// SARIF profile
    #[arg(long, short, value_enum, default_value_t = SarifProfile::Human)]
    pub sarif_profile: SarifProfile,

    /// Dump the taint graph to a dot file
    #[arg(long)]
    pub dump_taint_graph: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct GoArgs {
    /// Analysis project (index) name. Inferred from the first artifact by default
    #[arg(long, short)]
    pub name: Option<String>,

    /// Load additional models from one or more files. Can be specified multiple times to load
    /// multiple model files. A `.ctadl` (or `.dl`) file is written in Portcullis; `.json`,
    /// `.json5` and `.jsonl` are the older model-generator format, deprecated, and migrated to
    /// Portcullis as they are loaded.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// Do not load the built-in default propagation models for the imported language.
    /// See `ctadl index --help`.
    #[arg(long)]
    pub no_default_models: bool,

    /// Do not link Java `native` methods to their native implementations.
    /// See `ctadl index --help`.
    #[arg(long)]
    pub no_jni_bridge: bool,

    /// Ignore the `RegisterNatives` tables recovered at import time.
    /// See `ctadl index --help`.
    #[arg(long)]
    pub no_jni_registry: bool,

    /// One or more artifacts to import in this one-shot flow
    #[arg(required = true)]
    pub artifacts: Vec<PathBuf>,

    /// Output file path (defaults to results.sarif)
    #[arg(long, short, default_value = "results.sarif")]
    pub output: PathBuf,

    /// SARIF profile
    #[arg(long, short, value_enum, default_value_t = SarifProfile::Human)]
    pub sarif_profile: SarifProfile,

    /// Dump the taint graph to a dot file
    #[arg(long)]
    pub dump_taint_graph: Option<PathBuf>,

    #[command(flatten)]
    pub call: CallPolicyArgs,

    /// Language/IR family for the artifact, or auto to detect it
    #[arg(long, short, value_enum, default_value_t = ImportLanguage::Auto)]
    pub language: ImportLanguage,

    /// Re-import artifacts even when an up-to-date import already exists.
    /// See `ctadl import --help`.
    #[arg(long)]
    pub force: bool,

    /// Do not import the native libraries packaged inside an APK.
    /// See `ctadl import --help`.
    #[arg(long)]
    pub no_native_libs: bool,

    /// Import an APK's native libraries for this ABI (e.g. armeabi-v7a).
    /// See `ctadl import --help`.
    #[arg(long, value_name = "ABI")]
    pub native_abi: Option<String>,

    /// Import a .NET directory's framework and BCL assemblies too.
    /// See `ctadl import --help`.
    #[arg(long)]
    pub include_framework: bool,

    /// Number of threads to import and index with. Unset, the import uses every core and the
    /// index runs serially, as `ctadl import` and `ctadl index` do by default.
    /// See `ctadl import --help` and `ctadl index --help`.
    #[arg(short = 'j', long, num_args = 0..=1, default_missing_value = "0", value_name = "N")]
    pub jobs: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    ctadl_ascent::init();
    let cli = Cli::parse();

    // Apply the global store override before any store interaction. This sets the
    // store root directly to the given directory (no `ctadl` subdirectory).
    if let Some(dir) = &cli.store {
        project::init_store_path(Some(dir))
            .map_err(|e| anyhow::anyhow!("failed to initialize store path: {}", e))?;
    }

    match &cli.cmd {
        Command::Import(args) => {
            import_artifact_to_store(args).with_context(|| {
                format!(
                    "importing artifact from: '{}'",
                    cli.cmd.import_artifact().display()
                )
            })?;
        }
        Command::Index(args) => {
            // If no programs are supplied, fall back to using the project name as the sole program.
            let effective_progs = if args.progs.is_empty() {
                vec![args.name.clone()]
            } else {
                args.progs.clone()
            };
            // Pass the original args; the indexing function will handle the fallback.
            index_artifacts_to_store(args)
                .with_context(|| format!("running 'index' artifacts: {:?}", effective_progs))?;
        }
        Command::Query(args) => {
            query_project(args)
                .with_context(|| format!("running 'query' project: {:?}", args.name))?;
        }
        Command::Inspect(args) => {
            inspect_artifact(args)
                .with_context(|| format!("running 'inspect' artifact: {:?}", args.name))?;
        }
        Command::Report(args) => {
            report_project(args)
                .with_context(|| format!("running 'report' project: {:?}", args.name))?;
        }
        Command::Go(args) => {
            // Use the user-provided name or one derived from the first artifact.
            let name = match &args.name {
                Some(n) => n.clone(),
                None => {
                    let first = &args.artifacts[0];
                    let inferred = project::artifact_name(first)?
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("error converting filename to string"))?
                        .to_string();
                    if args.artifacts.len() > 1 {
                        log::warn!(
                            "no project name given (-n); using '{}' inferred from the first artifact",
                            inferred
                        );
                    }
                    inferred
                }
            };

            let mut imported_names = Vec::new();
            for artifact in &args.artifacts {
                let import_args = ImportArgs {
                    artifact: artifact.clone(),
                    name: None,
                    language: args.language,
                    force: args.force,
                    no_native_libs: args.no_native_libs,
                    native_abi: args.native_abi.clone(),
                    include_framework: args.include_framework,
                    jobs: args.jobs.unwrap_or(0),
                };
                let name = import_artifact_to_store(&import_args).with_context(|| {
                    format!("importing artifact from: '{}'", artifact.display())
                })?;
                imported_names.push(name);
            }

            index_artifacts_to_store(&IndexArgs {
                name: name.clone(),
                progs: imported_names.clone(),
                summary: vec![],
                no_native_libs: false,
                models: args.models.clone(),
                no_default_models: args.no_default_models,
                no_jni_bridge: args.no_jni_bridge,
                no_jni_registry: args.no_jni_registry,
                call: args.call.clone(),
                prune_unreachable_cfg_nodes: None,
                alias_rule: None,
                hybrid_context: HybridContext::default(),
                context_join: ContextJoin::default(),
                jobs: args.jobs,
                lsm: false,
                lsm_memtable_limit: None,
                lsm_memory_budget: None,
                mem2reg: Mem2RegFlag(ctadl_ir::ssa::Pipeline::index_default().mem2reg),
                frame: ctadl_ir::ssa::Pipeline::index_default().frame,
            })
            .with_context(|| format!("running 'index' artifacts: {:?}", imported_names))?;

            query_project(&QueryArgs {
                name: name.clone(),
                models: args.models.clone(),
                output: args.output.clone(),
                sarif_profile: args.sarif_profile,
                dump_taint_graph: args.dump_taint_graph.clone(),
            })
            .with_context(|| format!("running 'query' project: {:?}", name))?;
        }
        Command::InitModel(args) => {
            handle_init_model(args).context("running 'init-model'")?;
        }
        Command::MigrateModels(args) => {
            handle_migrate_models(args).context("running 'migrate-models'")?;
        }
    };

    Ok(())
}

fn handle_migrate_models(args: &MigrateModelsArgs) -> anyhow::Result<()> {
    use ctadl_ascent::models::portcullis;

    for language in &args.languages {
        if ctadl_ascent::project::ArtifactLanguage::from_name(language).is_none() {
            anyhow::bail!("--language {language:?} is not a language ctadl imports");
        }
    }
    let languages: Vec<&str> = args.languages.iter().map(String::as_str).collect();
    let languages = (!languages.is_empty()).then_some(languages.as_slice());
    // A generator that cannot be translated exactly fails the whole file, `--dry-run` included:
    // writing the rest would silently drop it. Every such generator is listed, not just the
    // first, so one run is enough to see what needs fixing.
    let (text, report) = match portcullis::migrate::migrate_file(&args.input, languages) {
        Ok(migrated) => migrated,
        Err(ctadl_ascent::error::Error::JsonModel(errors)) => anyhow::bail!(
            "{} cannot be migrated exactly; nothing was written:\n{errors}",
            args.input.display()
        ),
        Err(e) => {
            return Err(e).with_context(|| format!("migrating {}", args.input.display()));
        }
    };

    // Loading it is part of migrating it. A file that translates but does not check is worse
    // than a failure: it would be discovered on the next index, against an artifact, with the
    // original no longer in play.
    portcullis::PortcullisFile::from_text(args.input.clone(), text.clone())
        .with_context(|| format!("the migration of {} does not load", args.input.display()))?;

    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    eprintln!(
        "{}: {} generator(s) -> {} rule(s)",
        args.input.display(),
        report.generators,
        report.rules
    );

    if args.dry_run {
        return Ok(());
    }
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.input.with_extension("ctadl"));
    if output == std::path::Path::new("-") {
        print!("{text}");
    } else {
        std::fs::write(&output, &text).with_context(|| format!("writing {}", output.display()))?;
        eprintln!("wrote {}", output.display());
    }
    Ok(())
}

/// What `ctadl init-model` writes: one source, one sink and one propagation to edit.
const MODEL_TEMPLATE: &str = r#"// A Portcullis model file. The language reference is docs/portcullis.md.
//
// Each rule derives its heads (left of ':-') for every way its body (right of ':-') matches.
// Pass this file to `ctadl query --models` for its sources and sinks, and to
// `ctadl index --models` for its propagations.

// A source: the return value of every method named `readData` is untrusted. `kind` is a label
// you choose; a flow is reported when a source's kind reaches a sink of the same kind.
source(F::return, kind = "input_data") :-
  fun(F, name = N), regex_match(N, "readData");

// A sink: the first argument of `executeQuery`. `name` is the simple name, so this selects every
// `executeQuery` in the program. To pin down one method, test its full name instead:
//   fun(F), F = "Lcom/example/Db;->executeQuery(Ljava/lang/String;)V"
// On an instance method arg(0) is the receiver, so the first argument is arg(1).
sink(F::arg(1), kind = "input_data") :-
  fun(F, name = "executeQuery");

// A propagation: `canonicalize_url` returns what its first argument carries. This is a summary
// for code CTADL cannot see, and takes effect at `ctadl index`.
propagation(F::return <- F::arg(0)) :-
  fun(F, name = "canonicalize_url");
"#;

fn handle_init_model(args: &InitModelArgs) -> anyhow::Result<()> {
    std::fs::write(&args.output, MODEL_TEMPLATE)
        .with_context(|| format!("writing template model file: '{}'", args.output.display()))?;
    log::info!("Wrote template model file to '{}'", args.output.display());
    Ok(())
}

/// Imports artifacts into the store.
///
/// # Errors
///
/// If there are any errors importing or writing to the store
fn import_artifact_to_store(args: &ImportArgs) -> anyhow::Result<String> {
    let path = &args.artifact;
    // Detect the language
    let language = {
        use project::ArtifactLanguage::*;
        match autodetect_import_language(path, args.language)? {
            ImportLanguage::Apk => Apk,
            ImportLanguage::Xapk => Xapk,
            ImportLanguage::Dex => Dex,
            ImportLanguage::Jar => Jar,
            ImportLanguage::Jvm => Jvm,
            ImportLanguage::C => C,
            ImportLanguage::Lua => Lua,
            ImportLanguage::Pcode => Pcode,
            ImportLanguage::Flowy => Flowy,
            ImportLanguage::Cil => Cil,
            ImportLanguage::Auto => unreachable!(),
        }
    };

    // Use the user-provided name or a derived artifact name.
    let name = match &args.name {
        None => project::artifact_name(path)?.as_os_str(),
        Some(n) => OsStr::new(n),
    }
    .to_str()
    .ok_or(anyhow::anyhow!("error converting filename to string"))?;

    // Unless forced, skip the import when an up-to-date one already exists: the
    // destination is present and the stored artifact path and content hash match.
    let skip_existing = !args.force;
    if skip_existing && project::ArtifactImport::is_up_to_date(name, path)? {
        log::info!(
            "skipping import '{}': destination exists and artifact hash matches",
            name
        );
        return Ok(name.to_string());
    }

    // Create the import
    let config = project::ArtifactImport::try_create(name, language, path)?;
    cli::import(
        &config,
        cli::ImportOptions {
            skip_existing,
            native_libs: !args.no_native_libs,
            native_abi: args.native_abi.as_deref(),
            cil_include_framework: args.include_framework,
            jobs: args.jobs,
        },
    )?;
    // Import succeeded: reload the config so we pick up any updates the import wrote
    // (e.g. the pcode importer records `image_base`), then record the artifact's
    // content hash (and path) so a later import can tell the import is up to date.
    let mut config = project::ArtifactImport::load_by_name(name)?;
    // A Ghidra Server repository can't be content-hashed (it's remote), so skip the
    // hash for it; such artifacts are simply re-imported each time.
    if !project::is_ghidra_server_url(&config.artifact_path) {
        config.record_artifact_hash()?;
    }
    Ok(name.to_string())
}

/// Index the named programs and store the index into the named project
///
/// # Errors
///
/// If there are any loading or writing errors
fn index_artifacts_to_store(args: &IndexArgs) -> anyhow::Result<()> {
    if args.lsm {
        log::warn!("--lsm is experimental");
    }
    if let Some(limit) = args.lsm_memtable_limit {
        lsm::ascent_provider::set_memtable_limit(limit);
    }
    lsm::set_memory_budget(args.lsm_memory_budget);
    // Determine the list of program names to index. If the user did not supply any, use the project name.
    let import_names: Vec<String> = if args.progs.is_empty() {
        vec![args.name.clone()]
    } else {
        args.progs.clone()
    };
    let sub_imports = if args.no_native_libs {
        project::SubImports::NoNativeLibs
    } else {
        project::SubImports::All
    };
    let project = project::AnalysisProject::try_create(&args.name, &import_names, sub_imports)?;
    cli::index(
        &project,
        &args.summary,
        &args.models,
        args.no_default_models,
        cli::IndexOptions {
            no_jni_bridge: args.no_jni_bridge,
            no_jni_registry: args.no_jni_registry,
            strategy: args.call.strategy,
            call_policy: args.call.call_policy(),
            prune_unreachable_cfg_nodes: args.prune_unreachable_cfg_nodes.unwrap_or(true),
            alias_rule: args.alias_rule.unwrap_or(true),
            hybrid_context: args.hybrid_context,
            context_join: args.context_join,
            parallelism: Parallelism::from_jobs(args.jobs.unwrap_or(1)),
            storage: if args.lsm {
                Storage::Lsm
            } else {
                Storage::Memory
            },
            mem2reg: args.mem2reg.0,
            frame: args.frame,
        },
    )?;
    Ok(())
}

fn query_project(args: &QueryArgs) -> anyhow::Result<()> {
    let project = load_or_infer_project(&args.name)?;
    let status = cli::query(
        &project,
        &args.models,
        &args.output,
        args.sarif_profile,
        args.dump_taint_graph.as_deref(),
    )?;
    // SARIF §3.58.6: a run carrying an error-level notification SHOULD exit non-zero. This
    // is deliberately after `cli::query` has written (and announced) the output file, since
    // that file is what explains the failure.
    if !status.execution_successful {
        if status.model_check_only {
            anyhow::bail!(
                "no index, so only the model files were checked; see '{}' for what they match, \
                 then run `ctadl index {}`",
                args.output.display(),
                args.name
            );
        }
        anyhow::bail!(
            "query produced no analyzable endpoints; see '{}' for details",
            args.output.display()
        );
    }
    Ok(())
}

fn report_project(args: &ReportArgs) -> anyhow::Result<()> {
    let project = load_or_infer_project(&args.name)?;
    cli::report(
        &project,
        &args.models,
        &args.output,
        ctadl_ascent::report::ReportOptions {
            format: args.format,
            top: args.top,
            recursion: !args.no_recursion,
            no_default_models: args.no_default_models,
            call_policy: CallPolicy {
                cha_threshold: args.cha_threshold,
                cha_threshold_interface: args.cha_threshold_interface.unwrap_or(args.cha_threshold),
                order: args.dispatch_order,
                ..CallPolicy::default()
            },
        },
    )?;
    Ok(())
}

/// The project `name` denotes, or the one an import of that name would be indexed into.
///
/// `ctadl index app` creates a project named `app` out of the import named `app`, so before it
/// has ever run there is no project to load -- and that is exactly when checking a model file
/// against the import is most useful. Falling back to [`project::AnalysisProject::ephemeral`] gives
/// `ctadl query` the same import list, with the same `sub_imports` expansion (an APK's native
/// libraries), and writes nothing to the store. The query itself then finds no index and says
/// so; see [`cli::query`].
fn load_or_infer_project(name: &str) -> anyhow::Result<project::AnalysisProject> {
    match project::AnalysisProject::try_load_name(name) {
        Ok(project) => Ok(project),
        Err(project_error) => match project::ArtifactImport::load_by_name(name) {
            Ok(_) => Ok(project::AnalysisProject::ephemeral(
                name,
                &[name],
                project::SubImports::All,
            )),
            Err(import_error) if project::ArtifactImport::exists_by_name(name) => {
                Err(import_error).with_context(|| format!("loading import '{name}'"))
            }
            // Neither a project nor an import: report the project error, which is what the
            // command was asked for.
            Err(_) => Err(project_error).with_context(|| format!("loading project: '{name}'")),
        },
    }
}

fn inspect_artifact(args: &InspectArgs) -> anyhow::Result<()> {
    if let Some(dot_path) = &args.dump_index_graph {
        let name = args
            .name
            .as_ref()
            .expect("--dump-index-graph parses only with a name");
        let project = project::AnalysisProject::try_load_name(name)
            .with_context(|| format!("loading project: '{name}'"))?;
        return cli::inspect_index_graph(&project, dot_path).map_err(Into::into);
    }

    if let Some(name) = &args.name {
        let path = Path::new(name);
        if path.exists() && path.is_file() {
            let abs_path = std::fs::canonicalize(path)?;
            let store_root = project::StorePaths::root();
            if abs_path.starts_with(store_root) {
                if let Some(ext) = path.extension()
                    && ext == "parquet"
                {
                    return cli::inspect_parquet(path).map_err(Into::into);
                }
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
                    && (file_name == project::PROGRAM_BITCODE_FILE
                        || file_name == project::VMT_BITCODE_FILE)
                {
                    return cli::inspect_bitcode(path).map_err(Into::into);
                }
                // The JNI registry is JSON, not bitcode, so it gets its own printer rather than
                // a third branch inside `inspect_bitcode`.
                if path.file_name().and_then(|n| n.to_str()) == Some(project::JNI_REGISTRY_FILE) {
                    return cli::inspect_jni_registry(path).map_err(Into::into);
                }
            }
        }

        let (import, unreadable_import) = match project::ArtifactImport::load_by_name(name) {
            Ok(import) => (Some(import), None),
            Err(err) if project::ArtifactImport::exists_by_name(name) => (None, Some(err)),
            Err(_) => (None, None),
        };
        let index = project::AnalysisProject::try_load_name(name).ok();

        if import.is_none() && unreadable_import.is_none() && index.is_none() {
            anyhow::bail!(
                "no imported artifact or index named '{name}'; run `ctadl inspect` with no \
                 name to list the store"
            );
        }

        if args.dump_ir {
            if let Some(err) = unreadable_import {
                return Err(err).with_context(|| format!("loading artifact import: '{name}'"));
            }
            let Some(import) = &import else {
                let project = index
                    .as_ref()
                    .expect("checked above: no import means an index");
                anyhow::bail!(
                    "--dump-ir needs an imported artifact, and '{name}' is an index; name one \
                     of its imports instead: {}",
                    project.imports.join(", ")
                );
            };
            return cli::dump_ir(import, args.function.as_deref()).map_err(Into::into);
        }

        if let Some(import) = &import {
            cli::inspect(import)?;
        }
        if let Some(project) = &index {
            // Separate the two sections when both of them are printed.
            if import.is_some() {
                println!();
            }
            cli::inspect_project(project)?;
        }
        if let Some(err) = unreadable_import {
            return Err(err).with_context(|| format!("loading artifact import: '{name}'"));
        }
    } else {
        cli::list_store_contents()?;
    }
    Ok(())
}

/// If language is 'auto', detects a language using extension, url scheme, or file type.
///
/// # Errors
///
/// If autodetection fails
fn autodetect_import_language<P: AsRef<Path>>(
    path: P,
    language: ImportLanguage,
) -> anyhow::Result<ImportLanguage> {
    let path = path.as_ref();
    Ok(match language {
        ImportLanguage::Auto => {
            // A Ghidra Server repository URL has no filename extension; recognize it
            // by scheme and route it through the pcode (Ghidra) frontend.
            if project::is_ghidra_server_url(path) {
                return Ok(ImportLanguage::Pcode);
            }
            // A directory of Lua sources is imported whole (the directory is the `require` root),
            // and has no extension to detect by; recognize it by containing `.lua` files.
            if path.is_dir() && dir_contains_lua(path) {
                return Ok(ImportLanguage::Lua);
            }
            // A published .NET app is a directory of assemblies.
            if path.is_dir() && ctadl_ascent::languages::cil::dir_has_cli_image(path) {
                return Ok(ImportLanguage::Cil);
            }

            let ext = path.extension().and_then(|e| OsStr::to_str(e));

            match ext {
                Some("dex") => ImportLanguage::Dex,
                Some("apk") => ImportLanguage::Apk,
                Some("xapk") => ImportLanguage::Xapk,
                Some("class") => ImportLanguage::Jvm,
                Some("jar") => ImportLanguage::Jar,
                Some("lua") => ImportLanguage::Lua,
                Some("tnt") => ImportLanguage::Flowy,
                // A PE file is .NET when it has CLI metadata, and native otherwise.
                Some("dll" | "exe") if ctadl_ascent::languages::cil::is_cli_image(path) => {
                    ImportLanguage::Cil
                }
                // A Ghidra project file: export pcode from the existing project.
                Some("gpr") => ImportLanguage::Pcode,
                // A C source file or header.
                Some("c") | Some("h") => ImportLanguage::C,
                // A directory with no recognized extension is treated as a tree
                // of C sources (headers and `.c` files).
                _ if path.is_dir() => ImportLanguage::C,
                // No recognized extension: if the file's contents look binary,
                // route it through the pcode (Ghidra) frontend.
                _ if file_looks_binary(path) => ImportLanguage::Pcode,
                Some(ext) => {
                    anyhow::bail!("unrecognized filename extension: '{}'", ext)
                }
                None => anyhow::bail!("no filename extension"),
            }
        }
        _ => language,
    })
}

/// Whether a directory tree contains any `.lua` file.
fn dir_contains_lua(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains_lua(&path) {
                return true;
            }
        } else if path.extension().and_then(|e| OsStr::to_str(e)) == Some("lua") {
            return true;
        }
    }
    false
}

/// Heuristically decides whether `path` refers to a binary file.
///
/// Reads a prefix of the file and treats it as binary if it contains a NUL
/// byte -- the same heuristic git uses to tell binary from text. Returns
/// `false` if `path` is not a readable regular file (e.g. a directory or a
/// missing path), so callers fall through to their other detection logic.
fn file_looks_binary(path: &Path) -> bool {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    // A directory opens successfully but is not a binary we can import.
    if file.metadata().map(|m| !m.is_file()).unwrap_or(true) {
        return false;
    }
    let mut buf = [0u8; 8192];
    match file.read(&mut buf) {
        Ok(n) => buf[..n].contains(&0),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `init-model` writes a file the loader accepts, so the first edit starts from one that works.
    #[test]
    fn the_model_template_loads() {
        let file = ctadl_ascent::models::portcullis::PortcullisFile::from_text(
            "model.ctadl",
            MODEL_TEMPLATE,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(file.program.rules.len(), 3);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("4g"), Ok(4 << 30));
        assert_eq!(parse_size("1.5GiB"), Ok(3 << 29));
        assert_eq!(parse_size("64 KB"), Ok(64 << 10));
        assert!(parse_size("4X").is_err());
        assert!(parse_size("lots").is_err());
    }

    /// The two dump modes read different things -- one an import's IR, the other a project's
    /// index -- so asking for both at once is a usage error, not a silent pick.
    #[test]
    fn inspect_rejects_dump_index_graph_with_dump_ir() {
        let err = Cli::try_parse_from([
            "ctadl",
            "inspect",
            "p",
            "--dump-index-graph",
            "g.dot",
            "--dump-ir",
        ])
        .expect_err("--dump-index-graph and --dump-ir must conflict")
        .to_string();
        assert!(
            err.contains("--dump-ir"),
            "the error must name the conflict: {err}"
        );
    }

    #[test]
    fn inspect_accepts_dump_index_graph_with_a_name() {
        let cli = Cli::try_parse_from(["ctadl", "inspect", "p", "--dump-index-graph", "g.dot"])
            .expect("must parse");
        let Command::Inspect(args) = cli.cmd else {
            panic!("expected the inspect subcommand");
        };
        assert_eq!(args.name.as_deref(), Some("p"));
        assert_eq!(args.dump_index_graph.as_deref(), Some(Path::new("g.dot")));
    }

    #[test]
    fn inspect_dump_index_graph_without_a_name_is_a_usage_error() {
        let err = Cli::try_parse_from(["ctadl", "inspect", "--dump-index-graph", "g.dot"])
            .expect_err("a dump with no project name must fail")
            .to_string();
        assert!(
            err.contains("NAME"),
            "the error must say what is missing: {err}"
        );
    }
}
