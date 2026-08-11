/*! [`ProgramModelMatches`]: everything a models file matched, whichever phase consumes it.

As the import loop streams IR past, each import is matched and only the *matches* are retained
here -- not the match indexes, and not the IR.

# Which phase consumes what

This is one structure spanning two phases, and each field is consumed by exactly one of them:

- [`ProgramModelMatches::propagations`] -- index time. Phase 2 of codegen
  ([`crate::codegen::model_matches`]) turns them into `facts.summary` rows.
- [`ProgramModelMatches::access_paths`] -- index time. Phase 2 seeds them into the initial
  indexer paths.
- [`ProgramModelMatches::bridges`] -- index time. Phase 2 pairs the two sides and emits the
  `call`/`actual_param`/`assign`/`formal_param` rows.
- [`ProgramModelMatches::endpoints`] -- query time. Stage 2
  ([`crate::query_engine::build_query_endpoints`]) resolves and expands them into
  `QueryEndpoint`s.

Each phase therefore ignores the other's half, and both say so rather than dropping them in
silence: `ctadl index` warns that the given files declare source/sink models it ignores, and
`ctadl query` warns that they declare propagation/bridging models it ignores (both in
`cli::mod`). That symmetry is deliberate -- each phase used to discard what the other consumes
without a word.

The alternative -- a second, sibling structure for the query-time half -- was considered and
rejected: one matcher pass produces both halves, and splitting its output across two
accumulators buys nothing but a second thing to thread and document. The cost is paid here, in
having to say which field belongs to which phase.

Nothing here is written to disk, stored in the VMT, or applied to the IR. Matches are a function
of (artifact x models files) and the import cache must stay a pure function of the artifact
alone, or `ctadl index --models a.json` would poison the next `ctadl index --models b.json`.

# Eager evaluation, late classification

The bridging semantics say that if the `from` side matches nothing, the `to` side "isn't even
attempted". That is *reporting* semantics, not evaluation order. Under streaming, side B's
import may arrive before side A's, so whether `from` matched is unknowable when B's VMT is in
hand. Both sides are therefore matched eagerly, per import, and accumulate here; pairing,
`on-unmatched` classification and warning emission all happen once, in phase 2, after the stream
ends.
*/

use std::collections::BTreeSet;

use crate::error::Error;
use crate::facts;
use crate::facts::TaintDirection;
use crate::models::FormalIndexTypeTag;
use crate::models::spec::{BridgeSpec, Severity};

use super::json::ModelGeneratorIngest;
use super::match_index::ProgramMatchIndex;

/// One end of a matched propagation model: the selector the port named, plus its access path,
/// resolved against nothing yet. `AnyArgument` is deliberately still a *tag* here -- expanding
/// it needs the arity over actual parameters and call sites across every import, which is not
/// known until phase 2.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ModelPort {
    pub tag: FormalIndexTypeTag,
    /// Set only for a positional `Argument(n)` port.
    pub index: Option<i16>,
    pub path: facts::Path,
}

/// One matched `propagation`: taint arriving at `src` appears at `dst` in `function`.
///
/// The function is named, not resolved to a [`crate::facts::FunctionId`]: a summary attaches to
/// any function that was so much as *called*, including ones with no IR body at all, and the id
/// map that would resolve it does not hold every import's functions until the loop is over.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PropagationMatch {
    pub function: facts::Str,
    pub dst: ModelPort,
    pub src: ModelPort,
}

