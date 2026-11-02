//! ART's observations from a passing `cargo xtask dex-opcodes` run, saved in the repo so that
//! dex-reader can be checked against them without a device.
//!
//! The file is `tests/dex/opcodes-art.txt`: a header naming the device, a fingerprint of the
//! harness (H.java and Driver.java) and of each case (its smali and manifest entry), then the
//! run's `results.txt` verbatim. A fingerprint that no longer matches means the case changed
//! after the run, so its observation no longer describes it and the run must be redone.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::cases::Case;
use super::{DRIVER_JAVA, class_name, h_java, manifest_for, smali_for};

/// The saved file, compiled in so a prebuilt xtask (the Nix check) has it without a source tree.
pub const SAVED: &str = include_str!("../../tests/dex/opcodes-art.txt");

/// Where `--save` writes the file.
pub fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/dex/opcodes-art.txt")
}

/// 64-bit FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn harness_fingerprint() -> String {
    fnv1a(&(h_java() + DRIVER_JAVA))
}

/// Each case's class and fingerprint, in run order.
fn case_fingerprints(all: &[Case]) -> Vec<(String, String)> {
    all.iter()
        .enumerate()
        .map(|(i, case)| {
            let class = class_name(i);
            let (smali, _) = smali_for(case, &class);
            let print = fnv1a(&(smali + &manifest_for(case, &class)));
            (class, print)
        })
        .collect()
}

/// The text `--save` writes for a run of `all` on `device` that produced `results`.
pub fn render(all: &[Case], device: &str, results: &str) -> String {
    let mut s = String::from(
        "# ART's observations of every `cargo xtask dex-opcodes` case. The `dex:opcodes-replay`\n\
         # check judges dex-reader against them. After changing a case, H.java or Driver.java,\n\
         # rerun on a device: `cargo xtask dex-opcodes --avd <name> --save`.\n",
    );
    s.push_str(&format!("device {device}\n"));
    s.push_str(&format!("harness {}\n", harness_fingerprint()));
    for (class, print) in case_fingerprints(all) {
        s.push_str(&format!("fingerprint {class} {print}\n"));
    }
    s.push_str(results);
    s
}

/// A parsed saved file.
pub struct Saved<'a> {
    pub device: &'a str,
    harness: &'a str,
    fingerprints: Vec<(&'a str, &'a str)>,
    /// The run's `results.txt`.
    pub results: &'a str,
}

pub fn parse(text: &str) -> Result<Saved<'_>> {
    let mut device = None;
    let mut harness = None;
    let mut fingerprints = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end();
        let (key, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        match key {
            "#" | "" => {}
            "device" => device = Some(rest),
            "harness" => harness = Some(rest),
            "fingerprint" => {
                fingerprints.push(rest.split_once(' ').context("bad fingerprint line")?)
            }
            _ => break,
        }
        offset += line.len();
    }
    Ok(Saved {
        device: device.context("saved observations name no device")?,
        harness: harness.context("saved observations have no harness fingerprint")?,
        fingerprints,
        results: &text[offset..],
    })
}

/// Fails, naming what changed, unless `saved` was made by the current harness from exactly
/// the cases in `all`.
pub fn check_current(saved: &Saved<'_>, all: &[Case]) -> Result<()> {
    let rerun = "rerun `cargo xtask dex-opcodes --avd <name> --save` on a device";
    if saved.harness != harness_fingerprint() {
        bail!("H.java or Driver.java changed since the observations were saved; {rerun}");
    }
    let current = case_fingerprints(all);
    let mut changed = Vec::new();
    for (i, (class, print)) in current.iter().enumerate() {
        match saved.fingerprints.get(i) {
            Some((c, p)) if c == class && p == print => {}
            _ => changed.push(format!("{class} ({})", all[i].name)),
        }
    }
    if saved.fingerprints.len() > current.len() {
        changed.push(format!(
            "{} saved cases no longer exist",
            saved.fingerprints.len() - current.len()
        ));
    }
    if !changed.is_empty() {
        bail!(
            "{} cases changed since the observations were saved: {}; {rerun}",
            changed.len(),
            changed.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Editing a case without rerunning it on ART leaves the saved observation describing a
    /// different program. Catch that at `cargo test` time rather than at the nightly replay.
    #[test]
    fn saved_observations_match_the_cases() {
        let saved = parse(SAVED).unwrap();
        check_current(&saved, &super::super::cases::all()).unwrap();
    }

    #[test]
    fn parse_splits_header_from_results() {
        let text = "# c\ndevice d\nharness h\nfingerprint Case_0 ab\ncase Case_0\nend\n";
        let saved = parse(text).unwrap();
        assert_eq!(saved.device, "d");
        assert_eq!(saved.fingerprints, vec![("Case_0", "ab")]);
        assert_eq!(saved.results, "case Case_0\nend\n");
    }
}
