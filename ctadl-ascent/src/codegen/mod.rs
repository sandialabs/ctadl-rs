/*! Generates Datalog code from CTADL IR.

# Introduction

Codegen is the process of generating Datalog code from CTADL IR. CTADL IR, the input, is expressed
as a [`ctadl_ir::mir::ProgramInfo`] [`crate::index_engine::IndexFacts`] is the output type. This
phase provides a hook in [`crate::models::codegen`] to run arbitrary code that generates models for
indexing.

# Details

Some notes about choices made in the design of generating code:

Parameters in IR are mapped to the same indices in the Datalog. Return values are mapped to index
-1, -2, -3, etc. The global heap is mapped to [`GLOBALS_INDEX`], which is [`i16::MIN`].

# Resolving a Java call

Under [`CallResolutionStrategy::Mixed`] every Java call site is classified by one function,
`CodegenVisitor::classify`, which runs a four-rung ladder: super resolution, a dispatch model,
a target-count threshold, then hybrid inlining. Each site lands in exactly one of four counted
buckets, and [`codegen_program`] asserts that the counts add up. The other strategies have a
fixed answer for every site and consult none of the policy.

`skip_analysis` holds the names a `modes: ["skip-analysis"]` generator matched (see
[`crate::models::matches::ProgramModelMatches::skip_analysis`]). Those functions get their
signature lowered and their body dropped, which is the whole implementation of the directive:
with no `assign`, `call`, `callee_info` or `call_target_assign` rows inside them, the indexer
has nothing to derive a body summary from and nothing to feed hybrid inlining with. Matching
runs per import immediately before codegen, so every name this import can contribute is
already in the set by the time we get here.

*/
use std::collections::{BTreeMap, BTreeSet};

use smallvec::SmallVec;

use crate::facts as fx;
use crate::facts::{FlowVariable, FlowVariableKind, FlowVertex, FormalIndex, Str};
use crate::index_engine::{IndexFacts, source_info::IndexSourceInfo};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::{
    call::{JavaDispatch, VirtualMethodTable},
    visit::Visitor,
    *,
};

#[cfg(test)]
mod tests;

pub mod flowy;
pub mod model_matches;

/// Strategy for resolving virtual calls
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CallResolutionStrategy {
    /// Every call is resolved with Class Hierarchy Analysis.
    Cha,
    /// Every call is resolved with hybrid inlining (no calls resolved with CHA).
    Hi,
    /// The ladder: super resolution, then a dispatch model, then a target-count threshold,
    /// then hybrid inlining.
    #[default]
    Mixed,
    /// CHA when the site has exactly one target, hybrid inlining otherwise. Kept as the
    /// baseline the ladder is measured against, on one binary.
    LegacyMixed,
}

/// Which rung the ladder tries first, the dispatch model or the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum DispatchOrder {
    /// Take a modelled signature out of the analysis whatever its target count.
    #[default]
    ModelFirst,
    /// Resolve a signature with few enough targets exactly, even when a model matches it.
    /// For a precision-sensitive run.
    ThresholdFirst,
}

/// Default for [`CallPolicy::cha_threshold`].
///
/// Chosen for what it costs the *engine*, not for the size of the call graph. Raising it keeps
/// shrinking the share of sites hybrid inlining has to take -- 19.4% of antennapod's sites at
/// the legacy rule, 3.3% here, 0.6% at 32 -- but each step also turns deferred sites into CHA
/// edges, and on a program with a large recursive strongly connected component the fixpoint
/// cost of those is superlinear. Measured on antennapod, indexing takes 19 s at 4 and 106 s at
/// 32 for 2.7 further points; on xbot_android_samp it is flat through 8, eight times slower at
/// 16, and out of memory at 32.
///
/// So 4 buys most of the soundness win for no index cost at all. A run that wants the last
/// points, and can pay for them, raises the flag.
pub const DEFAULT_CHA_THRESHOLD: usize = 4;

/// How [`CallResolutionStrategy::Mixed`] classifies a Java call site.
///
/// Read at codegen and nowhere else. It is *recorded* in the on-disk index config so a query
/// can say what policy produced the index it is reading, but the engine never sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallPolicy {
    /// Rung 2: at or under this many CHA targets, a site gets ordinary CHA edges. `0` disables
    /// rung 2; a very large value disables rung 3.
    pub cha_threshold: usize,
    /// Rung 2 for `Interface` sites, which are a different population: about a tenth of them
    /// resolve to a single target against four fifths of ordinary virtual calls.
    pub cha_threshold_interface: usize,
    /// Rung 1 on or off, for virtual, super and unknown-dispatch sites.
    pub dispatch_models: bool,
    /// Rung 1 on or off for interface sites.
    pub dispatch_models_interface: bool,
    pub order: DispatchOrder,
}

impl Default for CallPolicy {
    fn default() -> Self {
        Self {
            cha_threshold: DEFAULT_CHA_THRESHOLD,
            cha_threshold_interface: DEFAULT_CHA_THRESHOLD,
            dispatch_models: true,
            dispatch_models_interface: true,
            order: DispatchOrder::default(),
        }
    }
}

/// What a matched `find: "dispatch"` model says to do with a signature, once the source/sink
/// refusal has had its say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDisposition {
    /// A non-empty propagation list: one synthetic summary at the site.
    Model,
    /// An empty propagation list: the target set is discarded.
    Skip,
    /// `resolve: "inline"`: hybrid inlining whatever the target count.
    Inline,
}

/// Which rung of the ladder a site takes, once rung 0 has declined it.
///
/// One function decides this ([`classify_rung`]) and two callers read it: codegen, which emits
/// the rows, and `ctadl report`'s policy section, which simulates the whole classification over
/// an import without indexing. A second implementation is how the report starts describing a
/// policy the index does not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1 with a propagation.
    Model,
    /// Rung 1 with an empty propagation.
    Skip,
    /// Rung 2: ordinary CHA edges over the signature's resolvent set, which may be empty.
    Cha,
    /// Rung 3, or a rung-1 `inline` disposition (`by_model`).
    Inline { by_model: bool },
}

/// The ladder below rung 0: a dispatch model, then the threshold, then hybrid inlining.
///
/// `targets` is the signature's CHA target count and `disposition` its matched model, if the
/// caller's policy admits one for this dispatch kind.
pub fn classify_rung(
    policy: &CallPolicy,
    dispatch: JavaDispatch,
    targets: usize,
    disposition: Option<DispatchDisposition>,
) -> Rung {
    let interface = dispatch == JavaDispatch::Interface;
    let models_on = if interface {
        policy.dispatch_models_interface
    } else {
        policy.dispatch_models
    };
    let threshold = if interface {
        policy.cha_threshold_interface
    } else {
        policy.cha_threshold
    };
    let disposition = if models_on { disposition } else { None };

    // The `inline` disposition is honoured in both orders: its purpose is to keep the site off
    // CHA, and threshold-first would silently undo that for every site at or under `K`. A site
    // with exactly one target stays exact, though -- inlining a monomorphic site gains nothing
    // and loses the edge when the receiver's allocation is not visible. Zero targets still
    // defer: hybrid inlining can find a callee from the allocated class where the static type
    // resolves to nothing.
    if disposition == Some(DispatchDisposition::Inline) {
        return if targets == 1 {
            Rung::Cha
        } else {
            Rung::Inline { by_model: true }
        };
    }
    let modelled = match disposition {
        Some(DispatchDisposition::Model) => Some(Rung::Model),
        Some(DispatchDisposition::Skip) => Some(Rung::Skip),
        _ => None,
    };
    // Rung 1. Model-first ignores the target count, which is what takes a sixth of an app's
    // sites out of the engine.
    if policy.order == DispatchOrder::ModelFirst
        && let Some(rung) = modelled
    {
        return rung;
    }
    // Rung 2, which covers 0 and 1 targets as well.
    if targets <= threshold {
        return Rung::Cha;
    }
    // Threshold-first reaches rung 1 only above `K`.
    if let Some(rung) = modelled {
        return rung;
    }
    // Rung 3.
    Rung::Inline { by_model: false }
}

/// Every Java call site lands in exactly one of the four buckets. The invariant is asserted:
/// `modelled + skipped + cha + inlined == java_sites`.
///
/// Without these counts a mis-scoped dispatch model silently swallows a signature and the only
/// symptom is a missing finding.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SiteBuckets {
    /// Rung 1 with a propagation: one `call` row to the signature's synthetic function.
    pub modelled: usize,
    /// Rung 1 with an empty propagation: the target set was discarded.
    pub skipped: usize,
    /// Rung 0 or rung 2: ordinary CHA edges.
    pub cha: usize,
    /// Rung 3, or a rung-1 `resolve: inline`: deferred to hybrid inlining.
    pub inlined: usize,
    /// Sub-count of [`Self::cha`]: sites that resolved to nothing at all, so the rung emitted
    /// no rows. Reporting it is how the fraction of a percent of sites with no callee stops
    /// being silent.
    pub cha_zero_targets: usize,
    /// Sub-count of [`Self::cha`]: sites where super resolution found the single real target.
    pub cha_super_exact: usize,
    /// Sub-count of [`Self::inlined`]: sites a `resolve: inline` model deferred, rather than
    /// the threshold. A mis-scoped entry moves sites to hybrid inlining silently, and its only
    /// other symptom is a slower index.
    pub inlined_by_model: usize,
    pub java_sites: usize,
}

