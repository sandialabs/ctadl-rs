/*! [`ProgramModelMatches`]: everything a models file matched, whichever phase consumes it.

As the import loop streams IR past, each import is matched and only the *matches* are retained
here -- not the match indexes, and not the IR.

# Which phase consumes what

This is one structure spanning two phases, and each field is consumed by exactly one of them:

- [`ProgramModelMatches::propagations`] -- index time. Phase 2 of codegen
  ([`crate::codegen::model_matches`]) turns them into `facts.summary` rows.
- [`ProgramModelMatches::access_paths`] -- index time. Phase 2 seeds them into the initial
  indexer paths.
- [`ProgramModelMatches::skip_analysis`] -- index time, and read by phase *1*:
  [`crate::codegen::codegen_program`] lowers those functions' signatures and drops their bodies,
  so nothing about them ever enters the fact base.
- [`ProgramModelMatches::resolved_bridges`] -- index time. Phase 2 emits the
  `call`/`actual_param`/`assign`/`formal_param` rows for each pair.
- [`ProgramModelMatches::dispatch`] -- index time, and also read by phase *1*:
  codegen classifies every Java and CIL call site against it before it emits anything for the
  site, and phase 2 writes the matched summaries against the synthetic functions phase 1 named.
- [`ProgramModelMatches::closure_shaped`] -- report time. A note about what hybrid inlining is
  for; nothing resolves differently for being on it.
- [`ProgramModelMatches::endpoints`] -- query time. Stage 2
  ([`crate::query_engine::build_query_endpoints`]) resolves and expands them into
  `QueryEndpoint`s. Phase 1 also *reads* them, without analysing them, to refuse a dispatch
  model whose CHA targets hold one.

Each phase therefore ignores the other's half, and both say so rather than dropping them in
silence: `ctadl index` warns that the given files declare source/sink models it ignores, and
`ctadl query` warns that they declare propagation/bridging models it ignores (both in
`cli::mod`).

The alternative -- a second, sibling structure for the query-time half -- was considered and
rejected: one engine pass produces both halves, and splitting its output across two
accumulators buys nothing but a second thing to thread and document. The cost is paid here, in
having to say which field belongs to which phase.

Nothing here is written to disk, stored in the VMT, or applied to the IR. Matches are a function
of (artifact x models files) and the import cache must stay a pure function of the artifact
alone, or `ctadl index --models a.json` would poison the next `ctadl index --models b.json`.

# Bridges are paired after the stream

A bridge's two sides may live in two imports, and side B's may arrive first. The DSL engine
accumulates each side of a bridging rule as the imports go past and pairs them once the loop
ends (see [`super::portcullis`]), so what lands here is already a pair of function names.
*/

use std::collections::BTreeSet;

use crate::facts;
use crate::facts::TaintDirection;
use crate::models::FormalIndexTypeTag;

/// One end of a matched propagation model: the selector the port named, plus its access path,
/// resolved against nothing yet. `AnyArgument` is deliberately still a *tag* here -- expanding
/// it needs the arity over actual parameters and call sites across every import, which is not
/// known until phase 2.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ModelPort {
    pub tag: FormalIndexTypeTag,
    /// Set only for a positional `Argument(n)` port.
    pub index: Option<i16>,
    pub path: facts::Path,
}

/// A call site's static signature: the `(cls, simple_name, descriptor)` triple a
/// `CallStyle::JavaCall` or `CallStyle::CilCall` carries, which is what a `find: "dispatch"`
/// generator matches.
pub type DispatchKey = (facts::Str, facts::Str, facts::Str);

/// What a matched `find: "dispatch"` generator says to do with every site of one signature.
///
/// Precedence when several generators match one key is `Inline > Model > Skip`: the one that
/// keeps the most of the program reachable. So a user overrides a shipped skip by adding a
/// propagation, and a shipped model by adding `resolve: inline`; going the other way needs
/// `--no-default-models`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// Hybrid inlining regardless of the threshold, unless the key has exactly one target. A
    /// non-empty list is `resolve: inline` with a `propagation` too: the site is inlined *and*
    /// gets the synthetic summary, for a contract whose receivers are sometimes the program's
    /// objects, whose bodies inlining reaches, and sometimes a library's, which have none.
    Inline(Vec<(ModelPort, ModelPort)>),
    /// One synthetic summary at the site, carrying these `(destination, source)` propagations.
    /// Two generators matching one key union their lists, as two generators matching one
    /// function do.
    Model(Vec<(ModelPort, ModelPort)>),
    /// Empty propagation list: the target set is discarded and nothing flows.
    Skip,
}

impl Disposition {
    /// Higher wins when several generators match one key.
    fn rank(&self) -> u8 {
        match self {
            Disposition::Inline(_) => 2,
            Disposition::Model(_) => 1,
            Disposition::Skip => 0,
        }
    }
}

/// One signature a `find: "dispatch"` generator matched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchModel {
    pub disposition: Disposition,
    /// `file:generator-index` of every contributing generator, for diagnostics.
    pub provenance: Vec<String>,
}

