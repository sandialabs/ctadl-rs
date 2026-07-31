use crate::error::Error;
use flate2::Compression;
use flate2::write::GzEncoder;
use rayon::prelude::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAXMEM: &str = "40G";
const LAUNCH_MODE: &str = "fg";
const VMARG_LIST: &str = "-XX:ParallelGCThreads=4 -XX:CICompilerCount=4 ";

/// The Ghidra program source that [`ExportPcode.java`](../../../../pcode-reader/ExportPcode.java)
/// runs against.
///
/// Ghidra's `analyzeHeadless` can either *import* a fresh binary into a throwaway
/// project or *process* program(s) that already live in a project. Both drive the
/// exact same post-script, so the facts are identical regardless of how the program
/// got into Ghidra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhidraSource {
    /// Import a binary from disk into a throwaway project, analyze it, then export
    /// (the default when the artifact is an ordinary file).
    Binary(PathBuf),
    /// Export from program(s) already present in an existing *local* Ghidra project.
    /// `location` is the directory holding `<name>.gpr`/`<name>.rep`. `program`
    /// optionally restricts processing to a single project file (an exact name or a
    /// `*`/`?` wildcard); `None` processes every program in the project's root folder.
    Project {
        location: PathBuf,
        name: String,
        program: Option<String>,
    },
    /// Export from program(s) in a Ghidra Server repository, addressed by a
    /// `ghidra://<host>[:<port>]/<repository>[/<folder>]` URL. `program` behaves as
    /// for [`Self::Project`].
    Server {
        url: String,
        program: Option<String>,
    },
}

impl GhidraSource {
    /// Classify an import artifact into a Ghidra source:
    ///
    /// * a `ghidra://…` URL → [`Self::Server`],
    /// * a `*.gpr` project file, or a directory containing exactly one, → [`Self::Project`],
    /// * anything else (an ordinary executable) → [`Self::Binary`].
    pub fn detect(artifact: &Path) -> Result<Self, Error> {
        let s = artifact.to_string_lossy();
        if s.starts_with("ghidra://") {
            return Ok(GhidraSource::Server {
                url: s.into_owned(),
                program: None,
            });
        }
        if let Some(project) = detect_local_project(artifact)? {
            return Ok(project);
        }
        Ok(GhidraSource::Binary(artifact.to_path_buf()))
    }
}

/// Returns `Some(GhidraSource::Project { .. })` when `artifact` names an existing
/// local Ghidra project: either a `*.gpr` file directly, or a directory that
/// contains exactly one `*.gpr` file. Returns `None` otherwise.
fn detect_local_project(artifact: &Path) -> Result<Option<GhidraSource>, Error> {
    // A `.gpr` file names the project directly.
    if artifact.extension().and_then(|e| e.to_str()) == Some("gpr") {
        return Ok(Some(project_from_gpr(artifact)?));
    }
    // A directory that holds exactly one `<name>.gpr` is a project too.
    if artifact.is_dir() {
        let mut gprs = fs::read_dir(artifact)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("gpr"));
        if let Some(first) = gprs.next()
            && gprs.next().is_none()
        {
            return Ok(Some(project_from_gpr(&first)?));
        }
    }
    Ok(None)
}

/// Splits a `<location>/<name>.gpr` path into the project location and name that
/// `analyzeHeadless` expects.
fn project_from_gpr(gpr: &Path) -> Result<GhidraSource, Error> {
    let location = gpr
        .parent()
        .ok_or_else(|| {
            Error::PcodeConversion(format!(
                "Ghidra project file has no parent directory: {}",
                gpr.display()
            ))
        })?
        .to_path_buf();
    let name = gpr
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            Error::PcodeConversion(format!(
                "Ghidra project file has no usable name: {}",
                gpr.display()
            ))
        })?
        .to_string();
    Ok(GhidraSource::Project {
        location,
        name,
        program: None,
    })
}

/// Runs the pcode exporter against `artifact_path`, auto-detecting whether it is a
/// binary to import, an existing local Ghidra project, or a Ghidra Server URL. See
/// [`GhidraSource::detect`].
pub fn run_ghidra_export(artifact_path: &Path, output_dir: &Path) -> Result<(), Error> {
    let source = GhidraSource::detect(artifact_path)?;
    run_ghidra_export_source(&source, output_dir)
}

