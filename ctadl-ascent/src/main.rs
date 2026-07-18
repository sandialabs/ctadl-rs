use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project;
use ctadl_ascent::query_engine::formatter::SarifProfile;

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
    Query(QueryArgs),

    /// One-shot: import artifacts, index them under name, query, and format output
    Go(GoArgs),

    /// Generate a template JSON5 model file to help write custom analysis models. Analysis models
    /// are used to specify sources and sinks (see the 'query' command) as well as specifying
    /// external function behavior (see the 'index' command)
    InitModel(InitModelArgs),

    /// Inspect the CTADL store
    Inspect(InspectArgs),

    /// Legacy Ghidra Pcode CLI: index and query commands for Ghidra integration.
    #[command(name = "legacy-pcode-cli")]
    LegacyPcodeCli(LegacyPcodeCliArgs),
}

#[derive(Debug, Args)]
pub struct InitModelArgs {
    /// Path where the template model file will be written (defaults to model.json5)
    #[arg(default_value = "model.json5")]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct LegacyPcodeCliArgs {
    /// Directory where the index/store is located
    #[arg(long)]
    pub directory: Option<PathBuf>,

    #[command(subcommand)]
    pub cmd: LegacyPcodeSubcommand,

    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum LegacyPcodeSubcommand {
    /// Legacy index command compatible with Ghidra
    Index(LegacyIndexArgs),
    /// Legacy query command compatible with Ghidra
    Query(LegacyQueryArgs),
}

#[derive(Debug, Args)]
pub struct LegacyIndexArgs {
    /// Number of parallel jobs (ignored)
    #[arg(short = 'j', default_value_t = 8)]
    pub jobs: usize,

    /// Path to the directory containing pcode facts
    #[arg(short = 'f')]
    pub facts_path: PathBuf,
}

#[derive(Debug, Args)]
pub struct LegacyQueryArgs {
    /// Taint direction to compute slices for
    #[arg(long, value_name = "DIRECTION")]
    pub compute_slices: Option<LegacyTaintDirection>,

    /// Skip compiling analysis (ignored)
    #[arg(long)]
    pub no_compile_analysis: bool,

    /// Number of parallel jobs (ignored)
    #[arg(short = 'j', default_value_t = 8)]
    pub jobs: usize,

    /// Output format
    #[arg(long, default_value = "sarif")]
    pub format: String,

    /// Query file path
    pub query_file: PathBuf,
}

#[derive(Debug, Clone, ValueEnum, Copy)]
pub enum LegacyTaintDirection {
    All,
    Fwd,
    Bwd,
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

    /// Language/IR family for the artifact: jvm, dex, or auto
    #[arg(long, short, value_enum, default_value_t = ImportLanguage::Auto)]
    pub language: ImportLanguage,

    /// Skip the import if an import of the same name already exists whose stored
    /// artifact path and content hash match the artifact being imported. This
    /// avoids re-doing the (potentially expensive) translation when nothing has
    /// changed.
    #[arg(long)]
    pub skip_existing: bool,
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
    /// Treat as C files
    C,
    /// Export pcode via Ghidra. The artifact may be a binary to import, an existing
    /// local Ghidra project (`<name>.gpr` or its directory), or a Ghidra Server
    /// repository URL (`ghidra://…`).
    Pcode,
    /// Treat as Flowy file
    Flowy,
    /// Infer from extension/content
    Auto,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Artifact name, project name, or store path
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Name for the analysis project (index name)
    pub name: String,

    /// One or more imported program names to co-index (from import step). If none given, assumes
    /// the project name also refers to the import.
    pub progs: Vec<String>,

    /// Load summaries from one or more previously indexed projects and map them into the current project.
    /// The summaries will be filtered to only include functions that exist in the current project.
    /// Can be specified multiple times to load from multiple projects.
    #[arg(long, short, action = clap::ArgAction::Append, id = "NAME")]
    pub summary: Vec<String>,

    /// Load additional models from one or more JSON, JSON5, or JSONL files. Can be specified
    /// multiple times to load multiple model files. This option is use primarily to provide
    /// propagation models, which provide function summaries for indexing external or
    /// hard-to-analyze code.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// Call resolution strategy: cha, hi, mixed
    #[arg(long, value_enum, default_value_t = CallResolutionStrategy::Mixed)]
    pub strategy: CallResolutionStrategy,

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