/// One matched source or sink port: everything Stage 1 of source/sink matching knows, with the
/// names interned and the port's access path resolved.
///
/// Stage 2 ([`crate::query_engine::build_query_endpoints`]) turns these into
/// `QueryEndpoint`s, resolving names to [`crate::facts::FunctionId`]s and performing the two
/// index-dependent expansions (call-site fan-out and sink wildcard expansion). The split
/// exists because Stage 1 runs while each import's IR is in hand and Stage 2 needs the index,
/// which does not exist until every import has been codegen'd.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EndpointMatch {
    /// Name of the endpoint function. For a callsite-scoped endpoint this is the callee.
    pub function: facts::Str,
    /// Selector type tag for the endpoint variable.
    pub selector_ty: FormalIndexTypeTag,
    /// Formal index, when the selector carries one.
    pub index: Option<i16>,
    /// The port's access path.
    pub path: facts::Path,
    /// Taint label.
    pub label: facts::Str,
    /// Forward (source) or backward (sink).
    pub direction: TaintDirection,
    /// Sink-only: match any access-path extension of the port (see
    /// [`crate::facts::Path::is_extension_of`]). Always `false` for sources.
    pub wildcard: bool,
    /// Source-only: this source is *saturating* -- the seeded vertex is tainted and reading any
    /// subfield/offset off it is also tainted, recursively. Always `false` for sinks.
    pub saturating: bool,
    /// Callsite-scoped only: the containing (caller) function the endpoint's callsites must sit
    /// inside. `None` means "any caller". Ignored unless `callsite_scoped` is `true`.
    pub in_function: Option<facts::Str>,
    /// `true` when this endpoint is anchored at individual call sites of `function` (from
    /// `find: callsites`) rather than at the function itself.
    pub callsite_scoped: bool,
    /// Base `LocalIdx` for a `Variable(name)` ([`FormalIndexTypeTag::Local`]) port; `None` for
    /// every other selector.
    ///
    /// This field is why an endpoint has to be recorded at all rather than re-derived at query
    /// time. The name is resolved against the *matched function's* `locals` during Stage 1
    /// (`json.rs`), and it **cannot** be recovered later: Stage 2 sees only the
    /// post-`eliminate_dead_temps`/`coalesce_copies` graph, where the name may no longer exist.
    pub local_index: Option<u32>,
}

/// The accumulated matches of one bridge spec's two sides, across every import.
///
/// "Unmatched" means *not matched anywhere in the project*, so these sets union over imports
/// rather than being classified per import. A scope that admits no import at all leaves both
/// sets empty, which is the same condition and gets the same warning -- deliberately, rather
/// than a separate configuration-error category.
#[derive(Clone, Debug, Default)]
pub struct BridgeSideMatches {
    /// fq-names side A (the call side) matched.
    pub from: BTreeSet<facts::Str>,
    /// fq-names side B (the implementation side) matched.
    pub to: BTreeSet<facts::Str>,
}

impl BridgeSideMatches {
    /// The cross product of the two sides. There is no `cardinality` key: every matched A pairs
    /// with every matched B, and a pairing that is not singleton x singleton is reported through
    /// `on-ambiguous` rather than restricted.
    pub fn pairs(&self) -> impl Iterator<Item = (facts::Str, facts::Str)> + '_ {
        self.from
            .iter()
            .flat_map(|a| self.to.iter().map(move |b| (*a, *b)))
    }

    /// Singleton x singleton -- the unambiguous case `on-ambiguous` exists to distinguish.
    pub fn is_unique(&self) -> bool {
        self.from.len() == 1 && self.to.len() == 1
    }
}

/// Matches for every scanned bridge spec, in spec order.
#[derive(Clone, Debug, Default)]
pub struct BridgeMatches {
    sides: Vec<BridgeSideMatches>,
}

impl BridgeMatches {
    /// Sizes the table to the scanned specs. Call once, before the import loop.
    pub fn prepare(&mut self, specs: &[BridgeSpec]) {
        self.sides.resize_with(specs.len(), Default::default);
    }

    pub fn is_empty(&self) -> bool {
        self.sides.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sides.len()
    }

    /// One spec's accumulated sides.
    ///
    /// A spec with no entry -- because no import was ever observed against it -- reads as two
    /// empty sets, which is the same thing observing it and matching nothing would give. That
    /// keeps a project with zero imports on the ordinary "matched nothing" path rather than
    /// panicking.
    pub fn get(&self, spec_index: usize) -> &BridgeSideMatches {
        static EMPTY: BridgeSideMatches = BridgeSideMatches {
            from: BTreeSet::new(),
            to: BTreeSet::new(),
        };
        self.sides.get(spec_index).unwrap_or(&EMPTY)
    }

    /// Mutable access to one spec's accumulated sides. [`observe_import`] is the only writer in
    /// the pipeline; tests use it to build a fixture without running a match pass.
    pub fn side_mut(&mut self, spec_index: usize) -> &mut BridgeSideMatches {
        &mut self.sides[spec_index]
    }

