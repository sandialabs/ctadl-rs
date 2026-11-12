//! What the fixpoint did, in machine-readable form: every relation's tuple count and every SCC's
//! iterations and time, with each rule's time, and the heap bytes of the `locals` store and of
//! every interner. `IndexResult::try_save` writes it next to the index as `profile.json`, so runs
//! can be compared relation by relation (across commits, or against a run stopped by
//! `CTADL_INDEX_TIMEOUT_SECS`) without rerunning them with logging on.
//!
//! The counts and times come from the summaries ascent generates (`relation_sizes_summary`,
//! `scc_times_summary`), which are the only form in which ascent exposes them; this module
//! parses that text.

use std::collections::BTreeMap;

use serde::Serialize;

pub use immortal::Stats;

use crate::facts::{CallDispatchKey, PathId, SegmentId, Str};
use crate::index_engine::locals_trie::HeapReport;

pub const PROFILE_FILE: &str = "profile.json";

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexProfile {
    /// False when `CTADL_INDEX_TIMEOUT_SECS` stopped the run first; every number is then partial.
    pub reached_fixpoint: bool,
    pub timeout_secs: Option<u64>,
    /// Wall time of the fixpoint itself, seeding and extraction excluded.
    pub fixpoint_secs: f64,
    pub update_indices_secs: f64,
    /// Tuple count of every relation, by name.
    pub relations: BTreeMap<String, usize>,
    /// The bytes each LSM-stored relation spends per row on its tuple, by name; empty unless the
    /// run used `Storage::Lsm`.
    pub bytes_per_row: BTreeMap<String, usize>,
    pub sccs: Vec<SccProfile>,
    pub memory: MemoryProfile,
    /// How many times a rule asked the path set to split a path that is not in it. Each is a
    /// bug that drops rows, so this is 0.
    pub split_misses: usize,
    /// Each stage of a staged run, in the order they ended; empty for a monolithic run. The
    /// other numbers are then sums over the stages, with no `sccs`.
    pub stages: Vec<StageProfile>,
}

/// One run of one stage of a staged run: a batch, a round of a part of a split SCC, the global
/// frame or the intent stage.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq)]
pub struct StageProfile {
    /// The run's name: `batch-3`, `batch-3-round-2`, `frame` or `intent`.
    pub id: String,
    /// The footprint of the process that ran it, in MB, the whole process included: a batch's
    /// or the intent stage's largest, and the frame's at its end, in the scheduler.
    pub peak_mb: f64,
    /// The rows it wrote.
    pub rows: usize,
    pub secs: f64,
}

/// Where the index's bytes are, so a memory regression can be traced to a store.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryProfile {
    /// The `locals` store's heap report at the fixpoint. `None` under `Storage::Lsm`, whose
    /// store is not on the heap.
    pub locals: Option<HeapReport>,
    /// Entries and heap bytes of each store of interned values, by name. See
    /// [`MemoryProfile::read_interned`].
    pub interned: BTreeMap<&'static str, Stats>,
    /// The entries each interner that the fixpoint may only read gained while it ran. All 0
    /// when this process built one index at a time.
    pub fixpoint_interned: BTreeMap<String, usize>,
}

impl MemoryProfile {
    /// Reads the size of each `id_handle!` interner into `interned`, beside the frozen path set
    /// that the engine put there (`PathSet`, whose entries are its members). Interned values are
    /// never freed, so the sizes cover every index this process has built, up to the moment of
    /// the call.
    pub fn read_interned(&mut self) {
        self.interned.extend([
            ("Str", Str::stats()),
            ("CallDispatchKey", CallDispatchKey::stats()),
            ("Path", PathId::stats()),
            ("PathSegment", SegmentId::stats()),
        ]);
    }