impl DispatchModel {
    /// Folds another generator's disposition for the same key in, applying the precedence in
    /// [`Disposition`]. Two `Model`s, or two `Inline`s, union their propagation lists; otherwise
    /// the higher-ranked disposition wins. Either way both generators stay in the provenance,
    /// which is what lets the diagnostics name the one that lost.
    pub fn merge(&mut self, other: DispatchModel) {
        match (&mut self.disposition, other.disposition) {
            (Disposition::Model(ours), Disposition::Model(theirs))
            | (Disposition::Inline(ours), Disposition::Inline(theirs)) => {
                for p in theirs {
                    if !ours.contains(&p) {
                        ours.push(p);
                    }
                }
            }
            (ours, theirs) => {
                if theirs.rank() > ours.rank() {
                    *ours = theirs;
                }
            }
        }
        for p in other.provenance {
            if !self.provenance.contains(&p) {
                self.provenance.push(p);
            }
        }
    }
}

/// One matched `propagation`: taint arriving at `src` appears at `dst` in `function`.
///
/// The function is named, not resolved to a [`crate::facts::FunctionId`]: a summary attaches to
/// any function that was so much as *called*, including ones with no IR body at all, and the id
/// map that would resolve it does not hold every import's functions until the loop is over.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
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
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
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
    /// time. The name has to be resolved against the *matched function's* `locals` during
    /// Stage 1, and it **cannot** be recovered later: Stage 2 sees only the
    /// post-`eliminate_dead_temps`/`coalesce_copies` graph, where the name may no longer exist.
    /// Portcullis spells it `F::local(L)`, with `L` bound by `local(F, L, name = ...)`.
    pub local_index: Option<u32>,
}

/// Everything a models file matched, accumulated across every (import x model file) pair.
///
/// Accumulation is append-only and its *order* is only required to be deterministic, not
/// specific: the SARIF writer sorts endpoints by content before writing anything, so
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
    /// Functions a `modes: ["skip-analysis"]` generator matched. Codegen reads this set as it
    /// lowers each function and simply does not lower the body of one that is in it, so nothing
    /// derived from those bodies exists to begin with -- the model's own `propagation` rows
    /// become the function's whole behaviour.
    pub skip_analysis: BTreeSet<facts::Str>,
    /// Functions a `mem2reg_full` or `mem2reg_window` head marked. The SSA pipeline reads this
    /// before codegen, to force or forbid full stack-slot promotion.
    pub mem2reg_modes: std::collections::BTreeMap<facts::Str, ctadl_ir::ssa::FullMode>,
    /// Functions a `frame_store` or `frame_update` head marked. The SSA pipeline reads this
    /// before codegen, to coarsen or keep the function's frame updates.
    pub frame_modes: std::collections::BTreeMap<facts::Str, ctadl_ir::ssa::FrameWrites>,
    /// Signatures a `find: "dispatch"` generator matched. Index time, and also read by phase
    /// *1*: codegen classifies each Java and CIL call site against this map before it
    /// emits anything for the site.
    ///
    /// A `BTreeMap` so iteration order does not depend on hashing: the synthetic functions
    /// phase 2 interns are named from these keys.
    pub dispatch: std::collections::BTreeMap<DispatchKey, DispatchModel>,
    /// Signatures a `model: {"closure_shaped": true}` generator named: arbitrary code behind a
    /// one-method interface, which is what hybrid inlining is for. Report time only -- nothing
    /// resolves differently for being on this list, and codegen never reads it.
    ///
    /// It exists because the structural single-abstract-method test cannot see an interface the
    /// import does not declare, which on an app that ships no framework is half of them. A
    /// framework name is something an obfuscator cannot touch, so the two together cover what
    /// either alone misses.
    pub closure_shaped: BTreeSet<DispatchKey>,
    /// Bridges the DSL engine grounded: both sides are function names, paired across every
    /// import. Phase 2 emits them.
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

    /// Folds in the functions one load marked `skip-analysis`.
    pub fn extend_skip_analysis(&mut self, functions: impl IntoIterator<Item = facts::Str>) {
        self.skip_analysis.extend(functions);
    }

    /// Folds one matched dispatch model in, merging with anything already matched for the key.
    pub fn add_dispatch(&mut self, key: DispatchKey, model: DispatchModel) {
        match self.dispatch.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut e) => e.get_mut().merge(model),
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert(model);
            }
        }
    }

    /// Whether anything was matched at all.
    pub fn is_empty(&self) -> bool {
        self.propagations.is_empty()
            && self.endpoints.is_empty()
            && self.access_paths.is_empty()
            && self.skip_analysis.is_empty()
            && self.mem2reg_modes.is_empty()
            && self.frame_modes.is_empty()
            && self.dispatch.is_empty()
            && self.closure_shaped.is_empty()
            && self.resolved_bridges.is_empty()
    }
}