    /// Dump the index graph to a dot file
    #[arg(long)]
    pub dump_index_graph: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Analysis project (index) name
    pub name: String,

    /// The query to run, or load additional models from one or more JSON, JSON5, or JSONL files.
    /// Can be specified multiple times to load multiple model files.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// Output as compact as possible (for the sarif extension)
    #[arg(long, short, action)]
    pub compact: bool,

    /// Output file path (defaults to results.sarif)
    #[arg(long, short, default_value = "results.sarif")]
    pub output: PathBuf,

    /// SARIF profile
    #[arg(long, value_enum, default_value_t = SarifProfile::Human)]
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

    /// Load additional models from one or more JSON, JSON5, or JSONL files. Can be specified
    /// multiple times to load multiple model files.
    #[arg(long, short, action = clap::ArgAction::Append)]
    pub models: Vec<PathBuf>,

    /// One or more artifacts to import in this one-shot flow
    #[arg(required = true)]
    pub artifacts: Vec<PathBuf>,

    /// Output as compact as possible (for the sarif extension)
    #[arg(long, short, action)]
    pub compact: bool,

    /// Output file path (defaults to results.sarif)
    #[arg(long, short, default_value = "results.sarif")]
    pub output: PathBuf,

    /// SARIF profile
    #[arg(long, value_enum, default_value_t = SarifProfile::Human)]
    pub sarif_profile: SarifProfile,

    /// Dump the taint graph to a dot file
    #[arg(long)]
    pub dump_taint_graph: Option<PathBuf>,

    /// Dump the index graph to a dot file
    #[arg(long)]
    pub dump_index_graph: Option<PathBuf>,

    /// Call resolution strategy: cha, hi, mixed
    #[arg(long, value_enum, default_value_t = CallResolutionStrategy::Mixed)]
    pub strategy: CallResolutionStrategy,

    /// Language/IR family for the artifact: jvm, dex, or auto
    #[arg(long, short, value_enum, default_value_t = ImportLanguage::Auto)]
    pub language: ImportLanguage,

    /// Skip importing an artifact when an import of the same name already exists
    /// whose stored artifact path and content hash match. Applies to the import
    /// step of this one-shot flow.
    #[arg(long)]
    pub skip_existing: bool,
}

fn main() -> anyhow::Result<()> {
    ctadl_ascent::init();
    let cli = Cli::parse();

    // Apply the global store override before any store interaction. This sets the
    // store root directly to the given directory (no `ctadl` subdirectory), the
    // same mechanism `legacy-pcode-cli --directory` uses.
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
                        eprintln!(
                            "Warning: no project name given (-n); using '{}' inferred from the first artifact",
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
                    skip_existing: args.skip_existing,
                };
                eprintln!("Importing '{}'...", artifact.display());
                let name = import_artifact_to_store(&import_args).with_context(|| {
                    format!("importing artifact from: '{}'", artifact.display())
                })?;
                imported_names.push(name);
            }

            eprintln!("Indexing...");
            index_artifacts_to_store(&IndexArgs {
                name: name.clone(),
                progs: imported_names.clone(),
                summary: vec![],
                models: args.models.clone(),
                strategy: args.strategy,
                prune_unreachable_cfg_nodes: None,
                alias_rule: None,
                dump_index_graph: args.dump_index_graph.clone(),
            })
            .with_context(|| format!("running 'index' artifacts: {:?}", imported_names))?;

            eprintln!("Querying...");
            query_project(&QueryArgs {
                name: name.clone(),
                models: args.models.clone(),
                compact: args.compact,
                output: args.output.clone(),
                sarif_profile: args.sarif_profile,
                dump_taint_graph: args.dump_taint_graph.clone(),
            })
            .with_context(|| format!("running 'query' project: {:?}", name))?;
        }
        Command::LegacyPcodeCli(args) => {
            handle_legacy_pcode_cli(args).context("running 'legacy-pcode-cli'")?;
        }
        Command::InitModel(args) => {
            handle_init_model(args).context("running 'init-model'")?;
        }
    };

    Ok(())
}