impl SiteBuckets {
    /// Folds another set of counts in, bucket by bucket.
    pub fn add(&mut self, other: &SiteBuckets) {
        self.modelled += other.modelled;
        self.skipped += other.skipped;
        self.cha += other.cha;
        self.inlined += other.inlined;
        self.cha_zero_targets += other.cha_zero_targets;
        self.cha_super_exact += other.cha_super_exact;
        self.inlined_by_model += other.inlined_by_model;
        self.java_sites += other.java_sites;
    }

    /// Whether every site landed in exactly one bucket.
    pub fn balanced(&self) -> bool {
        self.modelled + self.skipped + self.cha + self.inlined == self.java_sites
    }
}

impl std::fmt::Display for SiteBuckets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} java site(s): {} modelled, {} skipped, {} CHA ({} with no target, {} exact \
             super), {} inlined ({} by model)",
            self.java_sites,
            self.modelled,
            self.skipped,
            self.cha,
            self.cha_zero_targets,
            self.cha_super_exact,
            self.inlined,
            self.inlined_by_model
        )
    }
}

/// What phase 1 of codegen did, beyond the facts it wrote. Accumulated over the import loop.
#[derive(Debug, Default, Clone)]
pub struct CodegenReport {
    /// Bodies a `modes: ["skip-analysis"]` generator kept out of the fact base. Counted here
    /// rather than from the matched names, which is the only place that knows a name belonged
    /// to a function this project actually lowered.
    pub skipped_bodies: usize,
    /// Java call sites by bucket, per [`JavaDispatch::index`].
    pub buckets: [SiteBuckets; 4],
    /// Signatures whose dispatch model was refused because the CHA target set holds a matched
    /// source or sink, mapped to the endpoint that refused it.
    pub refused: BTreeMap<String, String>,
}

impl CodegenReport {
    /// Every dispatch kind together.
    pub fn totals(&self) -> SiteBuckets {
        let mut total = SiteBuckets::default();
        for b in &self.buckets {
            total.add(b);
        }
        total
    }

    /// Folds another import's phase-1 report in.
    pub fn merge(&mut self, other: CodegenReport) {
        self.skipped_bodies += other.skipped_bodies;
        for (ours, theirs) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            ours.add(theirs);
        }
        self.refused.extend(other.refused);
    }
}

/// Generate code for a program in SSA form (see [`ctadl_ir::ssa::transform`]).
///
/// `matches` is what the model files matched against *this* import; phase 1 reads its
/// `skip_analysis`, `dispatch` and `endpoints` fields. `policy` parameterizes
/// [`CallResolutionStrategy::Mixed`] and is ignored by every other strategy.
#[inline]
pub fn codegen_program(
    mut program_info: ProgramInfo,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
    strategy: CallResolutionStrategy,
    policy: CallPolicy,
    matches: &crate::models::ProgramModelMatches,
) -> CodegenReport {
    let mut instantiated_classes = BTreeSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    // Every function, skipped ones included. This feeds the class hierarchy, which is a property
    // of the whole program: dropping the allocations a skipped body performs would delete
    // resolvents that call sites *elsewhere* depend on.
    for f in program_info.program.functions.iter() {
        finder.visit_function_data(FunctionIdx::new(0), f);
    }

    let cha = ClassHierarchyAnalysis::new(&program_info.vmt, instantiated_classes);
    let mut v = CodegenVisitor::new(cha, facts, source_info, strategy, policy, matches);
    for f in program_info.program.functions.drain(..) {
        v.visit_function_data(FunctionIdx::new(0), &f);
    }
    v.finish_with_vmt(&program_info.vmt);
    let report = std::mem::take(&mut v.report);
    let totals = report.totals();
    // Returning a `SiteAction` for every site makes this true by construction; the assertion
    // keeps it true when someone adds a rung.
    debug_assert!(
        totals.balanced(),
        "unbalanced call-site buckets: {totals:?}"
    );
    if !totals.balanced() {
        log::error!("unbalanced call-site buckets: {totals:?}");
    }
    report
}

/// Generate code for a function in SSA form (see [`ctadl_ir::ssa::transform`]).
///
/// TODO this function doesn't do any class hierarchy analysis which seems like a bug waiting to
/// happen. It also doesn't apply any codegen models
#[inline]
pub fn codegen_function(
    function_data: &FunctionData,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    let function_data_owned = function_data.clone();
    let function_data = &function_data_owned;
    let mut instantiated_classes = BTreeSet::new();
    let mut finder = InstantiationFinder {
        instantiated_classes: &mut instantiated_classes,
    };
    finder.visit_function_data(FunctionIdx::new(0), function_data);

    let cha = ClassHierarchyAnalysis::new(&VirtualMethodTable::Unknown, instantiated_classes);
    log::trace!("codegen for {}", function_data.name);
    let no_matches = crate::models::ProgramModelMatches::default();
    let mut v = CodegenVisitor::new(
        cha,
        facts,
        source_info,
        CallResolutionStrategy::Mixed,
        CallPolicy::default(),
        &no_matches,
    );
    v.visit_function_data(FunctionIdx::new(0), function_data);
    v.finish();
}

/// For passing globals reference in parameter list
pub const GLOBALS_INDEX: i16 = i16::MIN;

/// Start of return values. If there's more than one return value, uses -2, -3, etc
pub const RETURN_INDEX: i16 = -1i16;

pub fn variable_is_globals(v: &FlowVariable) -> bool {
    match v.kind() {
        FlowVariableKind::Formal(idx) => *idx == GLOBALS_INDEX,
        // This has to be kept in sync with the name given to globals in the CodegenVisitor
        FlowVariableKind::Local(name) => name.starts_with("$globals_"),
        _ => false,
    }
}

/// The `call_target_assign` payload for an *object-valued* expression: the class of an
/// allocation-site tag, in the receiving language's own [`fx::CallTargetObject`] variant.
/// `None` for anything else, including a `FunctionPtr` ref — those carry an interned function
/// id and so are handled at each site, which needs the id for `funcptr_targets` too.
fn call_target_object(exp: &Exp) -> Option<fx::CallTargetObject> {
    match exp {
        Exp::ObjectRef(CallObject::JavaObject(cls)) => {
            Some(fx::CallTargetObject::Symbol(cls.0.clone()))
        }
        Exp::ObjectRef(CallObject::LuaClass(cls)) => {
            Some(fx::CallTargetObject::LuaClass(cls.clone()))
        }
        _ => None,
    }
}

/// Collects every class the program allocates, which is the input the RTA arm of
/// [`run_cha`] restricts on. `pub(crate)` so [`crate::report`] can run the same collection
/// over an import without going through codegen.
pub(crate) struct InstantiationFinder<'a> {
    instantiated_classes: &'a mut BTreeSet<Symbol>,
}

impl<'a> InstantiationFinder<'a> {
    pub(crate) fn new(instantiated_classes: &'a mut BTreeSet<Symbol>) -> Self {
        Self {
            instantiated_classes,
        }
    }
}

impl Visitor for InstantiationFinder<'_> {
    fn visit_exp(&mut self, exp: &Exp) {
        match exp {
            Exp::ObjectRef(CallObject::JavaObject(cls)) => {
                self.instantiated_classes.insert(cls.0.clone());
            }
            Exp::ObjectRef(CallObject::LuaClass(cls)) => {
                self.instantiated_classes.insert(cls.clone());
            }
            _ => {}
        }
        self.super_exp(exp);
    }
}