/// Runs the pcode exporter against an explicit [`GhidraSource`]. Binaries are
/// imported into a throwaway project (which is deleted afterwards); existing
/// projects and server repositories are opened read-only via `-process`, so the
/// user's data is never modified.
pub fn run_ghidra_export_source(source: &GhidraSource, output_dir: &Path) -> Result<(), Error> {
    // Benchmark/dev escape hatch: reuse already-exported pcode facts and skip the (slow) Ghidra
    // run, so re-import only re-runs the facts→IR conversion (which is what changes when the
    // frontend lowering changes). Enabled by `CTADL_REUSE_FACTS` when the facts dir is non-empty.
    let facts_dir = output_dir.join("facts");
    if std::env::var_os("CTADL_REUSE_FACTS").is_some()
        && std::fs::read_dir(&facts_dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        log::info!(
            "CTADL_REUSE_FACTS set: reusing cached pcode facts in {}",
            facts_dir.display()
        );
        return Ok(());
    }

    let ghidra_base = find_ghidra_base()?;
    let analyze_headless = find_analyze_headless(&ghidra_base)?;
    let script_dir = analyze_headless.parent().ok_or_else(|| {
        Error::PcodeConversion("Could not find script directory for Ghidra".to_string())
    })?;
    let launch_script = script_dir.join("launch.sh");

    let facts_dir = output_dir.join("facts");
    fs::create_dir_all(&facts_dir)?;

    // Write ExportPcode.java to a temporary directory
    let script_temp_dir = tempfile::Builder::new().prefix("ctadl-ghidra").tempdir()?;
    let export_script_path = script_temp_dir.path().join("ExportPcode.java");
    fs::write(
        &export_script_path,
        include_str!("../../../../pcode-reader/ExportPcode.java"),
    )?;
    let script_path = export_script_path.parent().unwrap().to_path_buf();

    // A throwaway project directory is only needed to `-import` a binary; bind it
    // here so it outlives the command invocation below. Use a dot-free temp-dir
    // prefix: Ghidra 12 rejects project-path elements that start with '.', and
    // tempfile's default prefix is ".tmp".
    let temp_project_dir = match source {
        GhidraSource::Binary(_) => Some(tempfile::Builder::new().prefix("ctadl-ghidra").tempdir()?),
        _ => None,
    };

    let mut command = Command::new(&launch_script);
    command.args([
        LAUNCH_MODE,
        "jdk",
        "Ghidra-Headless",
        MAXMEM,
        VMARG_LIST,
        "ghidra.app.util.headless.AnalyzeHeadless",
    ]);

    // Project addressing plus import-vs-process differs per source; the post-script
    // invocation that follows is identical for all of them.
    match source {
        GhidraSource::Binary(artifact) => {
            let project_dir = temp_project_dir.as_ref().unwrap();
            command
                .arg(project_dir.path())
                .arg("headless")
                .arg("-import")
                .arg(artifact)
                .arg("-deleteProject");
        }
        GhidraSource::Project {
            location,
            name,
            program,
        } => {
            command.arg(location).arg(name).arg("-process");
            if let Some(program) = program {
                command.arg(program);
            }
            // Never mutate the user's project.
            command.arg("-readOnly");
        }
        GhidraSource::Server { url, program } => {
            command.arg(url).arg("-process");
            if let Some(program) = program {
                command.arg(program);
            }
            // Never mutate the server repository.
            command.arg("-readOnly");
        }
    }

    command
        .arg("-postScript")
        .arg("ExportPcode.java")
        .arg(&facts_dir)
        .arg("-scriptPath")
        .arg(&script_path);

    log::info!("Running Ghidra: {:?}", command);

    let status = command.status()?;

    if !status.success() {
        return Err(Error::PcodeConversion(format!(
            "Ghidra analyzeHeadless failed with status: {}",
            status
        )));
    }

    // Ghidra's analyzeHeadless exits 0 even when the import itself fails (e.g. no
    // load spec for the artifact), leaving the facts directory empty. Detect that
    // here and propagate it, otherwise the failure only surfaces later as a
    // confusing "missing fact file" error while reading the (absent) facts.
    if fs::read_dir(&facts_dir)?.next().is_none() {
        return Err(Error::PcodeConversion(format!(
            "Ghidra produced no pcode facts in {} — check the Ghidra output above for an import error",
            facts_dir.display()
        )));
    }

    Ok(())
}

