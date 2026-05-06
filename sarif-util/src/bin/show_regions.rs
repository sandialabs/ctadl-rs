#![allow(non_snake_case)]
use clap::Parser;
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use url::Url;

/// Extract source‑code snippets described by SARIF region objects.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Opt {
    /// Base directory used to resolve *relative* artifact URIs.
    #[arg(short, long, default_value = ".")]
    basedir: PathBuf,

    /// One or more SARIF files (wildcards can be expanded by the shell).
    sarif_files: Vec<PathBuf>,
}

/* ----------  Minimal SARIF structs (only fields we need) ---------- */

#[derive(Debug, Deserialize)]
struct SarifLog {
    runs: Vec<Run>,
}

#[derive(Debug, Deserialize)]
struct Run {
    tool: Option<Tool>,
    results: Vec<ResultItem>,
}

#[derive(Debug, Deserialize)]
struct Tool {
    driver: Driver,
}
#[derive(Debug, Deserialize)]
struct Driver {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResultItem {
    ruleId: Option<String>,
    message: Option<Message>,
    locations: Option<Vec<Location>>,
}

#[derive(Debug, Deserialize)]
struct Message {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Location {
    physicalLocation: Option<PhysicalLocation>,
}

#[derive(Debug, Deserialize)]
struct PhysicalLocation {
    artifactLocation: Option<ArtifactLocation>,
    region: Option<Region>,
}

#[derive(Debug, Deserialize)]
struct ArtifactLocation {
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Region {
    startLine: Option<u64>,
    endLine: Option<u64>,
    startColumn: Option<u64>,
    endColumn: Option<u64>,
}

/* ---------------------------  Helpers  ---------------------------- */

/// Turn a SARIF `artifactLocation.uri` into an absolute `PathBuf`.
///
/// Handles plain file paths and `file:` URIs (percent‑decoded).
fn uri_to_path(uri: &str) -> io::Result<PathBuf> {
    // If the string contains no scheme we treat it as a plain path.
    if !uri.contains(':') && !uri.starts_with('/') {
        return Ok(PathBuf::from(uri));
    }

    // Parse as URL – map any error to std::io::Error so the signature stays simple.
    let parsed = Url::parse(uri).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    if parsed.scheme() != "file" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Unsupported URI scheme '{}'", parsed.scheme()),
        ));
    }

    // Decode percent escapes (`%20`, `%2F`, …).  Convert UTF‑8 errors to IoError.
    let decoded = percent_decode_str(parsed.path())
        .decode_utf8()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .into_owned();

    #[cfg(windows)]
    {
        // Windows file URIs look like "/C:/path". Strip the leading slash.
        let mut p = decoded;
        if p.starts_with('/') && p[1..].chars().nth(1) == Some(':') {
            p.remove(0);
        }
        Ok(PathBuf::from(p))
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from(decoded))
    }
}

/// Return a cached vector of lines (without line‑break characters).
fn get_file_lines<'a>(
    path: &Path,
    cache: &'a mut HashMap<PathBuf, Vec<String>>,
) -> io::Result<&'a [String]> {
    if !cache.contains_key(path) {
        let content = fs::read_to_string(path)?;
        // `lines()` removes both `\n` and trailing `\r`.
        let vec: Vec<String> = content.lines().map(|l| l.to_owned()).collect();
        cache.insert(path.to_path_buf(), vec);
    }
    Ok(cache.get(path).unwrap())
}

/// Slice a single line according to SARIF column numbers.
///
/// Columns are **1‑based inclusive** – this function converts them to Rust’s
/// 0‑based exclusive indexing.
fn slice_line(line: &str, start_col: u64, end_inclusive: u64) -> String {
    let chars: Vec<char> = line.chars().collect();

    let start_idx = (start_col - 1) as usize;
    let mut end_idx = end_inclusive as usize; // exclusive after conversion

    if start_idx >= chars.len() {
        return "".to_string(); // out of bounds → empty string
    }
    if end_idx > chars.len() {
        end_idx = chars.len();
    }

    chars[start_idx..end_idx].iter().collect()
}

/// Extract the text described by a SARIF `Region`.
///
/// * `lines` – source file split into lines **without** newline characters.
///
/// Returns exactly what appears in the file (including embedded new‑lines for multi‑line regions).
fn extract_region(lines: &[String], region: &Region) -> String {
    // The spec guarantees that `startLine` is present.
    let start_line = region.startLine.unwrap_or(1) as usize;
    let end_line = region.endLine.unwrap_or(start_line as u64) as usize;

    let max_line = lines.len();
    // Clamp to actual file size – if the SARIF data points past EOF we just stop.
    let s = start_line.min(max_line);
    let e = end_line.min(max_line);

    // Column defaults:
    //   startColumn → 1,
    //   endColumn   → line length + 1  (so the whole line is taken)
    let start_col = region.startColumn.unwrap_or(1);
    let end_col_opt = region.endColumn; // Option<u64>

    if s == e {
        // Single‑line case.
        let line = &lines[s - 1];
        let end_col = end_col_opt.unwrap_or(line.chars().count() as u64 + 1);
        slice_line(line, start_col, end_col)
    } else {
        // Multi‑line case.
        let first = {
            let l = &lines[s - 1];
            slice_line(l, start_col, l.chars().count() as u64 + 1) // to EOL
        };

        // Middle lines are taken verbatim.
        let middle: Vec<&String> = if e > s + 1 {
            lines[(s)..(e - 1)].iter().collect()
        } else {
            vec![]
        };

        // Last line – up to `endColumn` (or whole line).
        let last = {
            let l = &lines[e - 1];
            let end_col = end_col_opt.unwrap_or(l.chars().count() as u64 + 1);
            slice_line(l, 1, end_col)
        };

        std::iter::once(first)
            .chain(middle.into_iter().cloned())
            .chain(std::iter::once(last))
            .collect::<Vec<String>>()
            .join("\n")
    }
}