#[derive(Debug)]
struct CodegenVisitor<'a> {
    /// Output facts
    facts: &'a mut IndexFacts,
    source_info: &'a mut IndexSourceInfo,
    cha: ClassHierarchyAnalysis,
    strategy: CallResolutionStrategy,
    /// Name of the function under translation (set in visit_function_data)
    function: Option<fx::FunctionId>,
    /// We may see the same access path multiple times so we dedup them with this set
    paths_dedup: BTreeSet<(fx::Path,)>,
    /// Per-block map from a load-chain temporary to the (root variable, composed field path) it
    /// stands for (`t2 = load t1.b` where `t1 = load x.a` ⟹ `t2 ↦ (x, .a.b)`). Populated in the
    /// pre-pass over each block ([`Self::visit_basic_block_data`]) and used to *re-anchor* a
    /// `Store` through such a temporary (`store t2.c := v`) back onto the formal path it addresses
    /// (`x.a.b.c := v`), so a write through a loaded pointer is recorded at the object it names
    /// rather than at the temporary. Cleared per block.
    cap_path: BTreeMap<VariableRef, (VariableRef, fx::Path)>,
    /// Distinct functions stored as C-style call targets (`CallTargetObject::FunctionId`)
    /// anywhere in this codegen unit. Each gets an identity `callee_resolvents(FunctionId(f),
    /// C, f)` fact emitted in [`Self::finish`] — the function-pointer analogue of the CHA
    /// `callee_resolvents`, which is what lets the unified resolution rules resolve a reached
    /// function pointer to itself without a `C`-specific rule in the index engine.
    funcptr_targets: BTreeSet<fx::FunctionId>,
    /// What the model files matched against this import. Phase 1 reads three of its fields:
    /// `skip_analysis` (whose bodies are not lowered), `dispatch` (rung 1 of the ladder) and
    /// `endpoints` (which refuses a dispatch model, see [`Self::key_facts`]).
    matches: &'a crate::models::ProgramModelMatches,
    /// How [`CallResolutionStrategy::Mixed`] classifies a site.
    policy: CallPolicy,
    /// The functions a source or sink model matched, for the rung-1 refusal.
    endpoint_functions: BTreeSet<Str>,
    /// Everything about a signature the ladder needs that is not a property of the site. Sites
    /// outnumber signatures by two to three orders of magnitude, which is what makes computing
    /// this once per key rather than once per site worth the map.
    key_cache: hashbrown::HashMap<SignatureKey, KeyFacts>,
    /// Signatures a site has actually been modelled with, so the synthetic function's
    /// `external_function` row is pushed once.
    synthetics: BTreeSet<SignatureKey>,
    /// The `(simple name, descriptor)` pairs some site deferred to hybrid inlining.
    ///
    /// `callee_resolvents` is what the engine's resolution rule joins a deferred site's
    /// receiver class against, and a pair no site deferred can never be joined. Collecting
    /// them here and emitting in [`Self::finish`] is what keeps the whole program's CHA table
    /// out of the fact base. The class is deliberately not part of the key: the join is on the
    /// receiver's *allocated* class, which is not the one the site names.
    deferred_dispatch: BTreeSet<(Symbol, Symbol)>,
    /// What this visitor did, returned by [`codegen_program`].
    report: CodegenReport,
}

/// Everything the ladder needs to know about one signature, independent of the site.
#[derive(Debug, Clone)]
struct KeyFacts {
    /// How many methods CHA resolves the signature to.
    targets: usize,
    /// The rung-1 disposition, after the source/sink refusal has had its say.
    disposition: Option<DispatchDisposition>,
}

/// What a site's static signature, dispatch kind and CHA target count decide. Returned for
/// every Java site, zero-resolvent ones included, so the bucket counts cover all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SiteAction {
    /// Rung 1, non-empty propagation: one `call` row to the signature's synthetic function.
    Model(fx::FunctionId),
    /// Rung 1, empty propagation: emit nothing, count it.
    Skip,
    /// Rung 2, and every site under `--strategy cha`: ordinary CHA edges over the signature's
    /// resolvent set, which may be empty.
    Cha,
    /// Rung 0: super resolution found the one real target.
    SuperExact(Symbol),
    /// Rung 3, or a rung-1 `resolve: inline`: hybrid inlining.
    Defer { by_model: bool },
}

impl<'a> CodegenVisitor<'a> {
    /// Codegen visitor. Generates facts into the index facts. Call the visitor to generate the
    /// facts. You must call [`CodegenVisitor::finish`] to get all the facts.
    #[inline]
    fn new(
        cha: ClassHierarchyAnalysis,
        facts: &'a mut IndexFacts,
        source_info: &'a mut IndexSourceInfo,
        strategy: CallResolutionStrategy,
        policy: CallPolicy,
        matches: &'a crate::models::ProgramModelMatches,
    ) -> Self {
        Self {
            function: None,
            facts,
            cha,
            source_info,
            strategy,
            paths_dedup: Default::default(),
            cap_path: Default::default(),
            funcptr_targets: Default::default(),
            endpoint_functions: matches.endpoints.iter().map(|e| e.function).collect(),
            matches,
            policy,
            key_cache: Default::default(),
            synthetics: Default::default(),
            deferred_dispatch: Default::default(),
            report: Default::default(),
        }
    }

    /// Which of the four buckets a Java call site belongs in, and what to emit there.
    ///
    /// Under every strategy but [`CallResolutionStrategy::Mixed`] this is the strategy's own
    /// fixed answer; `Mixed` runs the ladder in [`Self::classify_ladder`].
    fn classify(
        &mut self,
        key: &SignatureKey,
        dispatch: JavaDispatch,
        super_start: Option<&Symbol>,
    ) -> SiteAction {
        match self.strategy {
            CallResolutionStrategy::Cha => SiteAction::Cha,
            CallResolutionStrategy::Hi => SiteAction::Defer { by_model: false },
            CallResolutionStrategy::LegacyMixed => {
                // A zero-target site emits nothing either way, so it takes the CHA arm; that
                // is also where the ladder counts it.
                if self.resolvent_count(key) <= 1 {
                    SiteAction::Cha
                } else {
                    SiteAction::Defer { by_model: false }
                }
            }
            CallResolutionStrategy::Mixed => self.classify_ladder(key, dispatch, super_start),
        }
    }

    /// Rung 0, then [`classify_rung`] for the rest.
    fn classify_ladder(
        &mut self,
        key: &SignatureKey,
        dispatch: JavaDispatch,
        super_start: Option<&Symbol>,
    ) -> SiteAction {
        // Rung 0. A super call's target is known, so modelling it would be strictly worse than
        // resolving it; this rung does not move under `--dispatch-order`.
        if dispatch == JavaDispatch::Super {
            let start = super_start.unwrap_or(&key.0);
            if let SuperResolution::Exactly(target) =
                self.cha.super_resolvent(start, &key.1, &key.2)
            {
                return SiteAction::SuperExact(target);
            }
        }
        let facts = self.key_facts(key);
        match classify_rung(&self.policy, dispatch, facts.targets, facts.disposition) {
            Rung::Model => SiteAction::Model(self.synthetic(key)),
            Rung::Skip => SiteAction::Skip,
            Rung::Cha => SiteAction::Cha,
            Rung::Inline { by_model } => SiteAction::Defer { by_model },
        }
    }

    /// Emits the rows a classified site calls for, and counts it into its bucket.
    fn apply(
        &mut self,
        site: fx::PackedInsnSiteId,
        recv_var: FlowVariable,
        key: &SignatureKey,
        dispatch: JavaDispatch,
        action: SiteAction,
    ) {
        let (cls, simple_name, descriptor) = key;
        let buckets = &mut self.report.buckets[dispatch.index()];
        buckets.java_sites += 1;
        match action {
            SiteAction::Model(target) => {
                buckets.modelled += 1;
                log::trace!("java: model {cls}.{simple_name}{descriptor}");
                self.facts.call.push((site, target));
            }
            SiteAction::Skip => {
                buckets.skipped += 1;
                log::trace!("java: skip {cls}.{simple_name}{descriptor}");
            }
            SiteAction::SuperExact(target) => {
                buckets.cha += 1;
                buckets.cha_super_exact += 1;
                log::trace!("java: super resolve {cls}.{simple_name}{descriptor} to {target}");
                let target = fx::Function(target.into());
                let target = self.source_info.sites.get_or_add_function(target);
                self.facts.call.push((site, target));
            }
            SiteAction::Cha => {
                let resolvents: SmallVec<[Symbol; 4]> = self
                    .cha
                    .java_resolvents(cls.clone(), simple_name.clone(), descriptor.clone())
                    .collect();
                buckets.cha += 1;
                if resolvents.is_empty() {
                    buckets.cha_zero_targets += 1;
                    log::trace!("java: no resolvents {cls}.{simple_name}{descriptor}");
                } else {
                    log::trace!(
                        "java: CHA resolve {cls}.{simple_name}{descriptor} with {} target(s)",
                        resolvents.len()
                    );
                }
                for target in resolvents {
                    let target = fx::Function(target.into());
                    let target = self.source_info.sites.get_or_add_function(target);
                    self.facts.call.push((site, target));
                }
            }
            SiteAction::Defer { by_model } => {
                buckets.inlined += 1;
                if by_model {
                    buckets.inlined_by_model += 1;
                }
                log::trace!("java: hybrid resolve {cls}.{simple_name}{descriptor} (deferred)");
                self.deferred_dispatch
                    .insert((simple_name.clone(), descriptor.clone()));
                self.facts.callee_info.push((
                    site,
                    FlowVertex(recv_var, fx::Path::empty()),
                    fx::CallDispatchKey::Java(simple_name.clone(), descriptor.clone()),
                ));
            }
        }
    }

    /// The function a modelled signature's summary hangs off, interned on first use.
    ///
    /// Interned here rather than when the model is matched, so a signature whose model is
    /// switched off for this site's dispatch kind, or lost to the threshold under
    /// `threshold-first`, leaves no function behind in the fact base.
    fn synthetic(&mut self, key: &SignatureKey) -> fx::FunctionId {
        let id = self
            .source_info
            .sites
            .get_or_add_function(fx::Function(Str::from(
                synthetic_dispatch_function(key).as_str(),
            )));
        if self.synthetics.insert(key.clone()) {
            // No body. Phase 2 gives it the `formal_param` rows its summary mentions and the
            // summary itself, and nothing else does.
            self.facts.external_function.push((id,));
        }
        id
    }

    /// How many methods CHA resolves `key` to.
    fn resolvent_count(&self, key: &SignatureKey) -> usize {
        self.cha
            .java_resolvents(key.0.clone(), key.1.clone(), key.2.clone())
            .len()
    }

