//! dex-reader regression checks, driven from source plus one real-world APK.
//!
//! Two kinds of input feed these checks:
//!
//!  - The same `jvm-reader/tests/sample/*.java` sources used by the jvm-reader
//!    checks, compiled here `javac --release 8` -> `dx` into a `.dex` and parsed
//!    in full by dex-reader. Nothing compiled is committed; the `.dex` is built
//!    fresh from source on every run.
//!  - A single committed real-world APK (`com.noto_54.apk`), which xtask owns
//!    under `xtask/tests/dex/`. It is third-party input that cannot be rebuilt
//!    from any source we hold, so it stays a binary fixture; its job is to prove
//!    dex-reader survives a real, large, multi-`classes*.dex` app.
//!
//! This is the port of the former `dex-reader/tests/integration_test.rs`
//! (`test_dex_files_do_not_crash`), restructured to report pass/fail rather than
//! panic and to drive freshly compiled inputs instead of a committed `.dex`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use dex_reader::collect_line_map_entries;
use dex_reader::parser::*;
use dex_reader::{APKParser, DexParser};

use crate::exec;
use crate::regression::Outcome;

/// A sample program compiled all the way to a DEX, held in memory.
struct SampleDex {
    name: String,
    bytes: Vec<u8>,
}

/// Compile the Java samples to DEX and run all dex-reader checks. Returns named
/// (case, outcome) pairs to fold into the regression report. The Java-derived
/// checks Skip if the JDK / `dx` toolchain is missing (mirrors the jvm Skip);
/// the APK check runs regardless, since it needs no external tools.
pub fn run_checks(
    java_samples: &Path,
    apk: Option<&Path>,
    work: &Path,
) -> Result<Vec<(String, Outcome)>> {
    let mut results: Vec<(String, Outcome)> = Vec::new();

    // The Java -> DEX path needs javac (compile) and dx (dexer). dx ships only
    // in the Android build-tools, so a plain `cargo xtask` checkout without the
    // SDK Skips rather than fails.
    let missing_tool = ["javac", "dx"]
        .into_iter()
        .find(|t| exec::which(t).is_none());
    if let Some(tool) = missing_tool {
        let why = format!("`{tool}` not on PATH");
        results.push(("dex:samples".to_string(), Outcome::Skip(why.clone())));
        results.push(("dex:line-map".to_string(), Outcome::Skip(why)));
    } else {
        let samples = compile_samples(java_samples, work)?;
        if samples.is_empty() {
            bail!("no .java samples found in {}", java_samples.display());
        }
        results.push(("dex:samples".to_string(), to_outcome(check_parse(&samples))));
        results.push((
            "dex:line-map".to_string(),
            to_outcome(check_line_map(&samples)),
        ));
    }

    // The real-world APK smoke test: parse every classes*.dex it carries. It is
    // pure-Rust, so it runs even when the JDK/dx toolchain is absent.
    match apk {
        Some(path) if path.exists() => {
            results.push(("dex:apk".to_string(), to_outcome(check_apk(path))));
        }
        Some(path) => {
            results.push((
                "dex:apk".to_string(),
                Outcome::Skip(format!("APK not found at {}", path.display())),
            ));
        }
        None => {
            results.push((
                "dex:apk".to_string(),
                Outcome::Skip("no --dex-apk provided".to_string()),
            ));
        }
    }

    Ok(results)
}

fn to_outcome(result: Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Pass,
        Err(err) => Outcome::Fail(format!("{err:#}")),
    }
}

// --- compilation ----------------------------------------------------------

fn compile_samples(java_samples: &Path, work: &Path) -> Result<Vec<SampleDex>> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(java_samples)
        .with_context(|| format!("reading {}", java_samples.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("java"))
        .collect();
    sources.sort();

    let mut samples = Vec::new();
    for java in sources {
        let name = java
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("bad sample name {}", java.display()))?
            .to_string();

        // Each sample compiles in its own dir so the `*.class` set handed to dx
        // is exactly this sample's classes and nothing else.
        let dir = work.join(&name);
        exec::fresh_dir(&dir)?;

        // Target Java 8 bytecode: dx (build-tools 30.0.2) predates and rejects
        // newer class-file versions.
        let mut javac = Command::new("javac");
        javac
            .current_dir(&dir)
            .args(["--release", "8", "-d", "."])
            .arg(&java);
        exec::run_checked(javac, "javac")?;

        let classes = class_files(&dir)?;
        if classes.is_empty() {
            bail!("javac produced no .class files for {name}");
        }

        let dex = dir.join(format!("{name}.dex"));
        let mut dx = Command::new("dx");
        dx.current_dir(&dir)
            .arg("--dex")
            // Some samples use interface `default`/`static` methods (Java 8),
            // which the legacy `dx` only accepts when told to target API 24+.
            .arg("--min-sdk-version=24")
            .arg(format!("--output={}", dex.display()));
        // dx derives the package path from the class-file path, so pass bare
        // names relative to the cwd rather than absolute paths.
        for class in &classes {
            dx.arg(class.file_name().context("class file has no name")?);
        }
        exec::run_checked(dx, "dx")?;

        let bytes = std::fs::read(&dex)
            .with_context(|| format!("reading compiled dex {}", dex.display()))?;
        samples.push(SampleDex { name, bytes });
    }
    Ok(samples)
}