/// Gzip every `*.facts` file in `facts_dir` in place, replacing it with a
/// `*.facts.gz` sibling and removing the original.
///
/// Ghidra emits ~43 large tab-separated fact tables; compressing them keeps the
/// store small. Files already ending in `.facts.gz` are skipped, which makes this
/// idempotent and safe to re-run on the `CTADL_REUSE_FACTS` reuse path. Each file
/// is written to a `.tmp` sibling and renamed over the final `.gz`, so an
/// interrupted run never leaves a truncated `.gz` behind.
pub fn compress_facts_dir(facts_dir: &Path) -> Result<(), Error> {
    let entries: Vec<PathBuf> = fs::read_dir(facts_dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str());
            matches!(name, Some(name) if name.ends_with(".facts") && !name.ends_with(".facts.gz"))
        })
        .collect();

    entries
        .par_iter()
        .try_for_each(|path| compress_one_fact_file(path))
}

/// Gzip a single `*.facts` file into a `*.facts.gz` sibling (via a `.tmp` +
/// rename) and remove the original.
fn compress_one_fact_file(path: &Path) -> Result<(), Error> {
    let gz_path = {
        let mut name = path.as_os_str().to_owned();
        name.push(".gz");
        PathBuf::from(name)
    };
    let tmp_path = {
        let mut name = gz_path.as_os_str().to_owned();
        name.push(".tmp");
        PathBuf::from(name)
    };

    let mut input = fs::File::open(path)?;
    let output = fs::File::create(&tmp_path)?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    std::io::copy(&mut input, &mut encoder)?;
    encoder.finish()?;

    fs::rename(&tmp_path, &gz_path)?;
    fs::remove_file(path)?;

    Ok(())
}

/// True when a Ghidra install can be located and it has an `analyzeHeadless`.
///
/// Purely a probe: it touches nothing and starts nothing. A caller that would
/// otherwise do expensive setup (extracting native libraries out of an APK, say)
/// asks first, so a machine without Ghidra degrades to a warning instead of doing
/// the work and then failing.
pub(crate) fn available() -> bool {
    find_ghidra_base()
        .and_then(|base| find_analyze_headless(&base))
        .is_ok()
}

fn find_ghidra_base() -> Result<PathBuf, Error> {
    if let Ok(ghidra_home) = env::var("GHIDRA_HOME") {
        return Ok(PathBuf::from(ghidra_home));
    }

    if let Ok(ghidra_bin) = which::which("ghidra")
        && let Ok(ghidra_bin) = ghidra_bin.canonicalize()
        && let Some(parent) = ghidra_bin.parent()
    {
        return Ok(parent.to_path_buf());
    }

    Err(Error::PcodeConversion(
        "Could not find Ghidra. Set GHIDRA_HOME or add 'ghidra' to PATH.".to_string(),
    ))
}