fn handle_init_model(args: &InitModelArgs) -> anyhow::Result<()> {
    let template = r#"{
    // Link to the schema to enable IDE features like autocomplete and hover documentation.
    // Adjust the path to match your installation if necessary.
    "$schema": "https://raw.githubusercontent.com/sandialabs/ctadl-rs/refs/heads/main/ctadl-ascent/src/models/ctadl-model-generator.schema.json",

    "model_generators": [
        {
            // Example 1: Define a data source using a signature pattern.
            // This will match any method containing 'readData' in its signature
            // and mark its return value as a source of taint.
            "find": "methods",
            "where": [
                {
                    "constraint": "signature_pattern",
                    "pattern": ".*readData.*"
                }
            ],
            "model": {
                "sources": [
                    {
                        "port": "Return",
                        "kind": "input_data"
                    }
                ]
            }
        },
        {
            // Example 2: Define a sink using an exact signature match.
            // This will match the exact method signature 'executeQuery' and mark its first argument
            // as a sink for taint analysis.
            "find": "methods",
            "where": [
                {
                    "constraint": "signature_match",
                    "name": "executeQuery"
                }
            ],
            "model": {
                "sinks": [
                    {
                        "port": "Argument(0)",
                        "kind": "sql_injection"
                    }
                ]
            }
        },
        {
            // Example 3: Define a propagation model.
            // This models a method 'canonicalize_url' that transforms data.
            // We model it as propagating taint from its first argument to its return value.
            "find": "methods",
            "where": [
                {
                    "constraint": "signature_match",
                    "name": "canonicalize_url"
                }
            ],
            "model": {
                "propagation": [
                    {
                        "input": "Argument(0)",
                        "output": "Return"
                    }
                ]
            }
        }
        ]
        }"#;

    std::fs::write(&args.output, template)?;
    eprintln!("Wrote template model file to '{}'", args.output.display());
    Ok(())
}