/* --------------------------  Core logic  --------------------------- */

/// Process a single SARIF file and write nicely formatted snippets to `out`.
///
/// Returns `true` if any error/warning was emitted (so the caller can exit
/// with a non‑zero status). All diagnostics are printed on stderr.
fn process_sarif(sarif_path: &Path, basedir: &Path, out: &mut dyn Write) -> bool {
    let mut had_error = false;

    // -----------------------------------------------------------------
    // Load and deserialize SARIF JSON (partial structs only).
    // -----------------------------------------------------------------
    let content = match fs::read_to_string(sarif_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Could not read {}: {}", sarif_path.display(), e);
            return true;
        }
    };
    let log: SarifLog = match serde_json::from_str(&content) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("❌ Invalid JSON in {}: {}", sarif_path.display(), e);
            return true;
        }
    };

    // Cache for source files so we only hit the filesystem once per file.
    let mut file_cache: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for run in log.runs.iter() {
        let tool_name = run
            .tool
            .as_ref()
            .and_then(|t| t.driver.name.as_deref())
            .unwrap_or("<unknown-tool>");

        for (res_idx, result) in run.results.iter().enumerate() {
            let rule_id = result
                .ruleId
                .clone()
                .unwrap_or_else(|| "<no-rule>".to_string());
            let message = result
                .message
                .as_ref()
                .and_then(|m| m.text.clone())
                .unwrap_or_default();

            let locations = match &result.locations {
                Some(l) => l,
                None => continue, // nothing to extract
            };

            for (loc_idx, location) in locations.iter().enumerate() {
                let phys = match &location.physicalLocation {
                    Some(p) => p,
                    None => continue,
                };
                let uri_opt = phys
                    .artifactLocation
                    .as_ref()
                    .and_then(|al| al.uri.as_deref());

                let region = match &phys.region {
                    Some(r) => r,
                    None => {
                        eprintln!(
                            "⚠️  Result {}:{} has no region – skipping",
                            res_idx, loc_idx
                        );
                        had_error = true;
                        continue;
                    }
                };

                // ---------------------------------------------------------
                // Resolve the source file path.
                // ---------------------------------------------------------
                let raw_uri = match uri_opt {
                    Some(u) => u,
                    None => {
                        eprintln!("⚠️  Missing artifact URI on result {}:{}", res_idx, loc_idx);
                        had_error = true;
                        continue;
                    }
                };

                let mut src_path = match uri_to_path(raw_uri) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("❌ Invalid URI '{}': {}", raw_uri, e);
                        had_error = true;
                        continue;
                    }
                };
                if !src_path.is_absolute() {
                    src_path = basedir.join(&src_path);
                }

                // Canonicalise (resolves `..` and symlinks). If it fails we keep the
                // non‑canonical version – the snippet extraction will still work.
                let src_path = src_path.canonicalize().unwrap_or_else(|_| src_path.clone());

                // ---------------------------------------------------------
                // Load file contents and extract the region.
                // ---------------------------------------------------------
                let lines = match get_file_lines(&src_path, &mut file_cache) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!(
                            "❌ Could not read source file '{}': {}",
                            src_path.display(),
                            e
                        );
                        had_error = true;
                        continue;
                    }
                };

                let snippet = extract_region(lines, region);

                // ---------------------------------------------------------
                // Pretty‑print a block for this location.
                // ---------------------------------------------------------
                let header = if message.is_empty() {
                    rule_id.clone()
                } else {
                    format!("{} – {}", rule_id, message)
                };

                // Build a compact “file Lx:Cy-Lz:Cw” string.
                let mut loc_str = format!("{}", src_path.display());
                let start_line = region.startLine.unwrap_or(0);
                let end_line = region.endLine.unwrap_or(start_line);
                let start_col = region.startColumn;
                let end_col = region.endColumn;

                loc_str.push_str(&format!(" L{start_line}"));
                if let Some(c) = start_col {
                    loc_str.push_str(&format!(":C{c}"));
                }

                if end_line != start_line || (end_col.is_some() && end_col != start_col) {
                    loc_str.push_str(&format!("-L{end_line}"));
                    if let Some(ec) = end_col {
                        loc_str.push_str(&format!(":C{ec}"));
                    }
                }

                writeln!(out).unwrap();
                writeln!(out, "=== {} ===", header).unwrap();
                writeln!(out, "Tool: {}", tool_name).unwrap();
                writeln!(out, "File: {}", loc_str).unwrap();
                writeln!(out, "{:-<72}", "").unwrap(); // visual separator
                writeln!(out, "{}\n{}", snippet, "-".repeat(72)).unwrap();
            } // locations loop
        } // results loop
    } // runs loop

    had_error
}

/* -----------------------------  Entry point  ----------------------------- */

fn main() {
    let opt = Opt::parse();

    let mut any_failure = false;
    for sarif_path in &opt.sarif_files {
        if !sarif_path.is_file() {
            eprintln!("❌ Not a file: {}", sarif_path.display());
            any_failure = true;
            continue;
        }

        let failure = process_sarif(sarif_path, &opt.basedir, &mut io::stdout().lock());
        any_failure |= failure;
    }

    std::process::exit(if any_failure { 1 } else { 0 });
}