    /// Everything about a signature the ladder needs, computed once per signature.
    ///
    /// A dispatch model is **refused** when the signature's CHA target set holds a function a
    /// source or sink model matched: modelling the site would take those bodies out of the
    /// analysis along with the endpoint inside them. The site falls to the threshold instead.
    /// The check is over the *direct* targets and cannot be more than that -- making it
    /// transitive would refuse nearly every model, since most of a program sits in one SCC --
    /// which is why a signature whose bodies must stay reachable takes `resolve: inline`.
    ///
    /// `Inline` is never refused: hybrid inlining hides no callee.
    fn key_facts(&mut self, key: &SignatureKey) -> KeyFacts {
        if let Some(facts) = self.key_cache.get(key) {
            return facts.clone();
        }
        let targets: SmallVec<[Symbol; 4]> = self
            .cha
            .java_resolvents(key.0.clone(), key.1.clone(), key.2.clone())
            .collect();
        let model_key = (
            Str::from(key.0.as_ref()),
            Str::from(key.1.as_ref()),
            Str::from(key.2.as_ref()),
        );
        let disposition = match self.matches.dispatch.get(&model_key) {
            None => None,
            Some(model) => match &model.disposition {
                crate::models::Disposition::Inline => Some(DispatchDisposition::Inline),
                crate::models::Disposition::Model(_) | crate::models::Disposition::Skip => {
                    let endpoint = targets
                        .iter()
                        .map(|t| Str::from(t.as_ref()))
                        .find(|t| self.endpoint_functions.contains(t));
                    match endpoint {
                        Some(endpoint) => {
                            self.report.refused.insert(
                                format!("{}->{}{}", key.0, key.1, key.2),
                                endpoint.to_string(),
                            );
                            None
                        }
                        None if matches!(model.disposition, crate::models::Disposition::Skip) => {
                            Some(DispatchDisposition::Skip)
                        }
                        None => Some(DispatchDisposition::Model),
                    }
                }
            },
        };
        let facts = KeyFacts {
            targets: targets.len(),
            disposition,
        };
        self.key_cache.insert(key.clone(), facts.clone());
        facts
    }

    /// Gens the dedup'd paths to the facts
    fn finish(&mut self) {
        // The empty path (whole-variable flow) must always be in the `paths` gate so the
        // forward field-propagation rules can reach a scalar. Previously this was implied by
        // every pathless `Exp::AccessPath` carrying an (empty) field-access list that the
        // visitor inserted; now that a pathless read is `Exp::Variable` (no field-access list),
        // insert it explicitly. It is trivially bounded, so it does not affect termination.
        if std::env::var_os("CTADL_NO_EMPTY_PATH").is_none() {
            self.paths_dedup.insert((fx::Path::empty(),));
        }
        let deferred = std::mem::take(&mut self.deferred_dispatch);
        emit_callee_resolvents(&self.cha, &deferred, self.facts, self.source_info);
        let paths = std::mem::take(&mut self.paths_dedup);
        self.facts.paths.extend(paths);
        let funcptr_targets = std::mem::take(&mut self.funcptr_targets);
        self.facts
            .callee_resolvents
            .extend(funcptr_targets.into_iter().map(|f| {
                (
                    fx::CallTargetObject::FunctionId(f),
                    fx::CallDispatchKey::C,
                    f,
                )
            }));
    }

    /// Does finish and also runs a datalog modeling pass
    #[inline]
    fn finish_with_vmt(&mut self, vmt: &VirtualMethodTable) {
        self.finish();
        crate::models::codegen::load_models(vmt, self.facts, &self.source_info.sites);
    }
}