/// Collect `.class` files anywhere under `dir` (samples are in the default
/// package, but be robust to nested output).
fn class_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut classes = Vec::new();
    collect_class_files(dir, &mut classes)?;
    classes.sort();
    Ok(classes)
}

fn collect_class_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_class_files(&path, out)?;
        } else if path.extension().and_then(|x| x.to_str()) == Some("class") {
            out.push(path);
        }
    }
    Ok(())
}

// --- checks ---------------------------------------------------------------

/// Every compiled sample parses end to end without error.
fn check_parse(samples: &[SampleDex]) -> Result<()> {
    for sample in samples {
        parse_dex_fully(&sample.name, &sample.bytes)
            .with_context(|| format!("parsing sample {}", sample.name))?;
    }
    Ok(())
}

/// The line map built from the compiled samples is non-empty and every entry
/// carries a DEX-style method id (`L...;->...`).
fn check_line_map(samples: &[SampleDex]) -> Result<()> {
    let mut saw_any = false;
    for sample in samples {
        let parser = DexParser::new(&sample.bytes)
            .map_err(|e| anyhow::anyhow!("DexParser::new for {}: {e}", sample.name))?;
        for entry in collect_line_map_entries(&parser) {
            saw_any = true;
            if !(entry.method.starts_with('L') && entry.method.contains(";->")) {
                bail!(
                    "line-map method id not DEX-style in {}: {}",
                    sample.name,
                    entry.method
                );
            }
        }
    }
    if !saw_any {
        bail!("no line-map entries across samples");
    }
    Ok(())
}

/// The committed real-world APK parses: every `classes*.dex` entry parses in
/// full without error.
fn check_apk(apk: &Path) -> Result<()> {
    let buffer = std::fs::read(apk).with_context(|| format!("reading {}", apk.display()))?;
    let apk_parser = APKParser::new(&buffer).map_err(|e| anyhow::anyhow!("parsing APK: {e}"))?;
    let mut entries = 0usize;
    for (name, parser) in apk_parser.dex_parsers_with_filenames() {
        entries += 1;
        let label = format!("{}:{}", apk.display(), name);
        parse_dex_fully(&label, parser.data)?;
    }
    if entries == 0 {
        bail!("APK {} contained no classes*.dex entries", apk.display());
    }
    Ok(())
}

/// Parse a DEX buffer end to end, exercising every table plus class data, code
/// items, and catch-handler lookups. This is the body of the former
/// `integration_test.rs::test_dex_buffer`, returning `Result` instead of
/// panicking.
fn parse_dex_fully(label: &str, buffer: &[u8]) -> Result<()> {
    let header = parse_dex_header(buffer).with_context(|| format!("header for {label}"))?;
    let map = parse_map_list(buffer, &header).with_context(|| format!("map list for {label}"))?;
    validate_map_against_header(&map, &header)
        .with_context(|| format!("validating map for {label}"))?;

    let _strings =
        parse_string_ids(buffer, &header).with_context(|| format!("string ids for {label}"))?;
    let _type_ids =
        parse_type_ids(buffer, &header).with_context(|| format!("type ids for {label}"))?;
    let _proto_ids =
        parse_proto_ids(buffer, &header).with_context(|| format!("proto ids for {label}"))?;
    let class_defs =
        parse_class_defs(buffer, &header).with_context(|| format!("class defs for {label}"))?;
    let _methods =
        parse_method_ids(buffer, &header).with_context(|| format!("method ids for {label}"))?;
    let _field_ids = parse_field_ids(buffer, header.field_ids_off, header.field_ids_size)
        .with_context(|| format!("field ids for {label}"))?;

    // Parse class data and code items, exercising catch-handler parsing/lookup.
    for class_def in &class_defs {
        let class_data = class_def
            .parse_class_data(buffer)
            .with_context(|| format!("class data for {label}"))?;
        for method in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            if let Some(code_item) = method
                .code(buffer)
                .with_context(|| format!("code item for {label}"))?
            {
                if let Some(ref handlers) = code_item.handlers {
                    // Handler lookup may legitimately return None; we only care
                    // that the lookup itself does not panic.
                    let _looked_up: BTreeSet<bool> = code_item
                        .tries
                        .iter()
                        .map(|t| handlers.get_by_off(t.handler_off).is_some())
                        .collect();
                }
            }
        }
    }
    Ok(())
}