    pub fn iter(&self) -> impl Iterator<Item = &BridgeSideMatches> {
        self.sides.iter()
    }
}

/// Everything a models file matched, accumulated across every (import x model file) pair.
///
/// Accumulation is append-only and its *order* is only required to be deterministic, not
/// specific: the SARIF writer sorts endpoints into `BTreeSet`s before writing anything, so
/// insertion order does not reach the output. The natural loop order (import outer, model file
/// inner, visit order within a file) is deterministic and is what accumulation gets for free;
/// the one rule is never to route it through unordered iteration.
#[derive(Clone, Debug, Default)]
pub struct ProgramModelMatches {
    /// Matched propagation models. Phase 2 turns these into `facts.summary` rows, which is what
    /// seeds `model_paths` -- so a propagation's paths keep the model-path bucket discipline
    /// that lets them concatenate with every program path.
    pub propagations: Vec<PropagationMatch>,
    /// Matched source/sink models. `ctadl query` runs Stage 2 over these; `ctadl index` ignores
    /// them and says so once (see `cli::index`'s "declare source/sink models" warning).
    ///
    /// A `Vec` and not a set: `CTADL0100` compares declared ports against *post-fan-out*
    /// endpoints, so deduplicating here would change a reported number. Two model files that
    /// match the same port on the same function legitimately contribute two entries.
    pub endpoints: Vec<EndpointMatch>,
    /// Access paths a *user* declared that occur nowhere in the IR, so nothing else would ever
    /// register them. Phase 2 seeds them into the initial indexer paths.
    ///
    /// This is the escape hatch for composition across a bridge: a pathful bridge composes with
    /// its callee's derived summary only where the summary's endpoints land exactly on the
    /// port's `to_path`, and the residues a deeper summary produces are fixpoint output that
    /// nothing can enumerate. A user who knows the callee's summary shape pre-declares them
    /// here. Flows needing residues nobody declared are dropped, silently -- that is the
    /// documented default, not an oversight.
    pub access_paths: BTreeSet<facts::Path>,
    /// Matched bridge sides, parallel to the scanned specs.
    pub bridges: BridgeMatches,
    /// Bridges the DSL engine already grounded: both sides are function names, so there is no
    /// spec to pair and nothing to diagnose. Phase 2 emits them through the same path as a
    /// paired [`BridgeSpec`].
    pub resolved_bridges: Vec<crate::models::spec::ResolvedBridge>,
}

impl ProgramModelMatches {
    /// Folds one load's matched propagation models in.
    pub fn extend_propagations(
        &mut self,
        propagations: impl IntoIterator<Item = PropagationMatch>,
    ) {
        self.propagations.extend(propagations);
    }

    /// Folds in the access paths one model file declared.
    pub fn extend_access_paths(&mut self, paths: impl IntoIterator<Item = facts::Path>) {
        self.access_paths.extend(paths);
    }

    /// Whether anything was matched at all.
    pub fn is_empty(&self) -> bool {
        self.propagations.is_empty()
            && self.endpoints.is_empty()
            && self.access_paths.is_empty()
            && self.bridges.is_empty()
            && self.resolved_bridges.is_empty()
    }
}