impl Visitor for CodegenVisitor<'_> {
    #[inline]
    fn visit_function_data(&mut self, idx: FunctionIdx, function: &FunctionData) {
        let name: Str = function.name.clone().into();
        // Checked before the name is interned, against the same spelling the model matcher took
        // out of the IR.
        let skip = self.matches.skip_analysis.contains(&name);
        let func_id = self
            .source_info
            .sites
            .get_or_add_function(fx::Function(name));
        self.function = Some(func_id);
        if function.blocks.is_empty() {
            self.facts.external_function.push((func_id,));
        }
        // Gens global param
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::formal_index(GLOBALS_INDEX.into()),
            fx::FormalType::ByRef,
        ));
        // Gens return parameter
        self.facts.formal_param.push((
            self.function.unwrap(),
            FlowVariable::formal_index(RETURN_INDEX.into()),
            fx::FormalType::ByRef,
        ));
        if skip {
            // `modes: ["skip-analysis"]`: lower the signature, drop the body. The function behaves
            // like a stub.
            log::debug!("skip-analysis: not lowering the body of {}", function.name);
            self.visit_params(&function.params);
            self.report.skipped_bodies += 1;
            return;
        }
        self.super_function_data(idx, function);
    }

    #[inline]
    fn visit_basic_block_data(
        &mut self,
        function: FunctionIdx,
        block: BasicBlockIdx,
        data: &BasicBlockData,
    ) {
        // Pre-pass: capture, for each load-chain temporary, the (root variable, composed field
        // path) it stands for. Used below to seed the `paths` gate and, in
        // `visit_statement_kind`, to re-anchor a `Store` through such a temporary onto the formal
        // path it addresses.
        self.cap_path.clear();
        for statement in &data.statements {
            match &statement.kind {
                StatementKind::Assign { dest, sources } if sources.len() == 1 => {
                    match &sources[0] {
                        // A whole-variable copy `dest = v` carries the captured (root, path) of `v`
                        // forward, so a Load chain that flows through a copy still composes.
                        Exp::Variable(v) => {
                            if let Some(cap) = self.cap_path.get(v).cloned() {
                                self.cap_path.insert(dest.clone(), cap);
                            }
                        }
                        // An address copy `dest = v.[k]` (pointer arithmetic) names the field
                        // path `<captured path of v> ++ [k]` rooted at v's root; record it so a
                        // later Load/Store through `dest` composes onto it and the composed path
                        // enters the `paths` gate.
                        Exp::AccessPath(ap) => {
                            let (root, base_path) = self
                                .cap_path
                                .get(&ap.base)
                                .cloned()
                                .unwrap_or_else(|| (ap.base.clone(), fx::Path::empty()));
                            let path = fx::Path::from_accesses(
                                base_path
                                    .iter()
                                    .cloned()
                                    .chain(ap.accesses.iter().cloned().map(PathSegment::from)),
                            );
                            self.paths_dedup.insert((path,));
                            self.cap_path.insert(dest.clone(), (root, path));
                        }
                        _ => {}
                    }
                }
                StatementKind::Load {
                    dest,
                    source,
                    field,
                } => {
                    // The effective field path read is the captured path of the source base
                    // variable, then the source's own (offset) address arithmetic, then the
                    // loaded field, all rooted at the source's root variable.
                    let (root, base_path) = self
                        .cap_path
                        .get(&source.base)
                        .cloned()
                        .unwrap_or_else(|| (source.base.clone(), fx::Path::empty()));
                    let path = fx::Path::from_accesses(
                        base_path
                            .iter()
                            .cloned()
                            .chain(source.accesses.iter().cloned().map(PathSegment::from))
                            .chain(std::iter::once(PathSegment::Symbol(
                                field.symbol_ref().clone(),
                            ))),
                    );
                    self.paths_dedup.insert((path,));
                    self.cap_path.insert(dest.clone(), (root, path));
                }
                StatementKind::Store { dest, field, .. } => {
                    // The full written field path is the captured path of the destination base
                    // variable, then the dest's own (offset) address arithmetic, then the written
                    // field.
                    let base_path = self
                        .cap_path
                        .get(&dest.base)
                        .map(|(_, p)| *p)
                        .unwrap_or_default();
                    let path = fx::Path::from_accesses(
                        base_path
                            .iter()
                            .cloned()
                            .chain(dest.accesses.iter().cloned().map(PathSegment::from))
                            .chain(std::iter::once(PathSegment::Symbol(
                                field.symbol_ref().clone(),
                            ))),
                    );
                    self.paths_dedup.insert((path,));
                }
                _ => {}
            }
        }
        self.super_basic_block_data(function, block, data);
        self.cap_path.clear();
    }

    /// Generates formal parameters
    #[inline]
    fn visit_params(&mut self, params: &Params) {
        self.super_params(params);
        for (i, &p) in params.iter_enumerated() {
            let i = i.try_into().unwrap();
            self.facts
                .formal_param
                .push((self.function.unwrap(), i, p.into()));
        }
    }

    /// Generates assignments for locals and out-parameters
    #[inline]
    fn visit_statement(&mut self, statement: &Statement, location: Location) {
        use StatementKind::*;
        self.super_statement(statement, location);
        let statement_kind = &statement.kind;
        let site = {
            let insn_site_id = self.source_info.add_insn_site(self.function.unwrap());
            insn_site_id.try_into().unwrap()
        };
        self.source_info
            .add_instruction_span(site, statement.source_info.span_id);
        match statement_kind {
            Assign { dest, sources } => {
                for src in sources {
                    if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = src {
                        let dest = self.trans_variable_ref(dest);
                        let target = fx::Function(name.clone().into());
                        let target = self.source_info.sites.get_or_add_function(target);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(dest, fx::Path::empty()),
                            fx::CallTargetObject::FunctionId(target),
                        ));
                        self.funcptr_targets.insert(target);
                    }
                    if let Some(object) = call_target_object(src) {
                        let dest = self.trans_variable_ref(dest);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(dest, fx::Path::empty()),
                            object,
                        ));
                    }
                    if let Exp::Str(value) = src {
                        let dest = self.trans_variable_ref(dest);
                        self.facts.const_str_assign.push((
                            site,
                            FlowVertex(dest, fx::Path::empty()),
                            Str::from(value.clone()),
                        ));
                    }
                    let Some(src) = self.trans_exp(src) else {
                        continue;
                    };
                    let dest = self.trans_variable_ref(dest);
                    self.facts
                        .assign
                        .push((site, FlowVertex(dest, fx::Path::empty()), src));
                }
            }
            Phi {
                dest: out,
                operands,
            } => {
                let dst = FlowVertex(self.trans_variable_ref(out), fx::Path::empty());
                let mut seen_phi = BTreeSet::new();
                for (_, op) in operands {
                    let src = FlowVertex(self.trans_variable_ref(op), fx::Path::empty());
                    if seen_phi.insert(src.clone()) {
                        self.facts.assign.push((site, dst.clone(), src));
                    }
                }
            }
            ParamFlow { params, global } => {
                let mut seen_param = BTreeSet::new();
                for (i, op) in params.iter().enumerate() {
                    // assign current version of formal back to the formal itself so we can track
                    // data flow
                    let dst = FlowVariable::formal_index(i.try_into().unwrap());
                    let src = self.trans_variable_ref(op);
                    if seen_param.insert((dst, src)) {
                        let dst = FlowVertex(dst, fx::Path::empty());
                        let src = FlowVertex(src, fx::Path::empty());
                        self.facts.assign.push((site, dst, src));
                    }
                }
                // assign current version of global back to the auxparam global
                let dst = FlowVariable::formal_index(GLOBALS_INDEX.into());
                let src = self.trans_variable_ref(global);
                if seen_param.insert((dst, src)) {
                    let dst = FlowVertex(dst, fx::Path::empty());
                    let src = FlowVertex(src, fx::Path::empty());
                    self.facts.assign.push((site, dst, src));
                }
            }
            CallAssign { rets, args, style } => {
                let mut args = args.clone();
                match style {
                    CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(targets),
                    } => {
                        for target in targets {
                            let target = fx::Function(target.clone().into());
                            let target = self.source_info.sites.get_or_add_function(target);
                            self.facts.call.push((site, target));
                        }
                    }
                    CallStyle::JavaCall {
                        receiver,
                        cls,
                        simple_name,
                        descriptor,
                        dispatch,
                        super_start,
                    } => {
                        let recv_var = self.trans_variable_ref(receiver);
                        self.facts.android_call_site.push((
                            site,
                            Str::from(cls.clone()),
                            Str::from(simple_name.clone()),
                            Str::from(descriptor.clone()),
                        ));
                        // add receiver as actual arg 0
                        args.insert(0, Exp::Variable(receiver.clone()));
                        let key = (cls.clone(), simple_name.clone(), descriptor.clone());
                        let action = self.classify(&key, *dispatch, super_start.as_ref());
                        self.apply(site, recv_var, &key, *dispatch, action);
                    }
                    CallStyle::LuaCall { receiver, method } => {
                        // A Lua receiver has no static type, so there is no declared class to key
                        // CHA on: the static resolvent set is every class method of this name
                        // across the recovered `__index` hierarchy, and the receiver's actual
                        // class comes from its allocation tag at analysis time. The receiver is
                        // already actual arg 0 (inserted by the frontend), so it is not
                        // re-inserted here.
                        let recv_var = self.trans_variable_ref(receiver);
                        let resolvents = self.cha.lua_resolvents_by_method(method);
                        // The empty descriptor is the sentinel the Lua CHA arm keys on; see
                        // [`Self::deferred_dispatch`].
                        let deferred_key = (method.clone(), Symbol::from(""));
                        // Deferred, context-sensitive resolution: the receiver's object facts
                        // (`call_target_assign`) join the Lua CHA `callee_resolvents` under this
                        // dispatch key at analysis time.
                        let deferred = (
                            site,
                            FlowVertex(recv_var, fx::Path::empty()),
                            fx::CallDispatchKey::Lua(method.clone()),
                        );
                        match self.strategy {
                            CallResolutionStrategy::Cha => {
                                log::trace!(
                                    "lua: CHA resolve {receiver}:{method} with {} targets",
                                    resolvents.len()
                                );
                                for target in &resolvents {
                                    let target = fx::Function(target.clone().into());
                                    let target = self.source_info.sites.get_or_add_function(target);
                                    self.facts.call.push((site, target));
                                }
                            }
                            CallResolutionStrategy::Hi => {
                                self.deferred_dispatch.insert(deferred_key);
                                self.facts.callee_info.push(deferred);
                                log::trace!("lua: HI resolve {receiver}:{method} (deferred)");
                            }
                            // The ladder is a Java construct: it keys on a static receiver
                            // class, a dispatch kind and a signature, and a Lua call has none
                            // of the three. Both mixed strategies get the same arm here.
                            CallResolutionStrategy::Mixed | CallResolutionStrategy::LegacyMixed => {
                                if resolvents.is_empty() {
                                    log::trace!("lua: no resolvents {receiver}:{method}");
                                } else {
                                    // Unlike the `JavaCall` arm above, an ambiguous Lua call keeps
                                    // its static edges instead of deferring to `callee_info`
                                    // alone. A `JavaCall` can defer safely because it carries a
                                    // declared receiver class, so hybrid inlining always has a
                                    // type to fall back on; a Lua receiver has none, and when no
                                    // dataflow reaches its allocation tag -- a module singleton,
                                    // a `self` handed in from opaque code, a computed metatable
                                    // -- the deferred path resolves to nothing at all and the
                                    // call site simply loses its callees. Measured on Prosody
                                    // 13.0.6 (examples/prosody): deferring alone drops
                                    // `--strategy mixed` from 2865 matched sinks / 806
                                    // tainted-path findings to 2145 / 263. So the sound CHA set
                                    // is always emitted, with `callee_info` on top to add the
                                    // context-sensitive resolution for the receivers whose class
                                    // the engine CAN reach.
                                    for target in &resolvents {
                                        let target = fx::Function(target.clone().into());
                                        let target =
                                            self.source_info.sites.get_or_add_function(target);
                                        self.facts.call.push((site, target));
                                    }
                                    // `callee_info` rides along even when the static set is a
                                    // singleton. The two are not redundant: a `call` edge is
                                    // resolved through the callee's summary, while the deferred
                                    // path instantiates it context-sensitively with a call
                                    // string, and each finds flows the other merges away.
                                    self.deferred_dispatch.insert(deferred_key);
                                    self.facts.callee_info.push(deferred);
                                    log::trace!(
                                        "lua: hybrid resolve {receiver}:{method} with {} target(s) + deferred",
                                        resolvents.len()
                                    );
                                }
                            }
                        }
                    }
                    CallStyle::FuncPtrCall { callee, .. } => {
                        let vertex = self.trans_access_path(callee);
                        self.facts
                            .callee_info
                            .push((site, vertex, fx::CallDispatchKey::C));
                    }
                    _ => log::warn!("unhandled call style: {style:?}"),
                }
                // pass parameters
                for (i, arg_exp) in args.iter().enumerate() {
                    // The negative half is reserved for the engine (returns at -1, -2, ...; globals
                    // at `GLOBALS_INDEX`), which is why this stops at `i16::MAX` rather than
                    // wrapping into it.
                    let Ok(formal_index) = FormalIndex::try_from(i) else {
                        log::warn!(
                            "found > {} parameters in function call; skipping rest",
                            i16::MAX
                        );
                        break;
                    };

                    if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = arg_exp {
                        let target = fx::Function(name.clone().into());
                        let target = self.source_info.sites.get_or_add_function(target);
                        let call_arg_packed = fx::PackedCallArg::try_from_parts(
                            fx::InsnSiteId::try_from(site).unwrap().insn_id,
                            formal_index,
                        )
                        .unwrap();
                        let call_arg_var = FlowVariable::call_arg_packed(call_arg_packed);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            fx::CallTargetObject::FunctionId(target),
                        ));
                        self.funcptr_targets.insert(target);
                    }

                    if let Some(object) = call_target_object(arg_exp) {
                        let call_arg_packed = fx::PackedCallArg::try_from_parts(
                            fx::InsnSiteId::try_from(site).unwrap().insn_id,
                            formal_index,
                        )
                        .unwrap();
                        let call_arg_var = FlowVariable::call_arg_packed(call_arg_packed);
                        self.facts.call_target_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            object,
                        ));
                    }

                    if let Exp::Str(value) = arg_exp {
                        let call_arg_packed = fx::PackedCallArg::try_from_parts(
                            fx::InsnSiteId::try_from(site).unwrap().insn_id,
                            formal_index,
                        )
                        .unwrap();
                        let call_arg_var = FlowVariable::call_arg_packed(call_arg_packed);
                        self.facts.const_str_assign.push((
                            site,
                            FlowVertex(call_arg_var, fx::Path::empty()),
                            Str::from(value.clone()),
                        ));
                    }

                    let Some(arg) = self.trans_exp(arg_exp) else {
                        continue;
                    };
                    self.facts.actual_param.push((site, formal_index, arg))
                }
                // pass return values
                // This will be bad if there are more than 32K return values
                for (i, ret) in rets.iter().enumerate().map(|(i, r)| (i + 1, r)) {
                    let i: i16 = i.try_into().unwrap();
                    let i = -i;
                    let ret = self.trans_variable_ref(ret);
                    self.facts.actual_param.push((
                        site,
                        i.into(),
                        FlowVertex(ret, fx::Path::empty()),
                    ));
                }
                // pass globals
                self.facts.actual_param.push((
                    site,
                    GLOBALS_INDEX.into(),
                    FlowVertex(
                        FlowVariable::formal_index(GLOBALS_INDEX.into()),
                        fx::Path::empty(),
                    ),
                ));
            }
            Load {
                dest,
                source,
                field,
            } => {
                let dest = self.trans_variable_ref(dest);
                // Re-anchor a read through a load-chain temporary onto the root composed path it
                // addresses, the read-side mirror of the `Store` arm.
                let (root_var, base_path) = self
                    .cap_path
                    .get(&source.base)
                    .cloned()
                    .unwrap_or_else(|| (source.base.clone(), fx::Path::empty()));
                let source_var = self.trans_variable_ref(&root_var);
                // The read path is the captured chain path, then the source's own (offset) address
                // arithmetic, then the loaded field.
                let path = fx::Path::from_accesses(
                    base_path
                        .iter()
                        .cloned()
                        .chain(source.accesses.iter().cloned().map(PathSegment::from))
                        .chain(std::iter::once(PathSegment::Symbol(
                            field.symbol_ref().clone(),
                        ))),
                );
                self.paths_dedup.insert((path,));
                // dest <- root.<composed path>
                self.facts.assign.push((
                    site,
                    FlowVertex(dest, fx::Path::empty()),
                    FlowVertex(source_var, path),
                ));
            }
            Store { dest, field, value } => {
                // Re-anchor a store through a load-chain temporary onto the formal path it
                // addresses: `v.f2.nf1.y = rhs` lowers to `t1 = load v.f2; t2 = load t1.nf1;
                // store t2.y := rhs`, and the write must be recorded at `v.f2.nf1.y`, not at the
                // temporary `t2.y` (which no summary can name). The pre-pass captured `t2 ↦
                // (v, .f2.nf1)`; compose that root + captured path with this store's offsets and
                // field. When the dest base is not a load-chain temporary, the root is the base
                // itself and the captured path is empty (an ordinary field/offset store).
                let (root_var, base_path) = self
                    .cap_path
                    .get(&dest.base)
                    .cloned()
                    .unwrap_or_else(|| (dest.base.clone(), fx::Path::empty()));
                let dest_var = self.trans_variable_ref(&root_var);
                // The written path is the captured chain path, then the dest's (offset) address
                // arithmetic, then the field.
                let path = fx::Path::from_accesses(
                    base_path
                        .iter()
                        .cloned()
                        .chain(dest.accesses.iter().cloned().map(PathSegment::from))
                        .chain(std::iter::once(PathSegment::Symbol(
                            field.symbol_ref().clone(),
                        ))),
                );
                self.paths_dedup.insert((path,));
                // dest.field <- value
                let dest = FlowVertex(dest_var, path);
                // A function pointer / Java object stored INTO A FIELD (`o.op = id`).
                // This is the field-store form of the `Assign` arm's object-ref handling:
                // record the store at its field path so indirect-call resolution can follow
                // it to the call site. Must run before `value` is lowered, since trans_exp()
                // returns None for an ObjectRef and would otherwise drop the binding (F1).
                if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = value {
                    let target = fx::Function(name.clone().into());
                    let target = self.source_info.sites.get_or_add_function(target);
                    self.facts.call_target_assign.push((
                        site,
                        dest.clone(),
                        fx::CallTargetObject::FunctionId(target),
                    ));
                    self.funcptr_targets.insert(target);
                }
                if let Some(object) = call_target_object(value) {
                    self.facts
                        .call_target_assign
                        .push((site, dest.clone(), object));
                }
                if let Exp::Str(value) = value {
                    self.facts.const_str_assign.push((
                        site,
                        dest.clone(),
                        Str::from(value.clone()),
                    ));
                }
                if let Some(value) = self.trans_exp(value) {
                    self.facts.assign.push((site, dest, value));
                }
            }
            Update {
                dest,
                source,
                field,
                value,
            } => {
                // A functional update `dest = update(source, dest.field := value)` lowers to two
                // flows: the whole-aggregate copy `dest <- source`, then the field write
                // `dest.field <- value`.
                let dest_var = self.trans_variable_ref(&dest.base);
                let source_var = self.trans_variable_ref(source);
                // The written path is the dest's (offset) address arithmetic, then the field.
                let path =
                    fx::Path::from_accesses(
                        dest.accesses.iter().cloned().map(PathSegment::from).chain(
                            std::iter::once(PathSegment::Symbol(field.symbol_ref().clone())),
                        ),
                    );
                self.paths_dedup.insert((path,));
                let dest_vertex = FlowVertex(dest_var, path);
                // A function pointer / Java object stored INTO A FIELD (`o.op = id`); mirrors the
                // `Store` arm.
                if let Exp::ObjectRef(CallObject::FunctionPtr(name)) = value {
                    let target = fx::Function(name.clone().into());
                    let target = self.source_info.sites.get_or_add_function(target);
                    self.facts.call_target_assign.push((
                        site,
                        dest_vertex.clone(),
                        fx::CallTargetObject::FunctionId(target),
                    ));
                    self.funcptr_targets.insert(target);
                }
                if let Exp::ObjectRef(CallObject::JavaObject(cls)) = value {
                    self.facts.call_target_assign.push((
                        site,
                        dest_vertex.clone(),
                        fx::CallTargetObject::Symbol(cls.0.clone()),
                    ));
                }
                // whole-aggregate copy: dest <- source
                self.facts.assign.push((
                    site,
                    FlowVertex(dest_var, fx::Path::empty()),
                    FlowVertex(source_var, fx::Path::empty()),
                ));
                // field write: dest.field <- value
                if let Some(value) = self.trans_exp(value) {
                    self.facts.assign.push((site, dest_vertex, value));
                }
            }
            Nop => (),
        }
    }

    // Generates assignments to aux formals from return instructions
    #[inline]
    fn visit_terminator_kind(&mut self, terminator: &TerminatorKind, location: Location) {
        self.super_terminator_kind(terminator, location);
        let site = {
            let insn_site_id = self.source_info.add_insn_site(self.function.unwrap());
            insn_site_id.try_into().unwrap()
        };
        if let TerminatorKind::Return { args } = terminator {
            // assigns for return values. This will be bad if there are more than 32K return values
            for (i, arg) in args.iter().enumerate().map(|(i, arg)| (i + 1, arg)) {
                let i: i16 = i.try_into().unwrap();
                let i = -i;
                let Some(src) = self.trans_exp(arg) else {
                    continue;
                };
                let dv = FlowVariable::formal_index(i.into());
                let dpath = fx::Path::empty();
                self.facts.assign.push((site, FlowVertex(dv, dpath), src));
            }
        }
    }

    // Generates access paths
    #[inline]
    fn visit_field_accesses(&mut self, fields: &OffsetAccesses) {
        self.super_field_accesses(fields);
        self.paths_dedup.insert((fields.into(),));
        if let Some(OffsetAccess::Offset(offset)) = fields.first() {
            // Insert just the first (offset) field to make sure we catch globals.
            let first_field = OffsetAccesses::with_offset(offset.0);
            self.paths_dedup.insert(((&first_field).into(),));
        }
    }
}

