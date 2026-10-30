/*! The counters a model load reports besides its matches.

What the diagnostics read: `CTADL0004`'s per-generator endpoint counts and why a port matched
nothing, `CTADL0011`'s match samples, and the index-time constructs `ctadl query` says it is
ignoring. A DSL load fills them in through [`super::ModelLoadReport::from_dsl`].
*/

use std::collections::BTreeSet;

/// What one generator's `where` selected in one program: the exact count, and a sample of the
/// names capped by whoever asked for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchedFunctions {
    pub total: usize,
    pub names: BTreeSet<String>,
}

impl MatchedFunctions {
    /// Folds in the same generator's capture from another import, keeping at most `cap` names.
    /// The counts sum, because the imports' function sets are disjoint.
    pub fn merge(&mut self, other: &Self, cap: usize) {
        self.total += other.total;
        for name in &other.names {
            if self.names.len() >= cap {
                break;
            }
            self.names.insert(name.clone());
        }
    }
}

/// What Stage 1 did with one generator's `propagation` list.
///
/// The two ends of the same fan-out that [`EndpointStats`] records for source/sink ports: a
/// generator declares propagation *ports*, and each matched function turns one into a row.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PropagationStats {
    /// `propagation` entries whose input and output ports both parsed. A property of the model
    /// file alone, so [`Self::merge`] takes the max -- the same file is re-matched once per
    /// import.
    pub ports_declared: usize,
    /// [`PropagationMatch`](super::PropagationMatch) rows emitted for those entries. Summed by [`Self::merge`]: a
    /// generator dead against one import but live against another is live.
    pub rows: usize,
}

impl PropagationStats {
    pub fn merge(&mut self, other: &Self) {
        self.ports_declared = self.ports_declared.max(other.ports_declared);
        self.rows += other.rows;
    }
}

/// How many generators declared each index-time-only model construct.
///
/// A property of the model *file*, not of the program it was matched against, so
/// [`Self::merge`] takes the max rather than summing: the same file is re-matched once per
/// import, and summing would multiply the count by the import count.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IndexTimeModelCounts {
    /// Generators carrying a `propagation`.
    pub propagations: usize,
    /// Generators carrying a `bridge`.
    pub bridges: usize,
    /// Generators carrying a `modes` directive.
    pub modes: usize,
}

impl IndexTimeModelCounts {
    pub fn merge(&mut self, other: &Self) {
        self.propagations = self.propagations.max(other.propagations);
        self.bridges = self.bridges.max(other.bridges);
        self.modes = self.modes.max(other.modes);
    }

    pub fn is_empty(&self) -> bool {
        self.propagations == 0 && self.bridges == 0 && self.modes == 0
    }

    /// How to name what was ignored, for the one warning a query-time load emits.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.propagations > 0 {
            parts.push(format!("{} propagation model(s)", self.propagations));
        }
        if self.bridges > 0 {
            parts.push(format!("{} bridging model(s)", self.bridges));
        }
        if self.modes > 0 {
            parts.push(format!("{} analysis-mode directive(s)", self.modes));
        }
        parts.join(" and ")
    }
}

/// What Stage 1 did with one generator's port declarations in one direction.
///
/// `ports_declared` and `endpoints_matched` are the two ends of the same fan-out: a model
/// file declares *ports* (one `{"port": "Argument(0)"}` entry each), and each port matches
/// zero or more functions, every one of which becomes an endpoint row.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EndpointStats {
    /// Ports the generator declared in this direction: one per `sources`/`sinks` entry whose
    /// `port` parsed. A property of the model file alone, so [`Self::merge`] does *not* sum
    /// it — the same file is re-matched once per import.
    pub ports_declared: usize,
    /// Endpoint rows emitted for those ports, after fanning out over the matched functions.
    /// Summed by [`Self::merge`]: a generator dead against one import but live against
    /// another is live.
    pub endpoints_matched: usize,
    /// How many functions the generator's `where` constraints selected. Reported by
    /// `CTADL0004` to separate "nothing matched" from "functions matched but the port did
    /// not resolve in any of them".
    pub functions_matched: usize,
    /// Why nothing matched. Meaningful only when `endpoints_matched` is zero; a generator
    /// declaring several ports of one direction can fail for several reasons at once.
    pub unmatched: BTreeSet<UnmatchedReason>,
}

impl EndpointStats {
    /// Folds in the stats for the same (generator, direction) from another load of the same
    /// model file — one per import. Each field combines differently; see the field docs.
    pub fn merge(&mut self, other: &Self) {
        self.ports_declared = self.ports_declared.max(other.ports_declared);
        self.endpoints_matched += other.endpoints_matched;
        self.functions_matched = self.functions_matched.max(other.functions_matched);
        if self.endpoints_matched > 0 {
            self.unmatched.clear();
        } else {
            self.unmatched.extend(other.unmatched.iter().cloned());
        }
    }
}

/// Why a declared port produced no endpoint row.
///
/// The DSL engine reports only [`Self::NoFunctionMatched`] today. [`Self::LocalNotFound`] is for
/// `Variable(...)` ports, which the DSL is to gain a spelling for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnmatchedReason {
    /// The generator's `where` constraints selected no function in the program.
    NoFunctionMatched,
    /// Functions matched, but none of them declares a local by this name, so the
    /// `Variable(name)` port resolved in none of them.
    LocalNotFound(String),
}