    /// The entries of each interner that the fixpoint may not add to, by name.
    pub fn frozen_interner_entries() -> BTreeMap<&'static str, usize> {
        BTreeMap::from([
            ("Str", Str::stats().entries),
            ("CallDispatchKey", CallDispatchKey::stats().entries),
            ("Path", PathId::stats().entries),
        ])
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SccProfile {
    pub iterations: usize,
    pub secs: f64,
    pub rules: Vec<RuleProfile>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RuleProfile {
    /// The rule as ascent summarizes it, e.g. `reach <-- edge, reach`.
    pub rule: String,
    pub secs: f64,
}

impl IndexProfile {
    /// Writes the profile to `dir` as [`PROFILE_FILE`].
    pub fn save(&self, dir: &std::path::Path) -> Result<(), crate::error::Error> {
        use crate::error::ErrorContext;
        let path = dir.join(PROFILE_FILE);
        let file =
            std::fs::File::create(&path).err_context(|| format!("creating {}", path.display()))?;
        serde_json::to_writer_pretty(std::io::BufWriter::new(file), self)
            .err_context(|| format!("writing {}", path.display()))
    }

    /// `relation_sizes` and `scc_times` are the program's `relation_sizes_summary()` and
    /// `scc_times_summary()`. A relation held in a store of its own reads 0 in the first; pass its
    /// real count in `store_sizes`, which takes precedence.
    pub fn from_summaries(
        relation_sizes: &str,
        scc_times: &str,
        store_sizes: &[(&str, usize)],
    ) -> Self {
        let mut relations = parse_relation_sizes(relation_sizes);
        for (name, n) in store_sizes {
            relations.insert((*name).to_string(), *n);
        }
        let (update_indices_secs, sccs) = parse_scc_times(scc_times);
        IndexProfile {
            relations,
            sccs,
            update_indices_secs,
            ..Default::default()
        }
    }
}

/// Lines of the form `<relation> size: <n>`.
pub(crate) fn parse_relation_sizes(text: &str) -> BTreeMap<String, usize> {
    text.lines()
        .filter_map(|line| {
            let (name, n) = line.rsplit_once(" size: ")?;
            Some((name.trim().to_string(), n.trim().parse().ok()?))
        })
        .collect()
}

/// The layout ascent writes:
///
/// ```text
/// update_indices time: <dur>
/// scc <i>: iterations: <n>, time: <dur>
///   sum of rule times: <dur>
///   rule <summary>
///     time: <dur>
/// -----------------------------------------
/// ```
///
/// Durations are `Debug`-formatted. A rule summary may span lines; everything up to its `time:`
/// line belongs to it.
fn parse_scc_times(text: &str) -> (f64, Vec<SccProfile>) {
    let mut update_indices = 0.0;
    let mut sccs: Vec<SccProfile> = Vec::new();
    let mut rule: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(d) = trimmed.strip_prefix("update_indices time: ") {
            update_indices = parse_debug_duration(d).unwrap_or(0.0);
        } else if let Some(rest) = trimmed.strip_prefix("scc ") {
            let iterations = rest
                .split_once("iterations: ")
                .and_then(|(_, r)| r.split_once(','))
                .and_then(|(n, _)| n.trim().parse().ok())
                .unwrap_or(0);
            let secs = rest
                .rsplit_once("time: ")
                .and_then(|(_, d)| parse_debug_duration(d))
                .unwrap_or(0.0);
            sccs.push(SccProfile {
                iterations,
                secs,
                rules: Vec::new(),
            });
            rule = None;
        } else if let Some(summary) = trimmed.strip_prefix("rule ") {
            rule = Some(summary.to_string());
        } else if let Some(d) = trimmed.strip_prefix("time: ") {
            if let (Some(r), Some(scc)) = (rule.take(), sccs.last_mut()) {
                scc.rules.push(RuleProfile {
                    rule: r,
                    secs: parse_debug_duration(d).unwrap_or(0.0),
                });
            }
        } else if let Some(r) = rule.as_mut() {
            r.push('\n');
            r.push_str(trimmed);
        }
    }
    (update_indices, sccs)
}

/// Seconds from a `Duration`'s `Debug` form: `1.5s`, `250ms`, `3.2µs`, `10ns`.
fn parse_debug_duration(text: &str) -> Option<f64> {
    let text = text.trim();
    let (number, scale) = [("ns", 1e-9), ("µs", 1e-6), ("ms", 1e-3), ("s", 1.0)]
        .iter()
        .find_map(|(unit, scale)| text.strip_suffix(unit).map(|n| (n, *scale)))?;
    Some(number.parse::<f64>().ok()? * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_debug_duration("1.5s"), Some(1.5));
        assert_eq!(parse_debug_duration("250ms"), Some(0.25));
        assert!((parse_debug_duration("3.2µs").unwrap() - 3.2e-6).abs() < 1e-12);
        assert_eq!(parse_debug_duration("0ns"), Some(0.0));
        assert_eq!(parse_debug_duration("soon"), None);
    }

    #[test]
    fn memory_reports_each_interner() {
        Str::intern("memory_reports_each_interner");
        CallDispatchKey::c();
        crate::facts::Path::from_symbol_names(["memory_reports_each_interner"]);
        let mut p = IndexProfile::default();
        p.memory.read_interned();
        let json = serde_json::to_value(&p).unwrap();
        for name in ["Str", "CallDispatchKey", "Path"] {
            let s = &json["memory"]["interned"][name];
            assert!(s["entries"].as_u64().unwrap() >= 1, "{name}");
            assert!(s["bytes"].as_u64().unwrap() > 0, "{name}");
        }
        assert!(json["memory"]["locals"].is_null());
    }

    #[test]
    fn summaries() {
        let sizes = "edge size: 3\nreach size: 12\nassign_like size: 0\n";
        let times = "update_indices time: 2ms\n\
                     scc 0: iterations: 1, time: 1ms\n\
                     scc 1: iterations: 4, time: 1.5s\n  \
                     sum of rule times: 1.4s\n  \
                     rule reach <-- edge\n    time: 400ms\n  \
                     rule reach <-- reach,\n  edge\n    time: 1s\n\
                     -----------------------------------------\n";
        let p = IndexProfile::from_summaries(sizes, times, &[("assign_like", 7)]);
        assert_eq!(p.relations["reach"], 12);
        assert_eq!(p.relations["assign_like"], 7);
        assert_eq!(p.update_indices_secs, 0.002);
        assert_eq!(p.sccs.len(), 2);
        assert_eq!(p.sccs[1].iterations, 4);
        assert_eq!(p.sccs[1].secs, 1.5);
        assert_eq!(p.sccs[1].rules.len(), 2);
        assert_eq!(p.sccs[1].rules[0].rule, "reach <-- edge");
        assert_eq!(p.sccs[1].rules[1].rule, "reach <-- reach,\nedge");
        assert_eq!(p.sccs[1].rules[1].secs, 1.0);
    }
}