/// Evaluates both sides of every bridge spec against one import, accumulating the matches.
///
/// Called per import, while its [`ProgramMatchIndex`] is in hand and before the IR is dropped.
/// Both sides are attempted eagerly; see the module docs for why the conditional `to`-side
/// semantics cannot be applied here.
pub fn observe_import(
    index: &ProgramMatchIndex<'_>,
    specs: &[BridgeSpec],
    matches: &mut ProgramModelMatches,
) -> Result<(), Error> {
    if specs.is_empty() {
        return Ok(());
    }
    matches.bridges.prepare(specs);
    // The ingest emits into `matches`, so it holds that borrow for as long as it lives and
    // `matches.bridges` cannot be written through it. Collect each spec's matched names while
    // the ingest is alive, release it, then fold. `match_where` already returns an owned
    // `Vec<String>` per side, so this allocates nothing the old shape did not.
    let mut matched: Vec<(Vec<String>, Vec<String>)> = Vec::with_capacity(specs.len());
    let result = {
        let mut ingest = ModelGeneratorIngest::new(index, matches);
        for spec in specs {
            let mut from = Vec::new();
            let mut to = Vec::new();
            if spec.from.scope.admits(&index.scope) {
                from = ingest.match_where(spec.index, &spec.from.where_);
                log::trace!(
                    "bridge {} from side matched {} function(s) in {}",
                    spec.provenance(),
                    from.len(),
                    index.scope.describe()
                );
            }
            if spec.to.scope.admits(&index.scope) {
                to = ingest.match_where(spec.index, &spec.to.where_);
                log::trace!(
                    "bridge {} to side matched {} function(s) in {}",
                    spec.provenance(),
                    to.len(),
                    index.scope.describe()
                );
            }
            matched.push((from, to));
        }
        // A malformed constraint on either side is a hard error, exactly as it is anywhere else
        // in the loader. The bridge's shape was validated before the loop; this catches what
        // only the evaluator can see.
        ingest.take_errors()
    };
    // Folded in even when the evaluation errored: an errored side yields no matches, and the
    // sides that did match were already recorded before the error under the old shape.
    for (i, (from, to)) in matched.into_iter().enumerate() {
        let side = matches.bridges.side_mut(i);
        side.from
            .extend(from.iter().map(|f| facts::Str::from(f.as_str())));
        side.to
            .extend(to.iter().map(|f| facts::Str::from(f.as_str())));
    }
    result
}

/// The verdict on one bridge spec's accumulated sides: `on-unmatched` per side, then
/// `on-ambiguous`.
///
/// Returns what to *say*, not the saying of it. Evaluation was eager -- both sides are matched
/// per import as imports stream by -- because side B's import may arrive before side A's, so
/// whether `from` matched is unknowable at match time. The conditionality is applied here, once,
/// over the whole project: "unmatched" means *not matched anywhere*, not "not matched in this
/// import".
///
/// This reads nothing but the spec and the two match sets, which is what lets `ctadl query`
/// render the same verdict with no index in existence (see `cli::model_check`). Phase 2 of codegen wraps it
/// in [`crate::codegen::model_matches::classify`], which is the only caller that acts on the
/// severity.
pub(crate) fn diagnose(spec: &BridgeSpec, side: &BridgeSideMatches) -> Option<(Severity, String)> {
    let where_ = spec.provenance();
    if side.from.is_empty() {
        // A scope admitting no import in the project lands here too, which is deliberate: it is
        // the same observable condition (nothing matched) and gets the same warning, rather
        // than a separate configuration-error category.
        return Some((
            spec.from.on_unmatched,
            format!(
                "bridge {where_}: the 'from' side matched no function in this project, so \
                 nothing is bridged. Check the 'where' constraints and the 'in' scope."
            ),
        ));
        // Note there is no `to`-side report here. "If the from side doesn't match anything, the
        // to side isn't even attempted" is exactly this: reporting semantics, not evaluation
        // order.
    }
    if side.to.is_empty() {
        return Some((
            spec.to.on_unmatched,
            format!(
                "bridge {where_}: the 'from' side matched {} function(s) ({}) but the 'to' side \
                 matched none, so nothing is bridged. Set 'on-unmatched': 'ignore' on the 'to' \
                 block if the implementation is legitimately absent.",
                side.from.len(),
                sample(&side.from, 3)
            ),
        ));
    }
    if !side.is_unique() {
        return Some((
            spec.on_ambiguous,
            format!(
                "bridge {where_}: matched {} 'from' function(s) ({}) and {} 'to' function(s) \
                 ({}); every combination is bridged, for {} pair(s). Add \
                 'on-ambiguous': 'ignore' to silence this, or narrow the 'where' constraints.",
                side.from.len(),
                sample(&side.from, 3),
                side.to.len(),
                sample(&side.to, 3),
                side.from.len() * side.to.len()
            ),
        ));
    }
    None
}

/// The functions a set of matches names, for a diagnostic that has to list a few of them.
pub(crate) fn sample(names: &BTreeSet<facts::Str>, limit: usize) -> String {
    let shown: Vec<&str> = names.iter().take(limit).map(|s| s.as_str()).collect();
    if names.len() > limit {
        format!("{}, … and {} more", shown.join(", "), names.len() - limit)
    } else {
        shown.join(", ")
    }
}