fn find_analyze_headless(ghidra_base: &Path) -> Result<PathBuf, Error> {
    let candidates = [
        ghidra_base
            .parent()
            .map(|p| p.join("lib/ghidra/support/analyzeHeadless")),
        Some(ghidra_base.join("support/analyzeHeadless")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(Error::PcodeConversion(format!(
        "Could not find Ghidra analyzeHeadless from ghidra directory {}",
        ghidra_base.display()
    )))
}

// No more need for find_export_script

#[cfg(test)]
mod tests {
    use super::*;

    // --- Source detection (fast; no Ghidra required) ---------------------------
    //
    // The end-to-end test that actually builds a Ghidra project and exports pcode
    // from it lives in the xtask regression suite (`cargo xtask regression`, case
    // `Pcode:GhidraProject`), which runs under Nix where Ghidra and a `tee` binary
    // are guaranteed to be present.

    #[test]
    fn detect_ordinary_file_is_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("prog");
        fs::write(&bin, b"\x7fELF").unwrap();
        assert_eq!(
            GhidraSource::detect(&bin).unwrap(),
            GhidraSource::Binary(bin)
        );
    }

    #[test]
    fn detect_gpr_file_is_project() {
        let dir = tempfile::tempdir().unwrap();
        let gpr = dir.path().join("myproj.gpr");
        fs::write(&gpr, b"").unwrap();
        assert_eq!(
            GhidraSource::detect(&gpr).unwrap(),
            GhidraSource::Project {
                location: dir.path().to_path_buf(),
                name: "myproj".to_string(),
                program: None,
            }
        );
    }

    #[test]
    fn detect_project_directory_with_single_gpr() {
        let dir = tempfile::tempdir().unwrap();
        // A real project also has a `<name>.rep` directory next to the `.gpr`.
        fs::write(dir.path().join("myproj.gpr"), b"").unwrap();
        fs::create_dir(dir.path().join("myproj.rep")).unwrap();
        assert_eq!(
            GhidraSource::detect(dir.path()).unwrap(),
            GhidraSource::Project {
                location: dir.path().to_path_buf(),
                name: "myproj".to_string(),
                program: None,
            }
        );
    }

    #[test]
    fn detect_directory_without_gpr_is_binary() {
        // A directory of, say, sources is not a Ghidra project.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.c"), b"").unwrap();
        assert_eq!(
            GhidraSource::detect(dir.path()).unwrap(),
            GhidraSource::Binary(dir.path().to_path_buf())
        );
    }

    #[test]
    fn detect_directory_with_two_gprs_is_not_a_project() {
        // Ambiguous: two projects side by side. Don't guess -- treat as a plain path.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("one.gpr"), b"").unwrap();
        fs::write(dir.path().join("two.gpr"), b"").unwrap();
        assert!(matches!(
            GhidraSource::detect(dir.path()).unwrap(),
            GhidraSource::Binary(_)
        ));
    }

    #[test]
    fn detect_server_url() {
        let url = Path::new("ghidra://example.com:13100/myrepo/folder");
        assert_eq!(
            GhidraSource::detect(url).unwrap(),
            GhidraSource::Server {
                url: "ghidra://example.com:13100/myrepo/folder".to_string(),
                program: None,
            }
        );
    }

    // --- Fact compression ------------------------------------------------------

    fn gunzip(path: &Path) -> Vec<u8> {
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(fs::File::open(path).unwrap());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn compress_replaces_facts_with_gz_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("PCODE_INPUT.facts");
        let content = b"a\t1\nb\t2\n";
        fs::write(&facts, content).unwrap();

        compress_facts_dir(dir.path()).unwrap();

        let gz = dir.path().join("PCODE_INPUT.facts.gz");
        assert!(!facts.exists(), "original .facts should be removed");
        assert!(gz.exists(), ".facts.gz should be created");
        assert_eq!(gunzip(&gz), content);
        // No leftover temp file from the write-then-rename.
        assert!(!dir.path().join("PCODE_INPUT.facts.gz.tmp").exists());
    }

    #[test]
    fn compress_skips_already_compressed_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        // A pre-existing `.gz` (e.g. from an earlier run) must be left untouched so
        // re-running on the reuse path is a no-op.
        let gz = dir.path().join("DONE.facts.gz");
        let gz_bytes = {
            let mut e = GzEncoder::new(Vec::new(), Compression::default());
            std::io::Write::write_all(&mut e, b"already\tcompressed\n").unwrap();
            e.finish().unwrap()
        };
        fs::write(&gz, &gz_bytes).unwrap();

        // A fresh `.facts` alongside it should get compressed.
        let facts = dir.path().join("FRESH.facts");
        fs::write(&facts, b"x\t9\n").unwrap();

        compress_facts_dir(dir.path()).unwrap();
        // Untouched pre-existing gz.
        assert_eq!(fs::read(&gz).unwrap(), gz_bytes);
        assert_eq!(gunzip(&dir.path().join("FRESH.facts.gz")), b"x\t9\n");

        // Second run has nothing to do and leaves everything as-is.
        compress_facts_dir(dir.path()).unwrap();
        assert_eq!(fs::read(&gz).unwrap(), gz_bytes);
        assert_eq!(gunzip(&dir.path().join("FRESH.facts.gz")), b"x\t9\n");
    }

    #[test]
    fn compress_leaves_non_facts_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), b"hi").unwrap();
        compress_facts_dir(dir.path()).unwrap();
        assert!(dir.path().join("notes.txt").exists());
        assert!(!dir.path().join("notes.txt.gz").exists());
    }
}
