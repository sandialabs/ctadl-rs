//! What each stage run did, which a run writes beside its outputs and the scheduler sums into
//! the run's [`IndexStats`] and `profile.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorContext};
use crate::index_engine::IndexStats;
use crate::index_engine::profile::{IndexProfile, StageProfile};
use crate::staged::store::{StageDir, write_atomic};

/// What one stage run did: its [`StageProfile`] and, for a run of the index rules, the numbers
/// of its [`IndexStats`] that add up over runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StageStats {
    pub stage: StageProfile,
    pub relations: BTreeMap<String, usize>,
    pub fixpoint_secs: f64,
    pub update_indices_secs: f64,
    pub split_misses: usize,
    pub initial_assign: usize,
    pub final_assign_like: usize,
    pub initial_formals: usize,
    pub final_locals: usize,
    pub final_resolved_call: usize,
    pub initial_summary: usize,
    pub final_summary: usize,
    pub num_functions: usize,
    pub num_variables: usize,
    pub reached_variables: usize,
}

impl StageStats {
    /// The stats of a run of the index rules, `stage`, whose engine reported `stats`.
    pub fn of_engine(stage: StageProfile, stats: &IndexStats) -> Self {
        let IndexStats {
            profile,
            initial_assign,
            final_assign_like,
            initial_formals,
            final_locals,
            final_resolved_call,
            initial_summary,
            final_summary,
            num_functions,
            num_variables,
            reached_variables,
        } = stats;
        StageStats {
            stage,
            relations: profile.relations.clone(),
            fixpoint_secs: profile.fixpoint_secs,
            update_indices_secs: profile.update_indices_secs,
            split_misses: profile.split_misses,
            initial_assign: *initial_assign,
            final_assign_like: *final_assign_like,
            initial_formals: *initial_formals,
            final_locals: *final_locals,
            final_resolved_call: *final_resolved_call,
            initial_summary: *initial_summary,
            final_summary: *final_summary,
            num_functions: *num_functions,
            num_variables: *num_variables,
            reached_variables: *reached_variables,
        }
    }

    /// The stats of a stage that runs no index rules.
    pub fn of_stage(stage: StageProfile) -> Self {
        StageStats {
            stage,
            ..Default::default()
        }
    }

    /// Writes these stats to the stage directory, replacing those of an earlier run of the same
    /// name.
    pub fn save(&self, stage: &StageDir) -> Result<(), Error> {
        let path = stats_file(stage, &self.stage.id);
        write_atomic(&path, |tmp| {
            let json = serde_json::to_vec_pretty(self).map_err(|e| Error::Staged(e.to_string()))?;
            std::fs::write(tmp, json)
                .map_err(Error::from)
                .err_context(|| format!("writing {}", tmp.display()))
        })
    }

    /// Every run's stats in the stage directory, in the order they were written. Runs of an
    /// earlier, resumed run are included; a killed run wrote none.
    pub fn load_all(stage: &StageDir) -> Result<Vec<StageStats>, Error> {
        let dir = stage.root().join(STATS_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error::from(e)).err_context(|| format!("reading {}", dir.display()));
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(Error::from)?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                let modified = entry.metadata().and_then(|m| m.modified()).ok();
                files.push((modified, path));
            }
        }
        files.sort();
        files
            .into_iter()
            .map(|(_, path)| {
                let text = std::fs::read(&path)
                    .map_err(Error::from)
                    .err_context(|| format!("reading {}", path.display()))?;
                serde_json::from_slice(&text)
                    .map_err(|e| Error::Staged(format!("{} does not read: {e}", path.display())))
            })
            .collect()
    }
}

/// The run's [`IndexStats`]: every number summed over `runs`, and each run as a stage of the
/// profile. Its memory profile is left empty.
pub fn sum(runs: Vec<StageStats>) -> IndexStats {
    let mut stats = IndexStats {
        profile: IndexProfile {
            reached_fixpoint: true,
            ..Default::default()
        },
        ..Default::default()
    };
    for run in runs {
        let profile = &mut stats.profile;
        for (name, n) in run.relations {
            *profile.relations.entry(name).or_default() += n;
        }
        profile.fixpoint_secs += run.fixpoint_secs;
        profile.update_indices_secs += run.update_indices_secs;
        profile.split_misses += run.split_misses;
        profile.stages.push(run.stage);
        stats.initial_assign += run.initial_assign;
        stats.final_assign_like += run.final_assign_like;
        stats.initial_formals += run.initial_formals;
        stats.final_locals += run.final_locals;
        stats.final_resolved_call += run.final_resolved_call;
        stats.initial_summary += run.initial_summary;
        stats.final_summary += run.final_summary;
        stats.num_functions += run.num_functions;
        stats.num_variables += run.num_variables;
        stats.reached_variables += run.reached_variables;
    }
    stats
}

/// The directory of the stage directory that holds each run's stats.
const STATS_DIR: &str = "stats";

fn stats_file(stage: &StageDir, id: &str) -> PathBuf {
    stage.root().join(STATS_DIR).join(format!("{id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_sum_over_runs() {
        let dir = tempfile::tempdir().unwrap();
        let stage = StageDir::new(dir.path());
        let run = |id: &str, locals: usize| StageStats {
            stage: StageProfile {
                id: id.to_string(),
                peak_mb: 100.0,
                rows: 3,
                secs: 0.5,
            },
            relations: BTreeMap::from([("locals".to_string(), locals)]),
            final_locals: locals,
            fixpoint_secs: 1.0,
            ..Default::default()
        };
        run("batch-0", 2).save(&stage).unwrap();
        run("batch-1", 5).save(&stage).unwrap();
        StageStats::of_stage(StageProfile {
            id: "frame".to_string(),
            ..Default::default()
        })
        .save(&stage)
        .unwrap();
        let runs = StageStats::load_all(&stage).unwrap();
        assert_eq!(runs.len(), 3);
        let stats = sum(runs);
        assert_eq!(stats.final_locals, 7);
        assert_eq!(stats.profile.relations["locals"], 7);
        assert_eq!(stats.profile.fixpoint_secs, 2.0);
        let mut ids: Vec<_> = stats.profile.stages.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["batch-0", "batch-1", "frame"]);
    }
}