impl CodegenVisitor<'_> {
    /// Translate an expression into a flow vertex. If the expression is a constant, None is
    /// returned. Otherwise the vertex is returned.
    #[inline]
    fn trans_exp(&mut self, exp: &Exp) -> Option<FlowVertex> {
        match exp {
            Exp::Variable(v) => Some(FlowVertex(self.trans_variable_ref(v), fx::Path::empty())),
            // An address expression `x.[k]` flows structurally from x's offset field.
            Exp::AccessPath(ap) => Some(self.trans_access_path(ap)),
            // A constant is not a vertex. The taint index tracks flow between storage
            // locations, and a literal value is not one. These variants are listed out instead
            // of matched with `_`, so that adding an `Exp` variant fails to compile here and
            // someone has to decide what it should do.
            Exp::ObjectRef(_) | Exp::Str(_) | Exp::Bytes(_) | Exp::Int(_) => None,
        }
    }

    #[inline]
    fn trans_access_path(&mut self, ap: &AccessPath) -> FlowVertex {
        let v = self.trans_variable_ref(&ap.base);
        let fields = &ap.accesses;
        FlowVertex(v, fields.into())
    }

    #[inline]
    fn trans_variable_ref(&mut self, v: &VariableRef) -> FlowVariable {
        match (v.variable.as_ref(), v.version) {
            // The one global heap maps to the globals index
            (Variable::GlobalHeap, None) => FlowVariable::formal_index(GLOBALS_INDEX.into()),
            // A versioned global heap is a local variable
            (Variable::GlobalHeap, Some(version)) => {
                FlowVariable::local(Str::from(format!("$globals_{}", version)))
            }
            _ => v.try_into().unwrap(),
        }
    }
}