fn handle_legacy_pcode_cli(args: &LegacyPcodeCliArgs) -> anyhow::Result<()> {
    // Initialize the store path to the directory provided by Ghidra, if provided.
    if let Some(dir) = &args.directory {
        project::init_store_path(Some(dir))
            .map_err(|e| anyhow::anyhow!("failed to initialize store path: {}", e))?;
    }

    let legacy_name = "legacy_pcode";

    match &args.cmd {
        LegacyPcodeSubcommand::Index(index_args) => {
            eprintln!("Legacy Index: facts='{}'", index_args.facts_path.display());

            // 1. Import pcode facts
            let import_args = ImportArgs {
                artifact: index_args.facts_path.clone(),
                name: Some(legacy_name.to_string()),
                language: ImportLanguage::Pcode,
                skip_existing: false,
            };
            import_artifact_to_store(&import_args)?;

            // 2. Index the imported program
            let index_args = IndexArgs {
                name: legacy_name.to_string(),
                progs: vec![legacy_name.to_string()],
                summary: vec![],
                models: args.models.clone(),
                strategy: CallResolutionStrategy::Mixed,
                prune_unreachable_cfg_nodes: None,
                alias_rule: None,
                dump_index_graph: None,
            };
            index_artifacts_to_store(&index_args)?;
        }
        LegacyPcodeSubcommand::Query(query_args) => {
            eprintln!("Legacy Query: file='{}'", query_args.query_file.display());
            if let Some(dir) = query_args.compute_slices {
                eprintln!(
                    "  (Note: --compute-slices {:?} is currently ignored and controlled by the query file)",
                    dir
                );
            }
            if query_args.no_compile_analysis {
                eprintln!("  (Note: --no-compile-analysis is currently ignored)");
            }

            let mut models = args.models.clone();
            models.push(query_args.query_file.clone());
            // Run the query and format the output (compact=true for Ghidra).
            let q_args = QueryArgs {
                name: legacy_name.to_string(),
                models,
                compact: true,
                output: PathBuf::from("results.sarif"),
                sarif_profile: SarifProfile::Human,
                dump_taint_graph: None,
            };
            query_project(&q_args)?;
        }
    }

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
        match autodetect_by_extension(path, args.language)? {
            ImportLanguage::Apk => Apk,
            ImportLanguage::Dex => Dex,
            ImportLanguage::Jar => Jar,
            ImportLanguage::Jvm => Jvm,
            ImportLanguage::C => C,
            ImportLanguage::Pcode => Pcode,
            ImportLanguage::Flowy => Flowy,
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

    // If requested, skip the import when an up-to-date one already exists: the
    // destination is present and the stored artifact path and content hash match.
    if args.skip_existing && project::ArtifactImport::is_up_to_date(name, path)? {
        log::info!(
            "skipping import '{}': destination exists and artifact hash matches",
            name
        );
        return Ok(name.to_string());
    }

    // Create the import
    let config = project::ArtifactImport::try_create(name, language, path)?;
    cli::import(&config)?;
    // Import succeeded: reload the config so we pick up any updates the import wrote
    // (e.g. the pcode importer records `image_base`), then record the artifact's
    // content hash (and path) so a later `--skip-existing` import can tell the import
    // is up to date.
    let mut config = project::ArtifactImport::load_by_name(name)?;
    // A Ghidra Server repository can't be content-hashed (it's remote), so skip the
    // hash for it; `--skip-existing` simply re-imports such artifacts each time.
    if !project::is_ghidra_server_url(&config.artifact_path) {
        config.record_artifact_hash()?;
    }
    Ok(name.to_string())
}

/// Index the named programs and store the index into the named project
///
/// # Errors
///
/// If there ary any loading or writing errors
fn index_artifacts_to_store(args: &IndexArgs) -> anyhow::Result<()> {
    // Determine the list of program names to index. If the user did not supply any, use the project name.
    let import_names: Vec<String> = if args.progs.is_empty() {
        vec![args.name.clone()]
    } else {
        args.progs.clone()
    };
    let project = project::AnalysisProject::try_create(&args.name, &import_names)?;
    cli::index(
        &project,
        &args.summary,
        &args.models,
        args.strategy,
        args.prune_unreachable_cfg_nodes.unwrap_or(true),
        args.alias_rule.unwrap_or(true),
        args.dump_index_graph.as_deref(),
    )?;
    Ok(())
}

fn query_project(args: &QueryArgs) -> anyhow::Result<()> {
    let project = project::AnalysisProject::try_load_name(&args.name)
        .with_context(|| format!("loading project: '{}'", args.name))?;
    cli::query(
        &project,
        &args.models,
        args.compact,
        &args.output,
        args.sarif_profile,
        args.dump_taint_graph.as_deref(),
    )?;
    Ok(())
}

fn inspect_artifact(args: &InspectArgs) -> anyhow::Result<()> {
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
                    && (file_name == "ir-program.bitcode" || file_name == "ir-vmt.bitcode")
                {
                    return cli::inspect_bitcode(path).map_err(Into::into);
                }
            }
        }

        let import = project::ArtifactImport::load_by_name(name)
            .with_context(|| format!("loading artifact import: '{}'", name))?;
        cli::inspect(&import)?;
    } else {
        cli::list_store_contents()?;
    }
    Ok(())
}

/// If language is 'auto', returns the language using the extension. Otherwise just returns the
/// language.
///
/// # Errors
///
/// If autodetection finds no filename extension or doesn't recognize it
fn autodetect_by_extension<P: AsRef<Path>>(
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
            let ext = path.extension().and_then(|e| OsStr::to_str(e));

            match ext {
                Some("dex") => ImportLanguage::Dex,
                Some("apk") => ImportLanguage::Apk,
                Some("class") => ImportLanguage::Jvm,
                Some("jar") => ImportLanguage::Jar,
                Some("tnt") => ImportLanguage::Flowy,
                // A Ghidra project file: export pcode from the existing project.
                Some("gpr") => ImportLanguage::Pcode,
                // A C source file or header.
                Some("c") | Some("h") => ImportLanguage::C,
                _ => {
                    // A directory with no recognized extension is treated as a tree
                    // of C sources (headers and `.c` files).
                    if path.is_dir() {
                        ImportLanguage::C
                    } else if let Some(ext) = ext {
                        anyhow::bail!("unrecognized filename extension: '{}'", ext);
                    } else {
                        anyhow::bail!("no filename extension");
                    }
                }
            }
        }
        _ => language,
    })
}