/// Which frontend's call-resolution scheme [`ClassHierarchyAnalysis::resolvents`] is keyed for.
/// The hierarchy computation itself ([`run_cha`]) is language-neutral; this only decides the
/// `(CallTargetObject, CallDispatchKey)` pair the resolvents are emitted under, so that a Lua
/// import and a JVM import sharing one fact base cannot collide in that key space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChaLanguage {
    #[default]
    Java,
    Lua,
}

/// `(class, method simple name, descriptor) -> targets`. The descriptor is a fixed empty
/// sentinel for [`ChaLanguage::Lua`], which has no overloading.
pub(crate) type ChaResolvents = BTreeMap<(Symbol, Symbol, Symbol), SmallVec<[Symbol; 4]>>;

/// A call site's static signature: `(class, method simple name, descriptor)`.
pub(crate) type SignatureKey = (Symbol, Symbol, Symbol);

/// What [`ClassHierarchyAnalysis::super_resolvent`] found by walking up from a start class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SuperResolution {
    /// One class in the nearest level declaring the method, and this is its implementation.
    Exactly(Symbol),
    /// No class at or above the start declares it: the hierarchy is incomplete, or the start
    /// class is not in this import.
    None,
    /// Several classes at the same level declare it, so the runtime's choice is not
    /// recoverable from the hierarchy alone.
    Ambiguous(usize),
}

#[derive(Debug, Default)]
pub(crate) struct ClassHierarchyAnalysis {
    language: ChaLanguage,
    resolvents: ChaResolvents,
    /// The same table under the RTA restriction: a target survives only if some class the
    /// program actually allocates inherits it. Empty unless built with [`Self::with_rta`],
    /// because codegen never asks for it and computing it is measurement, not resolution.
    rta_resolvents: ChaResolvents,
    /// Maps a signature key to the implementation its own class declares. Built from the
    /// `method_implemented` rows only: an abstract declaration has no body to resolve to.
    declared: hashbrown::HashMap<SignatureKey, Symbol>,
    /// `cls -> its direct parents` (superclass and super-interfaces, as the VMT `hierarchy`
    /// gives it). The upward half of what `run_cha` computes downward, kept small rather than
    /// materialized: `cha_super_method` is class x inherited method for the whole program.
    parents: hashbrown::HashMap<Symbol, SmallVec<[Symbol; 2]>>,
    /// Memo keyed by start signature. Bounded by distinct super-call signatures, about 1% of
    /// the virtual ones.
    super_memo: std::cell::RefCell<hashbrown::HashMap<SignatureKey, SuperResolution>>,
}

impl ClassHierarchyAnalysis {
    /// Plain CHA. What codegen uses; [`Self::rta_resolvents`] is left empty.
    fn new(vmt: &VirtualMethodTable, instantiated_classes: BTreeSet<Symbol>) -> Self {
        Self::build(vmt, instantiated_classes, false)
    }

    /// CHA *and* RTA, from one Datalog run over one shared subtype closure.
    pub(crate) fn with_rta(
        vmt: &VirtualMethodTable,
        instantiated_classes: BTreeSet<Symbol>,
    ) -> Self {
        Self::build(vmt, instantiated_classes, true)
    }

    fn build(vmt: &VirtualMethodTable, instantiated_classes: BTreeSet<Symbol>, rta: bool) -> Self {
        match vmt {
            VirtualMethodTable::Java {
                methods,
                hierarchy,
                interfaces,
                ..
            } => {
                let method_implemented: Vec<(Symbol, Symbol, Symbol, Symbol)> = methods
                    .iter()
                    .cloned()
                    .map(|(a, b, c, d)| (a.into(), b.into(), c.into(), d.into()))
                    .collect();
                let mut direct_superclass: Vec<(Symbol, Symbol)> = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| {
                        sups.into_iter()
                            .map(|sup| (sup.clone().into(), sub.clone().into()))
                    })
                    .collect();
                // Sort for determinism
                direct_superclass.sort_unstable();
                // Which types are interfaces, now that the frontends record it.
                let interface_type = interfaces.iter().map(|c| (c.clone().into(),)).collect();
                let super_interface = Default::default();
                let instantiated_classes_vec =
                    instantiated_classes.into_iter().map(|s| (s,)).collect();
                let declared = declared_implementations(&method_implemented);
                let parents = direct_parents(hierarchy);
                let (resolvents, rta_resolvents) = run_cha(
                    method_implemented,
                    direct_superclass,
                    interface_type,
                    super_interface,
                    instantiated_classes_vec,
                    rta,
                );
                Self {
                    language: ChaLanguage::Java,
                    resolvents,
                    rta_resolvents,
                    declared,
                    parents,
                    super_memo: Default::default(),
                }
            }
            // Lua mirrors the Java arm: a Lua method is a `method_implemented` with a fixed
            // empty descriptor sentinel, and each `__index` parent is a `direct_superclass`.
            // The shared `run_cha` then computes the `__index`-chain resolvents unchanged.
            VirtualMethodTable::Lua {
                methods, hierarchy, ..
            } => {
                let empty_desc = Symbol::from("");
                let method_implemented = methods
                    .iter()
                    .cloned()
                    .map(|(cls, name, id)| (cls, name, empty_desc.clone(), id))
                    .collect();
                let mut direct_superclass: Vec<(Symbol, Symbol)> = hierarchy
                    .iter()
                    .flat_map(|(sub, sups)| sups.iter().map(|sup| (sup.clone(), sub.clone())))
                    .collect();
                // Sort for determinism
                direct_superclass.sort_unstable();
                let instantiated_classes_vec =
                    instantiated_classes.into_iter().map(|s| (s,)).collect();
                let (resolvents, rta_resolvents) = run_cha(
                    method_implemented,
                    direct_superclass,
                    Default::default(),
                    Default::default(),
                    instantiated_classes_vec,
                    rta,
                );
                Self {
                    language: ChaLanguage::Lua,
                    resolvents,
                    rta_resolvents,
                    ..Default::default()
                }
            }
            _ => {
                log::warn!("CHA: unsupported virtual method table");
                Self::default()
            }
        }
    }

    pub(crate) fn language(&self) -> ChaLanguage {
        self.language
    }

    pub(crate) fn java_resolvents(
        &self,
        cls: Symbol,
        name: Symbol,
        descriptor: Symbol,
    ) -> impl ExactSizeIterator<Item = Symbol> + '_ {
        Self::lookup(&self.resolvents, cls, name, descriptor)
    }

    /// [`Self::java_resolvents`] under the RTA restriction. Empty unless this analysis was
    /// built by [`Self::with_rta`].
    pub(crate) fn java_rta_resolvents(
        &self,
        cls: Symbol,
        name: Symbol,
        descriptor: Symbol,
    ) -> impl ExactSizeIterator<Item = Symbol> + '_ {
        Self::lookup(&self.rta_resolvents, cls, name, descriptor)
    }

    fn lookup(
        table: &ChaResolvents,
        cls: Symbol,
        name: Symbol,
        descriptor: Symbol,
    ) -> impl ExactSizeIterator<Item = Symbol> + '_ {
        table
            .get(&(cls, name, descriptor))
            .map(|syms| syms.as_slice())
            .unwrap_or(&[])
            .iter()
            .cloned()
    }

    /// The deduplicated set of CHA targets for a Lua method name, unioned across every class in the
    /// recovered hierarchy. This is the *static* resolvent set for a Lua call site: unlike Java
    /// there is no declared receiver class to key on, so the name alone is all a purely static
    /// resolution has. (The `""` descriptor sentinel matches the Lua CHA arm.)
    ///
    /// A uniquely-named method — the common case — yields a singleton, which is an exact call
    /// edge. When the name is shared across unrelated classes the union is sound but imprecise,
    /// which is why [`CallResolutionStrategy::Mixed`] defers to `callee_info` instead of
    /// emitting it (see the [`CallStyle::LuaCall`] codegen arm).
    pub(crate) fn lua_resolvents_by_method(&self, method: &Symbol) -> BTreeSet<Symbol> {
        Self::lua_lookup(&self.resolvents, method)
    }

    /// [`Self::lua_resolvents_by_method`] under the RTA restriction. Empty unless this
    /// analysis was built by [`Self::with_rta`].
    pub(crate) fn lua_rta_resolvents_by_method(&self, method: &Symbol) -> BTreeSet<Symbol> {
        Self::lua_lookup(&self.rta_resolvents, method)
    }

    /// The single method a `super`-dispatched call starting at `start` reaches, if the
    /// hierarchy determines one.
    ///
    /// Breadth-first up [`Self::parents`], level 0 being `start` itself, collecting the
    /// implementations declared at the first level where any class declares the signature and
    /// stopping there. That is JVM and Dalvik lookup order, and it covers both super cases: a
    /// superclass target, where the walk climbs the class chain, and an interface default
    /// method for `X.super.m()`, where `X` declares it and level 0 is the answer.
    ///
    /// A walk that finds nothing returns [`SuperResolution::None`] and the caller falls back to
    /// the full CHA resolvent set. It never hands back an empty target set that CHA would have
    /// filled: resolving `super` is a precision change, and this is what keeps it from also
    /// being a soundness one.
    pub(crate) fn super_resolvent(
        &self,
        start: &Symbol,
        name: &Symbol,
        descriptor: &Symbol,
    ) -> SuperResolution {
        let key = (start.clone(), name.clone(), descriptor.clone());
        if let Some(cached) = self.super_memo.borrow().get(&key) {
            return cached.clone();
        }
        let result = self.walk_super(start, name, descriptor);
        self.super_memo.borrow_mut().insert(key, result.clone());
        result
    }

    fn walk_super(&self, start: &Symbol, name: &Symbol, descriptor: &Symbol) -> SuperResolution {
        let mut level: Vec<Symbol> = vec![start.clone()];
        let mut seen: BTreeSet<Symbol> = level.iter().cloned().collect();
        while !level.is_empty() {
            let found: SmallVec<[Symbol; 2]> = level
                .iter()
                .filter_map(|cls| {
                    self.declared
                        .get(&(cls.clone(), name.clone(), descriptor.clone()))
                        .cloned()
                })
                .collect();
            match found.len() {
                0 => {}
                1 => return SuperResolution::Exactly(found.into_iter().next().unwrap()),
                n => return SuperResolution::Ambiguous(n),
            }
            level = level
                .iter()
                .filter_map(|cls| self.parents.get(cls))
                .flatten()
                .filter(|parent| seen.insert((*parent).clone()))
                .cloned()
                .collect();
        }
        SuperResolution::None
    }

    fn lua_lookup(table: &ChaResolvents, method: &Symbol) -> BTreeSet<Symbol> {
        table
            .iter()
            .filter(|((_cls, name, _desc), _)| name == method)
            .flat_map(|(_, targets)| targets.iter().cloned())
            .collect()
    }
}

/// Maps each signature key to the implementation its own class declares, from the same
/// `method_implemented` rows [`run_cha`] takes. A class declaring one signature twice -- the
/// same class in two dex files of one app -- keeps the first, since both rows name the same
/// method id.
fn declared_implementations(
    method_implemented: &[(Symbol, Symbol, Symbol, Symbol)],
) -> hashbrown::HashMap<SignatureKey, Symbol> {
    let mut declared = hashbrown::HashMap::new();
    for (cls, name, desc, id) in method_implemented {
        declared
            .entry((cls.clone(), name.clone(), desc.clone()))
            .or_insert_with(|| id.clone());
    }
    declared
}

/// The VMT hierarchy read in the direction super resolution walks: subclass to its direct
/// parents, superclass and super-interfaces together.
fn direct_parents(
    hierarchy: &hashbrown::HashMap<
        ctadl_ir::mir::call::JavaClass,
        SmallVec<[ctadl_ir::mir::call::JavaClass; 2]>,
    >,
) -> hashbrown::HashMap<Symbol, SmallVec<[Symbol; 2]>> {
    hierarchy
        .iter()
        .map(|(sub, sups)| (sub.0.clone(), sups.iter().map(|s| s.0.clone()).collect()))
        .collect()
}

/// The function a modelled signature's summary hangs off.
///
/// One per matched key, not per generator or per site: per-key is what makes `Argument(*)`
/// well defined, since the descriptor fixes the arity. The `ctadl$dispatch$` prefix cannot
/// collide with a dex or jvm method id, both of which begin with `L`.
pub fn synthetic_dispatch_function(key: &SignatureKey) -> String {
    format!("ctadl$dispatch${}->{}{}", key.0, key.1, key.2)
}

/// Emits the `callee_resolvents` rows for a completed CHA, under the language's own
/// `(CallTargetObject, CallDispatchKey)` pair: `(Symbol(cls), Java(name, desc))` for JVM/Dex,
/// `(LuaClass(cls), Lua(name))` for Lua. The `""` descriptor the Lua CHA arm feeds `run_cha`
/// stays an implementation detail of that arm and never reaches a fact.
///
/// `wanted` holds the `(name, descriptor)` pairs some site deferred to hybrid inlining. Those
/// are the only pairs the engine's resolution rule can join, so a row under any other pair is
/// dead weight -- on a run where the ladder defers a couple of percent of sites, almost all of
/// them. The *class* is not filtered: the join is on the receiver's allocated class, which is
/// not the one the deferred site names.
fn emit_callee_resolvents(
    cha: &ClassHierarchyAnalysis,
    wanted: &BTreeSet<(Symbol, Symbol)>,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    for ((cls, name, desc), targets) in &cha.resolvents {
        if !wanted.contains(&(name.clone(), desc.clone())) {
            continue;
        }
        let (object, key) = match cha.language {
            ChaLanguage::Java => (
                fx::CallTargetObject::Symbol(cls.clone()),
                fx::CallDispatchKey::Java(name.clone(), desc.clone()),
            ),
            ChaLanguage::Lua => (
                fx::CallTargetObject::LuaClass(cls.clone()),
                fx::CallDispatchKey::Lua(name.clone()),
            ),
        };
        for target in targets {
            let func_id = source_info
                .sites
                .get_or_add_function(fx::Function(target.clone().into()));
            facts
                .callee_resolvents
                .push((object.clone(), key.clone(), func_id));
        }
    }
}

/// Runs the class hierarchy analysis, and -- when `rta` is set -- the rapid type analysis
/// beside it. One [`ascent::ascent_run!`] computes both. Returns `(cha, rta)`; the second is
/// empty when `rta` is false.
pub(crate) fn run_cha(
    method_implemented: Vec<(Symbol, Symbol, Symbol, Symbol)>,
    direct_superclass: Vec<(Symbol, Symbol)>,
    interface_type: Vec<(Symbol,)>,
    super_interface: Vec<(Symbol, Symbol)>,
    instantiated_classes: Vec<(Symbol,)>,
    rta: bool,
) -> (ChaResolvents, ChaResolvents) {
    let prog = ascent::ascent_run! {
        // input relations
        relation method_implemented(Symbol, Symbol, Symbol, Symbol) = method_implemented;
        relation interface_type(Symbol) = interface_type;
        relation super_interface(Symbol, Symbol) = super_interface;
        // sup, sub
        relation direct_superclass(Symbol, Symbol) = direct_superclass;
        relation instantiated_class(Symbol) = instantiated_classes;

        // internal relations
        relation cha_direct_subtype(Symbol, Symbol);
        relation cha_subtype(Symbol, Symbol);
        relation cha_subtype_reflexive(Symbol, Symbol);
        // maps triple to methods (inherited)
        relation cha_super_method(Symbol, Symbol, Symbol, Symbol);
        // output: static type resolves to possible methods
        relation cha_resolve(Symbol, Symbol, Symbol, Symbol);
        // output: the same, restricted to methods some *allocated* class inherits
        relation rta_resolve(Symbol, Symbol, Symbol, Symbol);

        cha_direct_subtype(sub, sup) <-- direct_superclass(sup, sub);
        cha_direct_subtype(cls, iface) <-- super_interface(iface, cls), !interface_type(cls);
        cha_subtype(sub, sup) <-- cha_direct_subtype(sub, sup);
        cha_subtype(sub, sup) <-- cha_subtype(sub, mid), cha_direct_subtype(mid, sup);

        relation class_or_interface(Symbol);
        class_or_interface(c) <-- method_implemented(c, _, _, _);
        class_or_interface(c) <-- direct_superclass(c, _);
        class_or_interface(c) <-- direct_superclass(_, c);
        class_or_interface(c) <-- interface_type(c);
        class_or_interface(c) <-- super_interface(c, _);
        class_or_interface(c) <-- super_interface(_, c);
        class_or_interface(c) <-- instantiated_class(c);

        cha_subtype_reflexive(c, c) <-- class_or_interface(c);
        cha_subtype_reflexive(sub, sup) <-- cha_subtype(sub, sup);

        cha_super_method(c, m, d, id) <-- method_implemented(c, m, d, id);
        cha_super_method(c, m, d, id) <--
            cha_super_method(c2, m, d, id),
            cha_direct_subtype(c, c2),
            !method_implemented(c, m, d, _);

        cha_resolve(sup, m, d, id) <--
            cha_super_method(sub, m, d, id),
            cha_subtype_reflexive(sub, sup);

        // RTA: the same, but only where the subtype carrying the method is one the program
        // actually allocates.
        rta_resolve(sup, m, d, id) <--
            if rta,
            cha_super_method(sub, m, d, id),
            cha_subtype_reflexive(sub, sup),
            instantiated_class(sub);
    };
    (collect(prog.cha_resolve), collect(prog.rta_resolve))
}

/// Folds a resolve relation into the `(class, name, descriptor) -> targets` map, sorted so
/// the result does not depend on derivation order.
fn collect(mut rows: Vec<(Symbol, Symbol, Symbol, Symbol)>) -> ChaResolvents {
    rows.sort_unstable();
    let mut result = ChaResolvents::new();
    for (c, n, d, id) in rows {
        log::trace!("Adding entry: {c}, {n}, {d} -> {id}");
        result.entry((c, n, d)).or_default().push(id);
    }
    result
}
