/*! The call-graph measurements.

We walk the imported IR once, run CHA once, and compute everything else by joining the two.

The walk keeps little. [`ClassHierarchyAnalysis`] is keyed by signature
`(class, name, descriptor)` rather than by call site, so every `Object.equals` site in the
program shares one target set. Keeping a row per site would mean millions of identical rows
on a large app, and a per-site top-10 would print ten copies of one line. So the walk stores
one entry per distinct signature along with how many sites use it. Every per-site number
below is that table expanded by those site counts. See [`crate::stats`], whose helpers take
`(value, weight)` pairs for this reason.

Everything here is the static tier: it needs an import and nothing else. No index is read.
*/

use hashbrown::HashMap;
use serde::Serialize;

use crate::codegen::{ChaLanguage, ClassHierarchyAnalysis, InstantiationFinder};
use crate::stats::{Distribution, top_n_share};
use ctadl_ir::graph::{DirectedGraph, Successors, scc::Sccs};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::visit::Visitor;
use ctadl_ir::mir::{
    FunctionIdx, ProgramInfo, StatementKind, Symbol,
    call::{CallStyle, JavaDispatch, TypeFacts},
};

/// A method's simple name and descriptor, matched exactly. Obfuscation renames and
/// repackages classes, but an `equals(Ljava/lang/Object;)Z` override still has to keep the
/// name and descriptor the JVM dispatches on. So this match survives obfuscation.
const HARD_CASES: &[(&str, &str)] = &[
    ("equals", "(Ljava/lang/Object;)Z"),
    ("hashCode", "()I"),
    ("toString", "()Ljava/lang/String;"),
];

/// Declared receiver types that mark a call as a Kotlin lambda or functional-interface call.
/// The arity digits are stripped, so `Function0` through `FunctionN` are one case.
///
/// Both spellings show up in real dex type pools, so we match both.
/// `kotlin.jvm.functions.FunctionN` is the interface carrying `invoke`, and
/// `kotlin.FunctionN` is the marker interface above it.
///
/// Neither spelling is a guarantee. An obfuscated app repackages the interface and its
/// method, so the lambda call site names some arbitrary type and no type list can find it.
/// That is why [`KotlinLambdas`] reports the method-name test beside this one instead of
/// relying on either alone.
const KOTLIN_FUNCTION_PREFIXES: &[&str] = &["kotlin/jvm/functions/Function", "kotlin/Function"];

/// Method names a Kotlin lambda body is invoked through.
const KOTLIN_INVOKE_NAMES: &[&str] = &["invoke", "invokeSuspend"];

/// Which resolution scheme the program's virtual calls use. This decides which sections of
/// the report apply. A Pcode or C import has no class hierarchy, so it gets the call census
/// and nothing else. Printing zeros for the other sections would look like findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Java,
    Lua,
    /// No class hierarchy (pcode, C, flowy).
    Other,
}

// --- the report ------------------------------------------------------------

/// Everything the static tier measures. Sections that do not apply to the program are left
/// out of the JSON rather than reported as zero. See [`Language`].
#[derive(Debug, Serialize)]
pub struct CallGraphReport {
    /// Which tier produced this. Always `"static"` in this version, since no index is read.
    pub tier: &'static str,
    /// The import the numbers describe.
    pub import: String,
    pub language: Language,
    pub functions: usize,
    pub census: Census,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_targets: Option<VirtualTargets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_signatures: Option<WorstSignatures>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rta: Option<RtaComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hard_cases: Option<Vec<NamedCase>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kotlin_lambdas: Option<KotlinLambdas>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functional_interfaces: Option<FunctionalInterfaces>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicySection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_in: Option<FanIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursion: Option<Recursion>,
}

/// One number per [`JavaDispatch`] kind.
///
/// Interface calls are counted separately from class-virtual ones everywhere. CHA resolves
/// an interface call much more loosely, since it admits every unrelated class that
/// implements the interface. An average over the two describes neither.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct DispatchCensus {
    /// `invoke-virtual`. `virtual` is a Rust keyword, so the field carries the trailing
    /// underscore and the JSON does not.
    #[serde(rename = "virtual")]
    pub virtual_: usize,
    /// `invoke-interface`.
    pub interface: usize,
    /// `invoke-super`. Its target is fixed at the named class, but CTADL resolves it as an
    /// ordinary virtual call. Counted separately to show what that costs.
    #[serde(rename = "super")]
    pub super_: usize,
    /// The frontend recorded no dispatch instruction. This happens for a JVM
    /// `invokedynamic` with a receiver, or for a synthesised call. It is a gap in the data
    /// rather than a kind of dispatch, and is reported as such.
    pub unknown: usize,
}

impl DispatchCensus {
    fn from_counts(counts: &[usize; 4]) -> Self {
        Self {
            virtual_: counts[JavaDispatch::Virtual.index()],
            interface: counts[JavaDispatch::Interface.index()],
            super_: counts[JavaDispatch::Super.index()],
            unknown: counts[JavaDispatch::Unknown.index()],
        }
    }

    pub fn total(&self) -> usize {
        self.virtual_ + self.interface + self.super_ + self.unknown
    }
}

/// Section 1: how many call sites there are, by kind.
///
/// `virtual` counts every `JavaCall`. [`Self::by_dispatch`] splits that into the three
/// dispatch instructions the frontends record. `invoke-interface` and `invoke-virtual` share
/// one `CallStyle` and one resolution path, but they behave differently enough that they are
/// worth measuring separately.
#[derive(Debug, Default, Serialize)]
pub struct Census {
    pub total: usize,
    /// Target named in the instruction: `invoke-static` and `invoke-direct` on Dex.
    pub direct: usize,
    /// Target depends on the receiver's runtime type. Virtual, super and interface calls.
    /// `virtual` is a Rust keyword, so the field carries the trailing underscore and the
    /// JSON does not.
    #[serde(rename = "virtual")]
    pub virtual_: usize,
    /// [`Self::virtual_`] split by dispatch instruction. Absent for a program whose virtual
    /// calls are not Java ones (Lua has no dispatch kinds to split by).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_dispatch: Option<DispatchCensus>,
    /// C function-pointer calls.
    pub func_ptr: usize,
    /// Lua `recv:m(...)`.
    pub lua: usize,
    /// A call the frontend could not classify.
    pub unknown: usize,
}

/// Sections 2, 3, 4 and 6: how many methods a virtual call could reach.
#[derive(Debug, Serialize)]
pub struct VirtualTargets {
    /// Distinct `(class, name, descriptor)` signatures across every virtual site.
    pub signatures: usize,
    /// Virtual call sites. Equals [`Census::virtual_`] (plus [`Census::lua`] for Lua).
    pub sites: usize,
    /// Sum of target counts over sites. Counts one (site, target) pair per unit, which is
    /// what a client of the call graph pays for.
    pub total_edges: usize,
    /// Targets per site, weighted by how many sites share each signature.
    pub targets_per_site: Distribution,
    /// Sites CHA resolves to exactly one method. These are the cheap ones, and the
    /// percentage says how much of the work is already done.
    pub sites_with_one_target: usize,
    /// Sites CHA resolves to nothing. Under the default `mixed` strategy codegen drops
    /// these silently, emitting no edge and no `callee_info`. This count is therefore where
    /// the call graph is unsound: missing library code, native methods, or reflection.
    pub sites_with_zero_targets: usize,
    /// Sites with two or more targets: what `mixed` hands to hybrid inlining.
    pub sites_deferred_to_hybrid_inlining: usize,
    /// The same numbers again, computed separately over the sites of each dispatch kind.
    ///
    /// This is what the interface/class-virtual split exists for. Pooled together, a
    /// program's `targets_per_site` averages two populations that behave differently, and
    /// the pooled percentiles describe neither. A kind with no sites in this program is left
    /// out rather than reported as a row of zeros. Empty for a non-Java program.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_dispatch: Vec<DispatchTargets>,
}

/// [`VirtualTargets`] restricted to the call sites of one dispatch kind.
///
/// Every count covers those sites alone. The target sets themselves do not change, because
/// CHA is keyed by signature and knows nothing about how a site dispatches. What differs
/// between the kinds is which signatures they reach and how often.
#[derive(Debug, Serialize)]
pub struct DispatchTargets {
    pub dispatch: &'static str,
    /// Signatures with at least one site of this kind. A signature dispatched both ways,
    /// such as an `invoke-super` and an `invoke-virtual` naming one method, counts in both.
    pub signatures: usize,
    pub sites: usize,
    pub total_edges: usize,
    /// `sites x (targets - 1)`, summed. These are the edges that exist because resolution
    /// is imprecise, rather than because the call is there. See [`WorstSignatures`].
    pub excess_edges: usize,
    pub targets_per_site: Distribution,
    pub sites_with_one_target: usize,
    pub sites_with_zero_targets: usize,
    pub sites_deferred_to_hybrid_inlining: usize,
}

/// Sections 3 and 5: the worst signatures, and how much of the imprecision they account for.
///
/// "Worst" can mean three things here, and they answer different questions.
///
/// **By site** is what `intent.md` asks for: the fraction of all call edges owned by the ten
/// individual call instructions with the most targets. On a large app this fraction is
/// necessarily tiny, since ten instructions out of millions cannot be much. That it is tiny
/// is itself the result, not a measurement failure.
///
/// **By target count** finds the least precise signature, such as an `Object.toString` with
/// hundreds of possible targets. This is what the intent means by "the call sites with the
/// most resolvents". On its own it over-weights a bad signature that is called from only one
/// place.
///
/// **By excess** is the one to act on. Every resolved call site needs at least one target,
/// and that edge is the call itself rather than imprecision. What imprecision costs is the
/// rest: `sites x (targets - 1)`, summed. A signature with one target contributes zero
/// however often it is called. Ranking by raw edge count instead would put something like
/// `StringBuilder.append` at the top of every small app, even though it has exactly one
/// target and no special case could improve it.
#[derive(Debug, Serialize)]
pub struct WorstSignatures {
    /// Least precise: ranked by target count.
    pub top_by_targets: Vec<SignatureRow>,
    /// Ranked by [`SignatureRow::excess`]. These make up the excess shares below, and they
    /// are what one would special-case.
    pub top_by_excess: Vec<SignatureRow>,
    /// Call edges beyond the one each resolved site must have: `total_edges` minus the
    /// sites that resolved to anything. This is the size of the problem.
    pub excess_edges: usize,
    /// Share of all call edges owned by the 10 worst sites. Computed by ranking the
    /// signature list by target count, expanding it by site counts, and cutting at 10.
    pub top_10_site_share: f64,
    pub top_100_site_share: f64,
    /// Share of [`Self::excess_edges`] owned by the 10 and 100 signatures contributing the
    /// most of it. This decides whether special-casing a few signatures beats making the
    /// whole analysis more precise.
    pub top_10_signature_excess_share: f64,
    pub top_100_signature_excess_share: f64,
    /// The same ranking done again within each dispatch kind. Empty for a non-Java program.
    ///
    /// Worth reporting separately because what one would special-case differs by kind, and
    /// pooling hides that. The interface list holds entries like `Iterator.next` and
    /// `Runnable.run`, while the class-virtual list holds entries like `Object.toString` and
    /// `hashCode`. A single pooled top ten is neither list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_dispatch: Vec<DispatchWorst>,
}

/// [`WorstSignatures`] restricted to one dispatch kind.
#[derive(Debug, Serialize)]
pub struct DispatchWorst {
    pub dispatch: &'static str,
    /// Excess from sites of this kind. The kinds partition the sites, so these sum to
    /// [`WorstSignatures::excess_edges`].
    pub excess_edges: usize,
    /// Shares of *this kind's* excess, not of the program's.
    pub top_10_signature_excess_share: f64,
    pub top_100_signature_excess_share: f64,
    /// Ranked by the excess this kind's sites contribute.
    pub top_by_excess: Vec<SignatureRow>,
}

/// One `(class, name, descriptor)` signature, with what it costs.
///
/// In a list restricted to one dispatch kind, [`Self::sites`] and everything derived from it
/// count that kind's sites only. [`Self::sites_by_dispatch`] is the exception: it always
/// describes the whole program, so a row taken from the interface list still says how the
/// signature is called everywhere else.
#[derive(Debug, Serialize)]
pub struct SignatureRow {
    pub class: String,
    pub name: String,
    pub descriptor: String,
    /// Call sites that dispatch on this signature, over whatever population this row's list
    /// covers.
    pub sites: usize,
    /// Every site in the program dispatching on this signature, split by kind. Always the
    /// whole program; see the type's note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sites_by_dispatch: Option<DispatchCensus>,
    /// Whether the declared receiver type is a class this import declares `interface`.
    /// `null` when the import declares no interfaces at all, such as a Lua program or a dex
    /// that ships none. That keeps "not an interface" distinct from "nothing to compare
    /// against".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_is_known_interface: Option<bool>,
    /// Methods CHA says one such site could reach.
    pub cha_targets: usize,
    /// The same under RTA. A lower bound; see [`RtaComparison`].
    pub rta_targets: usize,
    /// `sites * cha_targets`: this signature's share of the total edge count.
    pub edges: usize,
    /// `sites * (cha_targets - 1)`, or zero when there are no targets. These are the edges
    /// that exist only because resolution is imprecise here. Zero for a signature with one
    /// target.
    pub excess: usize,
}

/// Section 7: what restricting to allocated types buys.
///
/// **RTA here is a lower bound, not a more precise answer.** The set of allocated classes
/// comes only from `new`-like expressions in imported code. An object created by a library
/// we did not import, by reflection, or by deserialization is invisible, so RTA drops
/// targets that are genuinely reachable. These numbers say how much CHA over-approximates
/// relative to what this import can see. They are a measurement, not a resolution strategy.
#[derive(Debug, Serialize)]
pub struct RtaComparison {
    /// Distinct classes the imported code allocates.
    pub allocated_classes: usize,
    pub cha_edges: usize,
    pub rta_edges: usize,
    /// `cha_edges - rta_edges`: (site, target) pairs RTA throws away.
    pub edges_dropped: usize,
    /// Per-site gap `cha - rta`, weighted by sites.
    pub gap_per_site: Distribution,
    /// The same three edge totals per dispatch kind. Empty for a non-Java program.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_dispatch: Vec<DispatchRta>,
}

/// [`RtaComparison`] restricted to one dispatch kind.
#[derive(Debug, Serialize)]
pub struct DispatchRta {
    pub dispatch: &'static str,
    pub cha_edges: usize,
    pub rta_edges: usize,
    pub edges_dropped: usize,
}

/// Section 8: one named hard case, aggregated over every class that declares it.
#[derive(Debug, Serialize)]
pub struct NamedCase {
    pub name: String,
    pub descriptor: String,
    /// Distinct declared receiver classes this was called on.
    pub signatures: usize,
    pub sites: usize,
    /// Those sites by dispatch kind. `equals` and `hashCode` are declared on `Object`, so
    /// they are class-virtual almost everywhere. A large interface count here means the app
    /// calls them through an interface type, which CHA resolves much more loosely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sites_by_dispatch: Option<DispatchCensus>,
    pub total_edges: usize,
    pub max_targets: usize,
}

/// Section 11: Kotlin lambda call sites, found two different ways.
///
/// Neither test is reliable on its own, and we have observed both fail. On one large app the
/// receiver-type test found 31,622 sites while the method-name test found 57,383, so the
/// type test missed over 25,000 real ones. On another app the type test found none at all:
/// its type pool carried `Lkotlin/Function0;`, but no call site dispatched on it, because
/// the obfuscator had repackaged the functional interfaces along with their methods. The
/// sites the name test found there are a floor rather than a count.
///
/// Reporting both tests and where they disagree makes that visible as data, rather than as a
/// zero with no explanation.
#[derive(Debug, Serialize)]
pub struct KotlinLambdas {
    /// Sites whose declared receiver type is a `kotlin` `FunctionN`.
    pub sites_by_receiver_type: usize,
    /// Sites whose method name is `invoke` or `invokeSuspend`, whatever the receiver.
    pub sites_by_method_name: usize,
    /// Sites both discriminators agree on.
    pub sites_by_both: usize,
    /// Receiver-type sites the name test misses, and name sites the type test misses.
    pub receiver_type_only: usize,
    pub method_name_only: usize,
    /// Of the receiver-type sites, how many CHA resolves to exactly one body -- the ones a
    /// lambda-aware analysis would already get right.
    pub receiver_type_sites_with_one_target: usize,
    /// The receiver types actually matched, so that a stale prefix list shows up here
    /// instead of going unnoticed.
    pub matched_receiver_types: Vec<String>,
    /// The receiver-type-matched sites by dispatch kind. A Kotlin lambda call is an
    /// `invoke-interface` on `FunctionN`, so anything else here is worth a look.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type_sites_by_dispatch: Option<DispatchCensus>,
}

/// Section 12: functional interfaces in general, not only Kotlin's.
///
/// A functional interface declares exactly one abstract method. That is the shape a lambda,
/// a method reference, or a `Runnable` compiles against. Recognising them needs two facts
/// the IR did not carry before this phase: which types are interfaces, and which of their
/// methods are abstract. Neither can be derived from the resolvent map, because its entries
/// for an interface hold every method of every implementer rather than the interface's own.
///
/// Unlike [`KotlinLambdas`], this needs no name list and no package prefix, so it survives
/// obfuscation. A renamed interface with one abstract method still has one abstract method.
///
/// What it cannot see is an interface the import does not declare, such as
/// `java/util/Iterator` in an app that does not ship the framework.
/// [`Self::interface_sites_on_unknown_type`] measures that gap, which is why it is reported
/// alongside the rest.
#[derive(Debug, Serialize)]
pub struct FunctionalInterfaces {
    /// Types this import declares `interface`.
    pub interfaces_declared: usize,
    /// Of those, the ones declaring exactly one abstract method.
    pub single_abstract_method: usize,
    /// Call sites whose declared receiver type is one of those.
    pub sites: usize,
    /// Of those sites, the ones CHA resolves to exactly one body: what a lambda-aware
    /// analysis would already get right.
    pub sites_with_one_target: usize,
    pub total_edges: usize,
    pub excess_edges: usize,
    /// `invoke-interface` sites whose declared receiver type this import does not declare.
    /// This is the coverage gap. On an app that does not ship the framework, most interface
    /// calls name a type this import has never seen, and every measurement above covers only
    /// the remainder.
    pub interface_sites_on_unknown_type: usize,
    /// The functional-interface signatures contributing the most excess.
    pub top: Vec<SignatureRow>,
}

/// Section 13: what the call-resolution policy would do to this program.
///
/// Simulates the [`CallResolutionStrategy::Mixed`] ladder over the complete key table without
/// indexing anything, so a user can write dispatch models against their own app and see the
/// effect in seconds. [`Self::top_inlined`] and [`Self::unmodelled_closures`] are what that
/// loop reads.
///
/// [`CallResolutionStrategy::Mixed`]: crate::codegen::CallResolutionStrategy
#[derive(Debug, Serialize)]
pub struct PolicySection {
    /// The policy simulated, as `ctadl index` would record it.
    pub cha_threshold: usize,
    pub cha_threshold_interface: usize,
    pub order: String,
    pub buckets: crate::codegen::SiteBuckets,
    pub by_dispatch: Vec<DispatchBuckets>,
    /// Edges plain CHA would emit, and edges the policy emits. The ratio is the headline.
    pub cha_edges: usize,
    pub policy_edges: usize,
    /// Left on hybrid inlining, ranked by excess, each row saying what put it there. What a
    /// user writes dispatch models against.
    pub top_inlined: Vec<InlinedSignatureRow>,
    /// Matched by a dispatch model, ranked by the excess it removes.
    pub top_modelled: Vec<ModelledSignatureRow>,
    /// Dispatch models a matched source or sink refused. Empty unless endpoint-declaring model
    /// files were given.
    pub refused: Vec<RefusedSignatureRow>,
    /// Closure-shaped -- single-abstract-method receiver, or a name on the shipped list -- and
    /// named by no model. The per-app profiling a fixed list leaves benefit on the table for.
    pub unmodelled_closures: Vec<SignatureRow>,
}

/// [`PolicySection::buckets`] restricted to one dispatch kind.
#[derive(Debug, Serialize)]
pub struct DispatchBuckets {
    pub dispatch: &'static str,
    #[serde(flatten)]
    pub buckets: crate::codegen::SiteBuckets,
}

/// A signature the policy leaves on hybrid inlining.
#[derive(Debug, Serialize)]
pub struct InlinedSignatureRow {
    #[serde(flatten)]
    pub signature: SignatureRow,
    /// `"threshold"` or `"model"`. A mis-scoped `resolve: inline` entry looks exactly like a
    /// signature that is genuinely too wide, so the row says which it is.
    pub reason: &'static str,
    /// `file:generator-index` of the entry that deferred it, when a model did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<String>,
}

/// A signature a dispatch model covers.
#[derive(Debug, Serialize)]
pub struct ModelledSignatureRow {
    #[serde(flatten)]
    pub signature: SignatureRow,
    /// `"model"` for a propagation, `"skip"` for an empty one.
    pub disposition: &'static str,
    pub provenance: Vec<String>,
}

/// A signature whose dispatch model was refused because its targets hold a matched endpoint.
#[derive(Debug, Serialize)]
pub struct RefusedSignatureRow {
    #[serde(flatten)]
    pub signature: SignatureRow,
    /// The source or sink inside the target set. The reason those bodies have to stay in the
    /// analysis, and so the reason the site takes CHA.
    pub endpoint: String,
    pub provenance: Vec<String>,
}

/// Simulates the ladder over the whole key table.
///
/// Built from the complete table, not from [`WorstSignatures::top_by_excess`], which drops
/// every zero-excess signature -- thousands of them on a real app. Site percentages computed
/// from that list would be wrong.
fn policy_section(
    w: &Walk,
    targets: &Targets,
    types: &TypeFacts,
    cha: &ClassHierarchyAnalysis,
    matches: &crate::models::ProgramModelMatches,
    opts: super::ReportOptions,
) -> PolicySection {
    use crate::codegen::{DispatchDisposition, DispatchOrder, Rung, SiteBuckets};
    use crate::models::Disposition;

    let policy = opts.call_policy;
    let endpoints: std::collections::HashSet<&str> = matches
        .endpoints
        .iter()
        .map(|e| e.function.as_ref())
        .collect();
    // How many super sites of each key the hierarchy resolves exactly. Rung 0 precedes
    // everything else, so those sites never reach the model or the threshold.
    let mut super_exact: HashMap<u32, usize> = HashMap::new();
    for ((k, start), count) in &w.super_starts {
        let (_, name, desc) = &w.keys[*k as usize];
        if let crate::codegen::SuperResolution::Exactly(_) = cha.super_resolvent(start, name, desc)
        {
            *super_exact.entry(*k).or_default() += count;
        }
    }

    let mut buckets = [SiteBuckets::default(); 4];
    let mut cha_edges = 0usize;
    let mut policy_edges = 0usize;
    // `(excess, key id, disposition flag)`; the rows themselves are built after ranking.
    let mut inlined: Vec<(usize, usize, bool)> = Vec::new();
    let mut modelled: Vec<(usize, usize, bool)> = Vec::new();
    let mut refused: Vec<RefusedSignatureRow> = Vec::new();
    let mut unmodelled: Vec<(usize, usize)> = Vec::new();

    for k in 0..w.keys.len() {
        let (cls, name, desc) = &w.keys[k];
        let count = targets.cha[k].len();
        cha_edges += count * w.sites(k);
        let model = matches.dispatch.get(&(
            crate::facts::Str::from(cls.as_ref()),
            crate::facts::Str::from(name.as_ref()),
            crate::facts::Str::from(desc.as_ref()),
        ));
        // The rung-1 refusal, over the same direct target set codegen intersects.
        let endpoint = match model.map(|m| &m.disposition) {
            Some(Disposition::Model(_)) | Some(Disposition::Skip) => targets.cha[k]
                .iter()
                .map(|node| w.nodes[*node as usize].as_ref())
                .find(|target| endpoints.contains(target)),
            _ => None,
        };
        if let (Some(endpoint), Some(model)) = (endpoint, model) {
            refused.push(RefusedSignatureRow {
                signature: signature_row(w, targets, types, k, Population::All, true),
                endpoint: endpoint.to_string(),
                provenance: model.provenance.clone(),
            });
        }
        let disposition = if endpoint.is_some() {
            None
        } else {
            model.map(|m| match m.disposition {
                Disposition::Inline => DispatchDisposition::Inline,
                Disposition::Model(_) => DispatchDisposition::Model,
                Disposition::Skip => DispatchDisposition::Skip,
            })
        };

        let mut this_key = SiteBuckets::default();
        for dispatch in JavaDispatch::ALL {
            let mut sites = w.sites_of(k, dispatch);
            if sites == 0 {
                continue;
            }
            let b = &mut buckets[dispatch.index()];
            b.java_sites += sites;
            this_key.java_sites += sites;
            // Rung 0.
            if dispatch == JavaDispatch::Super {
                let exact = super_exact
                    .get(&(k as u32))
                    .copied()
                    .unwrap_or(0)
                    .min(sites);
                b.cha += exact;
                b.cha_super_exact += exact;
                this_key.cha += exact;
                policy_edges += exact;
                sites -= exact;
                if sites == 0 {
                    continue;
                }
            }
            let rung = crate::codegen::classify_rung(&policy, dispatch, count, disposition);
            match rung {
                Rung::Model => {
                    b.modelled += sites;
                    this_key.modelled += sites;
                    // One `call` row to the signature's synthetic function.
                    policy_edges += sites;
                }
                Rung::Skip => {
                    b.skipped += sites;
                    this_key.skipped += sites;
                }
                Rung::Cha => {
                    b.cha += sites;
                    this_key.cha += sites;
                    if count == 0 {
                        b.cha_zero_targets += sites;
                        this_key.cha_zero_targets += sites;
                    }
                    policy_edges += count * sites;
                }
                Rung::Inline { by_model } => {
                    b.inlined += sites;
                    this_key.inlined += sites;
                    if by_model {
                        b.inlined_by_model += sites;
                        this_key.inlined_by_model += sites;
                    }
                }
            }
        }

        let excess = excess_of_key(w, targets, k, Population::All);
        if this_key.inlined > 0 {
            inlined.push((excess, k, this_key.inlined_by_model > 0));
        }
        if this_key.modelled > 0 || this_key.skipped > 0 {
            modelled.push((excess, k, this_key.modelled > 0));
        }
        // Closure-shaped and covered by no model: what a user would write one against.
        let closure_shaped = types.single_abstract_method.contains(cls)
            || matches.closure_shaped.contains(&(
                crate::facts::Str::from(cls.as_ref()),
                crate::facts::Str::from(name.as_ref()),
                crate::facts::Str::from(desc.as_ref()),
            ));
        if closure_shaped && model.is_none() {
            unmodelled.push((excess, k));
        }
    }

    // Rank on the excess alone, cut to `top`, and only then build the rows. Under
    // `--top 1000000` the eager form materialized a `SignatureRow` -- three owned strings --
    // for every key in the program, three times over.
    let rank = |rows: &mut Vec<(usize, usize, bool)>| {
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        rows.truncate(opts.top);
    };
    rank(&mut inlined);
    rank(&mut modelled);
    unmodelled.sort_by(|a, b| b.0.cmp(&a.0));
    unmodelled.truncate(opts.top);
    refused.sort_by(|a, b| b.signature.excess.cmp(&a.signature.excess));
    let provenance_of = |k: usize| -> Vec<String> {
        let (cls, name, desc) = &w.keys[k];
        matches
            .dispatch
            .get(&(
                crate::facts::Str::from(cls.as_ref()),
                crate::facts::Str::from(name.as_ref()),
                crate::facts::Str::from(desc.as_ref()),
            ))
            .map(|m| m.provenance.clone())
            .unwrap_or_default()
    };

    let mut total = SiteBuckets::default();
    for b in &buckets {
        total.add(b);
    }
    PolicySection {
        cha_threshold: policy.cha_threshold,
        cha_threshold_interface: policy.cha_threshold_interface,
        order: match policy.order {
            DispatchOrder::ModelFirst => "model-first".to_string(),
            DispatchOrder::ThresholdFirst => "threshold-first".to_string(),
        },
        buckets: total,
        by_dispatch: JavaDispatch::ALL
            .into_iter()
            .map(|d| DispatchBuckets {
                dispatch: d.as_str(),
                buckets: buckets[d.index()],
            })
            .collect(),
        cha_edges,
        policy_edges,
        top_inlined: inlined
            .into_iter()
            .map(|(_, k, by_model)| InlinedSignatureRow {
                signature: signature_row(w, targets, types, k, Population::All, true),
                reason: if by_model { "model" } else { "threshold" },
                provenance: if by_model {
                    provenance_of(k)
                } else {
                    Vec::new()
                },
            })
            .collect(),
        top_modelled: modelled
            .into_iter()
            .map(|(_, k, is_model)| ModelledSignatureRow {
                signature: signature_row(w, targets, types, k, Population::All, true),
                disposition: if is_model { "model" } else { "skip" },
                provenance: provenance_of(k),
            })
            .collect(),
        refused,
        unmodelled_closures: unmodelled
            .into_iter()
            .map(|(_, k)| signature_row(w, targets, types, k, Population::All, true))
            .collect(),
    }
}

/// Section 9: how many sites call each method.
///
/// Measured over the CHA call graph. A direct call contributes one edge, and a virtual site
/// contributes one edge to every method CHA says it could reach. This is the fan-in an
/// inlining-based approach has to handle, which is why it is measured on the CHA graph
/// rather than on the monomorphic subset an index would record.
#[derive(Debug, Serialize)]
pub struct FanIn {
    /// Methods with at least one incoming edge.
    pub methods: usize,
    pub calls_per_method: Distribution,
    /// Incoming edges by the dispatch kind of the site they come from. Direct calls are not
    /// counted here; [`Self::direct_edges`] is their total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_by_dispatch: Option<DispatchCensus>,
    pub direct_edges: usize,
    pub top: Vec<FanInRow>,
}

#[derive(Debug, Serialize)]
pub struct FanInRow {
    pub method: String,
    pub callers: usize,
    /// Of those callers, the ones reaching this method through an `invoke-interface`. When
    /// a method's fan-in is almost all interface dispatch, an inlining-based approach reaches
    /// it through the loosest resolution available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_interface: Option<usize>,
}

/// Section 10: where inlining cannot terminate on its own.
///
/// Strongly connected components of the CHA call graph, with edges deduplicated per
/// (caller function, target). The graph carries every CHA resolvent, so this is an upper
/// bound on recursion. It is also the bound inlining faces, since inlining has to be sound
/// against every target the resolution admits.
#[derive(Debug, Serialize)]
pub struct Recursion {
    pub nodes: usize,
    /// Deduplicated (caller, target) pairs.
    pub edges: usize,
    /// Functions that call themselves.
    pub self_recursive: usize,
    /// Strongly connected components with more than one member.
    pub nontrivial_sccs: usize,
    /// Functions inside one.
    pub functions_in_nontrivial_sccs: usize,
    pub largest_scc: usize,
    /// The same graph with every edge from an `invoke-interface` site removed, and Tarjan
    /// run over it again.
    ///
    /// Phase 1 found one very large cycle on every large app we measured, in one case
    /// holding 37% of the program's functions. This comparison says how much of that cycle
    /// is interface dispatch. If the large component survives, interfaces are not what makes
    /// inlining fail to terminate. If it collapses, they are. The two cases are worth
    /// telling apart before trying to inline through interface calls.
    ///
    /// The cost is a second Tarjan run over the same successor array. Each caller's
    /// class-virtual targets are stored first, so both views share one allocation. Left out
    /// for a program with no interface dispatch, where it would repeat the numbers above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub without_interface_edges: Option<RecursionCore>,
}

/// [`Recursion`] over a subgraph. `nodes` is not repeated, because removing edges never
/// removes a node and the count is the same.
#[derive(Debug, Serialize)]
pub struct RecursionCore {
    pub edges: usize,
    pub self_recursive: usize,
    pub nontrivial_sccs: usize,
    pub functions_in_nontrivial_sccs: usize,
    pub largest_scc: usize,
}

// --- the walk --------------------------------------------------------------

/// One entry per distinct virtual-call signature. Keys are interned symbols, so this is
/// cheap to hold for a whole app. Names become strings only for the rows that print.
type SignatureKey = (Symbol, Symbol, Symbol);

/// What the single pass over the IR collects. Everything else joins this against the CHA
/// tables.
#[derive(Default)]
struct Walk {
    census: Census,
    /// Call sites by dispatch kind, indexed by [`JavaDispatch::index`].
    sites_by_dispatch: [usize; 4],
    /// Dense id per distinct signature, and the reverse table.
    key_ids: HashMap<SignatureKey, u32>,
    keys: Vec<SignatureKey>,
    /// Call sites per signature and dispatch kind, indexed by key id.
    ///
    /// Four counters rather than one, so that any measurement can be recomputed over the
    /// sites of a single kind without walking the IR again and without a second key table.
    /// The signature is the same key either way, since CHA does not know how a site
    /// dispatches, so splitting the key itself would only duplicate target sets.
    sites_per_key: Vec<[usize; 4]>,
    /// Dense id per function name. Covers the program's own functions and every name a call
    /// mentions. A CHA target or a direct-call edge may name a method this import does not
    /// define, such as a library method, and those become nodes with no successors.
    node_ids: HashMap<Symbol, u32>,
    nodes: Vec<Symbol>,
    /// Per caller function, the signatures it dispatches on, how each site dispatched, and
    /// the functions it calls directly. Indexed by the caller's node id.
    ///
    /// The dispatch kind is kept here because [`recursion`] builds the call graph both with
    /// and without the interface edges, and the statements are gone by then.
    caller_keys: Vec<Vec<(u32, JavaDispatch)>>,
    caller_direct: Vec<Vec<u32>>,
    /// Super-dispatched sites by `(key id, the class the runtime begins lookup at)`, counted.
    ///
    /// The start class is a property of the *site* -- it is the enclosing class's superclass,
    /// not anything the signature carries -- so the policy section cannot recover it from the
    /// key table. There are few enough distinct pairs to hold: super calls are about a
    /// hundredth of the virtual ones.
    super_starts: HashMap<(u32, Symbol), usize>,
}

impl Walk {
    fn node(&mut self, name: Symbol) -> u32 {
        if let Some(&id) = self.node_ids.get(&name) {
            return id;
        }
        let id = u32::try_from(self.nodes.len()).expect("more than 4 billion functions");
        self.node_ids.insert(name.clone(), id);
        self.nodes.push(name);
        self.caller_keys.push(Vec::new());
        self.caller_direct.push(Vec::new());
        id
    }

    fn key(&mut self, key: SignatureKey, dispatch: JavaDispatch) -> u32 {
        self.sites_by_dispatch[dispatch.index()] += 1;
        if let Some(&id) = self.key_ids.get(&key) {
            self.sites_per_key[id as usize][dispatch.index()] += 1;
            return id;
        }
        let id = u32::try_from(self.keys.len()).expect("more than 4 billion signatures");
        self.key_ids.insert(key.clone(), id);
        self.keys.push(key);
        let mut counts = [0usize; 4];
        counts[dispatch.index()] = 1;
        self.sites_per_key.push(counts);
        id
    }

    /// Sites dispatching on signature `k`, of every kind.
    fn sites(&self, k: usize) -> usize {
        self.sites_per_key[k].iter().sum()
    }

    /// Sites dispatching on signature `k` through `d` alone.
    fn sites_of(&self, k: usize, d: JavaDispatch) -> usize {
        self.sites_per_key[k][d.index()]
    }
}

/// Walks every statement of every function once, recording the call census, the site count
/// per virtual-call signature, and the caller-to-callee edges that the fan-in and SCC
/// sections need. This is the only pass over the program.
fn walk(program: &ctadl_ir::mir::Program) -> Walk {
    let mut w = Walk::default();
    // Every function gets a node up front. That keeps a function with no calls in the call
    // graph, and keeps the node ids of the program's own functions stable.
    for func in program.functions.iter() {
        w.node(Symbol::from(func.name.as_str()));
    }
    for func in program.functions.iter() {
        let caller = w.node(Symbol::from(func.name.as_str()));
        for block in func.blocks.iter() {
            for stmt in block.statements.iter() {
                let StatementKind::CallAssign { style, .. } = &stmt.kind else {
                    continue;
                };
                w.census.total += 1;
                match style {
                    CallStyle::DirectCall { call_edges } => {
                        w.census.direct += 1;
                        let ctadl_ir::call::CallEdges::Explicit(targets) = call_edges;
                        for target in targets.iter() {
                            let id = w.node(Symbol::from(target.as_str()));
                            w.caller_direct[caller as usize].push(id);
                        }
                    }
                    CallStyle::JavaCall {
                        cls,
                        simple_name,
                        descriptor,
                        dispatch,
                        super_start,
                        receiver: _,
                    } => {
                        w.census.virtual_ += 1;
                        let id = w.key(
                            (cls.clone(), simple_name.clone(), descriptor.clone()),
                            *dispatch,
                        );
                        w.caller_keys[caller as usize].push((id, *dispatch));
                        if *dispatch == JavaDispatch::Super {
                            let start = super_start.clone().unwrap_or_else(|| cls.clone());
                            *w.super_starts.entry((id, start)).or_default() += 1;
                        }
                    }
                    CallStyle::LuaCall { method, .. } => {
                        w.census.lua += 1;
                        // Lua has no declared receiver class and no overloading, so the
                        // method name alone is the key. The empty class and descriptor match
                        // the sentinel the Lua CHA arm uses. Lua has no dispatch instruction
                        // either, so the per-kind sections are left out for a Lua program
                        // rather than reporting every site as `unknown`.
                        let id = w.key(
                            (Symbol::from(""), method.clone(), Symbol::from("")),
                            JavaDispatch::Unknown,
                        );
                        w.caller_keys[caller as usize].push((id, JavaDispatch::Unknown));
                    }
                    CallStyle::FuncPtrCall { .. } => w.census.func_ptr += 1,
                    CallStyle::Unknown => w.census.unknown += 1,
                }
            }
        }
    }
    w
}

// --- assembling the report -------------------------------------------------

/// Measures `program_info`, which must be a loaded import.
pub fn measure(
    import: &str,
    program_info: &ProgramInfo,
    opts: super::ReportOptions,
    matches: &crate::models::ProgramModelMatches,
) -> CallGraphReport {
    let top = opts.top;
    let language = match &program_info.vmt {
        ctadl_ir::call::VirtualMethodTable::Java { .. } => Language::Java,
        ctadl_ir::call::VirtualMethodTable::Lua { .. } => Language::Lua,
        _ => Language::Other,
    };
    let program = &program_info.program;
    log::debug!("report: walking {} functions", program.functions.len());
    let mut w = walk(program);

    // Only a Java program has dispatch kinds to split by. A Lua call site records
    // `Unknown`, which says something about the IR rather than about the program, so a
    // per-kind section there would be a single row saying so. The report leaves out sections
    // that do not apply rather than filling them with zeros.
    let split = language == Language::Java;
    if split {
        w.census.by_dispatch = Some(DispatchCensus::from_counts(&w.sites_by_dispatch));
    }

    let mut report = CallGraphReport {
        tier: "static",
        import: import.to_owned(),
        language,
        functions: program.functions.len(),
        census: std::mem::take(&mut w.census),
        virtual_targets: None,
        worst_signatures: None,
        rta: None,
        hard_cases: None,
        kotlin_lambdas: None,
        functional_interfaces: None,
        policy: None,
        fan_in: None,
        recursion: None,
    };

    if language == Language::Other {
        // No class hierarchy, so the census and the indirect-call count are the whole
        // report. Printing zeros for the type-resolution sections would look like findings.
        return report;
    }

    // Allocated classes feed the RTA arm of the same Datalog run. This covers every
    // function, including skipped ones, because the hierarchy is a property of the whole
    // program.
    let mut instantiated = std::collections::BTreeSet::new();
    let mut finder = InstantiationFinder::new(&mut instantiated);
    for f in program.functions.iter() {
        finder.visit_function_data(FunctionIdx::new(0), f);
    }
    let allocated_classes = instantiated.len();
    log::debug!("report: {allocated_classes} allocated classes; running CHA+RTA");
    let cha = ClassHierarchyAnalysis::with_rta(&program_info.vmt, instantiated);

    // Target sets per key. One lookup per distinct signature, not per site.
    let targets = resolve_keys(&mut w, &cha);
    // What the VMT says about types, as opposed to what the call sites say about dispatch.
    // Empty for a Lua program, which has neither.
    let types = program_info.vmt.type_facts();

    report.virtual_targets = Some(virtual_targets(&w, &targets, split));
    report.worst_signatures = Some(worst_signatures(&w, &targets, &types, top, split));
    report.rta = Some(rta_comparison(&w, &targets, allocated_classes, split));
    if language == Language::Java {
        report.hard_cases = Some(hard_cases(&w, &targets));
        report.kotlin_lambdas = Some(kotlin_lambdas(&w, &targets));
        report.functional_interfaces = Some(functional_interfaces(&w, &targets, &types, top));
        report.policy = Some(policy_section(&w, &targets, &types, &cha, matches, opts));
    }
    report.fan_in = Some(fan_in(&w, &targets, top, split));
    if opts.recursion {
        report.recursion = Some(recursion(w, targets));
    }
    report
}

/// The CHA and RTA target sets of every signature the walk saw, as node ids.
struct Targets {
    /// Per key id, the CHA targets as node ids.
    cha: Vec<Vec<u32>>,
    /// Per key id, how many targets RTA keeps. Only the count is needed, because RTA's
    /// targets are a subset of CHA's and nothing downstream reads the set itself.
    rta_counts: Vec<usize>,
}

/// Looks up every signature the walk saw in both tables, interning the CHA targets as graph
/// nodes.
///
/// Takes the walk mutably rather than copying its node table. A CHA target may name a method
/// this import does not define, such as a library method, and that has to become a node with
/// no successors. On an app with millions of functions the node map is large enough that
/// cloning it to add a few entries is worth avoiding.
fn resolve_keys(w: &mut Walk, cha: &ClassHierarchyAnalysis) -> Targets {
    let mut t = Targets {
        cha: Vec::with_capacity(w.keys.len()),
        rta_counts: Vec::with_capacity(w.keys.len()),
    };
    let lua = cha.language() == ChaLanguage::Lua;
    // The key list does not change here, and interning a node needs the rest of the walk
    // mutably. Hand the list back at the end.
    let keys = std::mem::take(&mut w.keys);
    for key in &keys {
        let (cha_targets, rta_count) = if lua {
            // A Lua call names only a method. Its static resolvent set is every method of
            // that name across the recovered hierarchy, which is what codegen resolves too.
            let set = cha.lua_resolvents_by_method(&key.1);
            let rta = cha.lua_rta_resolvents_by_method(&key.1).len();
            (set.into_iter().collect::<Vec<_>>(), rta)
        } else {
            let set: Vec<Symbol> = cha
                .java_resolvents(key.0.clone(), key.1.clone(), key.2.clone())
                .collect();
            let rta = cha
                .java_rta_resolvents(key.0.clone(), key.1.clone(), key.2.clone())
                .len();
            (set, rta)
        };
        // RTA restricts CHA, so it can never keep more targets. Checked per key rather than
        // on an aggregate, since an aggregate check could not catch a single key where this
        // failed.
        debug_assert!(
            rta_count <= cha_targets.len(),
            "RTA kept {rta_count} targets where CHA found {} for {}.{}{}",
            cha_targets.len(),
            key.0,
            key.1,
            key.2
        );
        t.cha
            .push(cha_targets.into_iter().map(|name| w.node(name)).collect());
        t.rta_counts.push(rta_count);
    }
    w.keys = keys;
    t
}

/// `(targets, sites)` pairs, one per signature. This is the weighted form every
/// distribution here is computed over. `sites` counts the sites that `population` selects,
/// so passing a per-kind selector recomputes any of these numbers over one dispatch kind
/// without walking the IR again.
fn weighted(w: &Walk, targets: &Targets, population: Population) -> Vec<(usize, usize)> {
    (0..w.keys.len())
        .map(|k| (targets.cha[k].len(), population.sites(w, k)))
        .collect()
}

/// Which call sites a measurement covers: all of them, or one dispatch kind's.
#[derive(Clone, Copy)]
enum Population {
    All,
    Dispatch(JavaDispatch),
}

impl Population {
    fn sites(self, w: &Walk, k: usize) -> usize {
        match self {
            Population::All => w.sites(k),
            Population::Dispatch(d) => w.sites_of(k, d),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Population::All => "all",
            Population::Dispatch(d) => d.as_str(),
        }
    }
}

fn virtual_targets(w: &Walk, targets: &Targets, split: bool) -> VirtualTargets {
    let mut v = virtual_targets_of(w, targets, Population::All);
    if split {
        v.by_dispatch = JavaDispatch::ALL
            .into_iter()
            .map(Population::Dispatch)
            // A kind with no sites in this program is left out rather than reported as a
            // row of zeros.
            .filter(|p| (0..w.keys.len()).any(|k| p.sites(w, k) > 0))
            .map(|p| {
                let core = virtual_targets_of(w, targets, p);
                DispatchTargets {
                    dispatch: p.label(),
                    signatures: core.signatures,
                    sites: core.sites,
                    total_edges: core.total_edges,
                    excess_edges: (0..w.keys.len())
                        .map(|k| excess_of_key(w, targets, k, p))
                        .sum(),
                    targets_per_site: core.targets_per_site,
                    sites_with_one_target: core.sites_with_one_target,
                    sites_with_zero_targets: core.sites_with_zero_targets,
                    sites_deferred_to_hybrid_inlining: core.sites_deferred_to_hybrid_inlining,
                }
            })
            .collect();
    }
    v
}

fn virtual_targets_of(w: &Walk, targets: &Targets, population: Population) -> VirtualTargets {
    let mut pairs = weighted(w, targets, population);
    let sites: usize = pairs.iter().map(|(_, n)| n).sum();
    let total_edges: usize = pairs.iter().map(|(v, n)| v * n).sum();
    let count_where = |pred: fn(usize) -> bool| -> usize {
        pairs
            .iter()
            .filter(|(v, _)| pred(*v))
            .map(|(_, n)| *n)
            .sum()
    };
    VirtualTargets {
        // Signatures with at least one site in this population, so a per-kind row does not
        // claim signatures that only the other kinds reach.
        signatures: pairs.iter().filter(|(_, n)| *n > 0).count(),
        sites,
        total_edges,
        sites_with_one_target: count_where(|v| v == 1),
        sites_with_zero_targets: count_where(|v| v == 0),
        sites_deferred_to_hybrid_inlining: count_where(|v| v >= 2),
        targets_per_site: Distribution::from_weighted(&mut pairs),
        by_dispatch: Vec::new(),
    }
}

fn worst_signatures(
    w: &Walk,
    targets: &Targets,
    types: &TypeFacts,
    top: usize,
    split: bool,
) -> WorstSignatures {
    let mut order: Vec<u32> = (0..w.keys.len() as u32).collect();
    // Descending by target count, with ties broken by the key itself so the list is stable
    // across runs. The walk's key ids follow IR order, which is stable, but two keys with
    // equal counts should not swap places on an unrelated edit.
    order.sort_by(|&a, &b| {
        targets.cha[b as usize]
            .len()
            .cmp(&targets.cha[a as usize].len())
            .then_with(|| w.keys[a as usize].cmp(&w.keys[b as usize]))
    });
    let row = |k: u32| signature_row(w, targets, types, k as usize, Population::All, split);
    // By site. The ten worst sites may all share one signature, so the ranked signature
    // list is expanded by its site counts and `top_n_share` may take an entry partially.
    let by_site: Vec<(usize, usize)> = order
        .iter()
        .map(|&k| (targets.cha[k as usize].len(), w.sites(k as usize)))
        .collect();

    let (by_excess, excess) = excess_ranking(w, targets, Population::All);

    WorstSignatures {
        top_by_targets: order.iter().take(top).map(|&k| row(k)).collect(),
        top_by_excess: by_excess.iter().take(top).map(|&k| row(k)).collect(),
        excess_edges: excess.iter().map(|(v, _)| v).sum(),
        top_10_site_share: top_n_share(&by_site, 10),
        top_100_site_share: top_n_share(&by_site, 100),
        top_10_signature_excess_share: top_n_share(&excess, 10),
        top_100_signature_excess_share: top_n_share(&excess, 100),
        by_dispatch: if split {
            JavaDispatch::ALL
                .into_iter()
                .map(Population::Dispatch)
                .filter_map(|p| {
                    let (ranked, excess) = excess_ranking(w, targets, p);
                    let total: usize = excess.iter().map(|(v, _)| v).sum();
                    // A kind that contributes no excess has nothing to rank. Either it has
                    // no sites, or all of them already have one target. The census and the
                    // per-kind target rows report both cases, so an empty list here would
                    // add nothing.
                    (total > 0).then(|| DispatchWorst {
                        dispatch: p.label(),
                        excess_edges: total,
                        top_10_signature_excess_share: top_n_share(&excess, 10),
                        top_100_signature_excess_share: top_n_share(&excess, 100),
                        top_by_excess: ranked
                            .iter()
                            .take(top)
                            .map(|&k| signature_row(w, targets, types, k as usize, p, split))
                            .collect(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// One printable row for signature `k`, with its counts taken over `population`.
fn signature_row(
    w: &Walk,
    targets: &Targets,
    types: &TypeFacts,
    k: usize,
    population: Population,
    split: bool,
) -> SignatureRow {
    let (cls, name, desc) = &w.keys[k];
    let sites = population.sites(w, k);
    SignatureRow {
        class: cls.to_string(),
        name: name.to_string(),
        descriptor: desc.to_string(),
        sites,
        sites_by_dispatch: split.then(|| DispatchCensus::from_counts(&w.sites_per_key[k])),
        receiver_is_known_interface: types.receiver_is_interface(cls),
        cha_targets: targets.cha[k].len(),
        rta_targets: targets.rta_counts[k],
        edges: targets.cha[k].len() * sites,
        excess: excess_of_key(w, targets, k, population),
    }
}

/// Signatures ordered by the excess they contribute over `population`, and that excess as
/// `(value, weight)` pairs for [`top_n_share`].
///
/// Each signature gets weight 1, because a signature is one thing to special-case however
/// many sites dispatch on it. That is why this ranking exists alongside the by-site one.
/// Signatures contributing no excess are dropped, since they cannot be special-cased and
/// would only add a tail of zeros beneath the shares.
fn excess_ranking(
    w: &Walk,
    targets: &Targets,
    population: Population,
) -> (Vec<u32>, Vec<(usize, usize)>) {
    let mut ranked: Vec<u32> = (0..w.keys.len() as u32)
        .filter(|&k| excess_of_key(w, targets, k as usize, population) > 0)
        .collect();
    ranked.sort_by(|&a, &b| {
        excess_of_key(w, targets, b as usize, population)
            .cmp(&excess_of_key(w, targets, a as usize, population))
            .then_with(|| w.keys[a as usize].cmp(&w.keys[b as usize]))
    });
    let excess = ranked
        .iter()
        .map(|&k| (excess_of_key(w, targets, k as usize, population), 1))
        .collect();
    (ranked, excess)
}

/// `sites x (targets - 1)`: the edges this signature contributes beyond the one each of its
/// sites would have if resolution were exact. Zero for a signature with no targets, since
/// there is no call edge there to be excessive about. An unresolved site is reported
/// separately by [`VirtualTargets::sites_with_zero_targets`].
fn excess_of_key(w: &Walk, targets: &Targets, k: usize, population: Population) -> usize {
    targets.cha[k].len().saturating_sub(1) * population.sites(w, k)
}

fn rta_comparison(
    w: &Walk,
    targets: &Targets,
    allocated_classes: usize,
    split: bool,
) -> RtaComparison {
    let edges = |population: Population| -> (usize, usize) {
        (0..w.keys.len()).fold((0, 0), |(cha, rta), k| {
            let sites = population.sites(w, k);
            (
                cha + targets.cha[k].len() * sites,
                rta + targets.rta_counts[k] * sites,
            )
        })
    };
    let mut gaps: Vec<(usize, usize)> = (0..w.keys.len())
        .map(|k| (targets.cha[k].len() - targets.rta_counts[k], w.sites(k)))
        .collect();
    let (cha_edges, rta_edges) = edges(Population::All);
    RtaComparison {
        allocated_classes,
        cha_edges,
        rta_edges,
        edges_dropped: cha_edges - rta_edges,
        gap_per_site: Distribution::from_weighted(&mut gaps),
        by_dispatch: if split {
            JavaDispatch::ALL
                .into_iter()
                .map(Population::Dispatch)
                .filter_map(|p| {
                    let (cha, rta) = edges(p);
                    // Nothing resolved this way at all, so leave the row out rather than
                    // reporting three zeros.
                    (cha > 0).then_some(DispatchRta {
                        dispatch: p.label(),
                        cha_edges: cha,
                        rta_edges: rta,
                        edges_dropped: cha - rta,
                    })
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

fn hard_cases(w: &Walk, targets: &Targets) -> Vec<NamedCase> {
    HARD_CASES
        .iter()
        .map(|(name, descriptor)| {
            let mut case = NamedCase {
                name: (*name).to_owned(),
                descriptor: (*descriptor).to_owned(),
                signatures: 0,
                sites: 0,
                sites_by_dispatch: None,
                total_edges: 0,
                max_targets: 0,
            };
            let mut by_dispatch = [0usize; 4];
            for k in 0..w.keys.len() {
                let (_, n, d) = &w.keys[k];
                if &**n != *name || &**d != *descriptor {
                    continue;
                }
                let count = targets.cha[k].len();
                let sites = w.sites(k);
                case.signatures += 1;
                case.sites += sites;
                case.total_edges += count * sites;
                case.max_targets = case.max_targets.max(count);
                for (slot, n) in by_dispatch.iter_mut().zip(w.sites_per_key[k]) {
                    *slot += n;
                }
            }
            // This section only runs for a Java program, so the split always applies.
            case.sites_by_dispatch = Some(DispatchCensus::from_counts(&by_dispatch));
            case
        })
        .collect()
}

/// `Lkotlin/jvm/functions/Function1;` and `kotlin/Function1` alike, with the arity stripped.
fn is_kotlin_function_type(cls: &str) -> bool {
    let bare = cls.strip_prefix('L').unwrap_or(cls);
    let bare = bare.strip_suffix(';').unwrap_or(bare);
    KOTLIN_FUNCTION_PREFIXES.iter().any(|prefix| {
        bare.strip_prefix(prefix)
            .is_some_and(|arity| !arity.is_empty() && arity.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn kotlin_lambdas(w: &Walk, targets: &Targets) -> KotlinLambdas {
    let mut k = KotlinLambdas {
        sites_by_receiver_type: 0,
        sites_by_method_name: 0,
        sites_by_both: 0,
        receiver_type_only: 0,
        method_name_only: 0,
        receiver_type_sites_with_one_target: 0,
        matched_receiver_types: Vec::new(),
        receiver_type_sites_by_dispatch: None,
    };
    let mut matched = std::collections::BTreeSet::new();
    let mut by_dispatch = [0usize; 4];
    for i in 0..w.keys.len() {
        let (cls, name, _) = &w.keys[i];
        let sites = w.sites(i);
        let by_type = is_kotlin_function_type(cls);
        let by_name = KOTLIN_INVOKE_NAMES.contains(&&**name);
        if by_type {
            k.sites_by_receiver_type += sites;
            matched.insert(cls.to_string());
            for (slot, n) in by_dispatch.iter_mut().zip(w.sites_per_key[i]) {
                *slot += n;
            }
            if targets.cha[i].len() == 1 {
                k.receiver_type_sites_with_one_target += sites;
            }
        }
        if by_name {
            k.sites_by_method_name += sites;
        }
        match (by_type, by_name) {
            (true, true) => k.sites_by_both += sites,
            (true, false) => k.receiver_type_only += sites,
            (false, true) => k.method_name_only += sites,
            (false, false) => {}
        }
    }
    k.matched_receiver_types = matched.into_iter().collect();
    k.receiver_type_sites_by_dispatch = Some(DispatchCensus::from_counts(&by_dispatch));
    k
}

/// Section 12. Functional interfaces in general: one interface, one abstract method.
///
/// Both halves of that test are new data (see [`TypeFacts`]), and neither is a name or a
/// package, so unlike [`kotlin_lambdas`] this survives obfuscation. What it cannot see is an
/// interface the import does not declare. That gap is reported directly rather than left to
/// be inferred from a small number.
fn functional_interfaces(
    w: &Walk,
    targets: &Targets,
    types: &TypeFacts,
    top: usize,
) -> FunctionalInterfaces {
    let mut f = FunctionalInterfaces {
        interfaces_declared: types.interfaces.len(),
        single_abstract_method: types.single_abstract_method.len(),
        sites: 0,
        sites_with_one_target: 0,
        total_edges: 0,
        excess_edges: 0,
        interface_sites_on_unknown_type: 0,
        top: Vec::new(),
    };
    let mut matched: Vec<u32> = Vec::new();
    for k in 0..w.keys.len() {
        let (cls, _, _) = &w.keys[k];
        // The coverage gap, counted over interface-dispatched sites only. Those are the
        // sites whose receiver type should be an interface, so a type missing from the table
        // is one this import never saw declared.
        if !types.interfaces.contains(cls) {
            f.interface_sites_on_unknown_type += w.sites_of(k, JavaDispatch::Interface);
            continue;
        }
        if !types.single_abstract_method.contains(cls) {
            continue;
        }
        let sites = w.sites(k);
        if sites == 0 {
            continue;
        }
        let count = targets.cha[k].len();
        f.sites += sites;
        f.total_edges += count * sites;
        f.excess_edges += excess_of_key(w, targets, k, Population::All);
        if count == 1 {
            f.sites_with_one_target += sites;
        }
        matched.push(k as u32);
    }
    matched.sort_by(|&a, &b| {
        excess_of_key(w, targets, b as usize, Population::All)
            .cmp(&excess_of_key(w, targets, a as usize, Population::All))
            .then_with(|| w.keys[a as usize].cmp(&w.keys[b as usize]))
    });
    f.top = matched
        .iter()
        .take(top)
        .map(|&k| signature_row(w, targets, types, k as usize, Population::All, true))
        .collect();
    f
}

/// Section 9. Counts sites, so it reads the weighted table rather than the deduplicated
/// graph: a method called from a thousand sites has fan-in one thousand even if all those
/// sites sit in one function. This costs one pass over the target sets and builds no graph,
/// which is why it is not gated the way [`recursion`] is.
fn fan_in(w: &Walk, targets: &Targets, top: usize, split: bool) -> FanIn {
    let nodes = &w.nodes;
    let mut fanin = vec![0usize; nodes.len()];
    // A second array rather than four. The intent asks only that interface dispatch not be
    // averaged in with the rest, and splitting one method's fan-in four ways would add a
    // column nobody reads. The per-kind totals are counted in scalars beside it.
    let mut via_interface = vec![0usize; if split { nodes.len() } else { 0 }];
    let mut edges_by_dispatch = [0usize; 4];
    let mut direct_edges = 0usize;
    for (k, targets_of_key) in targets.cha.iter().enumerate() {
        let sites = w.sites(k);
        let interface_sites = w.sites_of(k, JavaDispatch::Interface);
        for &t in targets_of_key {
            fanin[t as usize] += sites;
            if split && interface_sites > 0 {
                via_interface[t as usize] += interface_sites;
            }
        }
        for (slot, n) in edges_by_dispatch.iter_mut().zip(w.sites_per_key[k]) {
            *slot += n * targets_of_key.len();
        }
    }
    for direct in &w.caller_direct {
        for &t in direct {
            fanin[t as usize] += 1;
            direct_edges += 1;
        }
    }

    let mut order: Vec<u32> = (0..nodes.len() as u32)
        .filter(|&i| fanin[i as usize] > 0)
        .collect();
    // Ties broken by name, so the printed list does not reshuffle on an unrelated edit.
    order.sort_by(|&a, &b| {
        fanin[b as usize]
            .cmp(&fanin[a as usize])
            .then_with(|| nodes[a as usize].cmp(&nodes[b as usize]))
    });
    let mut per_method: Vec<(usize, usize)> =
        order.iter().map(|&i| (fanin[i as usize], 1)).collect();
    FanIn {
        methods: order.len(),
        calls_per_method: Distribution::from_weighted(&mut per_method),
        edges_by_dispatch: split.then(|| DispatchCensus::from_counts(&edges_by_dispatch)),
        direct_edges,
        top: order
            .iter()
            .take(top)
            .map(|&i| FanInRow {
                method: nodes[i as usize].to_string(),
                callers: fanin[i as usize],
                via_interface: split.then(|| via_interface[i as usize]),
            })
            .collect(),
    }
}

/// Section 10, and the only part of the report that has to materialize the CHA call graph.
///
/// That graph is not the size of the program. We have observed 1.9 million functions and
/// 5.2 million virtual sites expand to 1.21 billion deduplicated edges, because a signature
/// with twenty thousand targets contributes twenty thousand edges from every function that
/// calls it. On that run this section took 34 s out of 89 s. That is why `--no-recursion`
/// exists, and why the edge count is logged before Tarjan starts.
///
/// It does not set the memory peak. The same run peaked at 24.9 GiB with this section and
/// 24.1 GiB without it. The high-water mark is reached earlier, inside [`run_cha`], whose
/// intermediate relations are freed before this runs, so the graph is built underneath a
/// ceiling that already exists.
///
/// [`run_cha`]: crate::codegen::run_cha
///
/// Node and SCC indices are `u32` rather than `usize`. The successor lists and [`Sccs`]'s
/// own concatenated successor array each use one machine word per edge, so this halves the
/// graph's footprint, even though it does not move the measured peak.
///
/// Takes ownership, because the graph is the largest thing the report holds and nothing
/// after it needs the walk.
fn recursion(w: Walk, targets: Targets) -> Recursion {
    let n = w.nodes.len();
    // Every node has a successor list, possibly empty. `Walk::node` extends the node table
    // and the two per-caller tables together, so a library method interned as a CHA target
    // becomes a node with no outgoing edges.
    debug_assert_eq!(w.caller_keys.len(), n);
    // Deduplicated per (caller, target). A caller with many sites on one signature
    // contributes that signature's targets once.
    //
    // Each successor list is ordered so that the targets reachable without interface
    // dispatch come first, and `class_virtual` records where that prefix ends. One
    // allocation then serves both views of the graph: the whole thing, and the same graph
    // with interface edges removed. That is what makes the comparison affordable, since the
    // expensive half of this section is building and deduplicating the lists, and that is
    // done once.
    let mut succ: Vec<Vec<u32>> = Vec::with_capacity(n);
    let mut class_virtual: Vec<u32> = Vec::with_capacity(n);
    let mut edges = 0usize;
    let mut class_virtual_edges = 0usize;
    let mut self_recursive = 0usize;
    let mut class_virtual_self_recursive = 0usize;
    let mut any_interface = false;
    for (caller, keys) in w.caller_keys.iter().enumerate() {
        let mut out = w.caller_direct[caller].clone();
        for &(k, dispatch) in keys {
            if dispatch != JavaDispatch::Interface {
                out.extend_from_slice(&targets.cha[k as usize]);
            }
        }
        out.sort_unstable();
        out.dedup();
        let split = out.len();
        // The interface half, minus anything the class-virtual half already reaches. An
        // edge is one (caller, target) pair however many sites produce it.
        let mut iface: Vec<u32> = Vec::new();
        for &(k, dispatch) in keys {
            if dispatch == JavaDispatch::Interface {
                any_interface = true;
                iface.extend_from_slice(&targets.cha[k as usize]);
            }
        }
        if !iface.is_empty() {
            iface.sort_unstable();
            iface.dedup();
            iface.retain(|t| out.binary_search(t).is_err());
            out.extend_from_slice(&iface);
        }
        out.shrink_to_fit();
        edges += out.len();
        class_virtual_edges += split;
        // Both halves are sorted but their concatenation is not, so look for the self-edge
        // in each half separately.
        if out[..split].binary_search(&(caller as u32)).is_ok() {
            class_virtual_self_recursive += 1;
            self_recursive += 1;
        } else if out[split..].binary_search(&(caller as u32)).is_ok() {
            self_recursive += 1;
        }
        succ.push(out);
        class_virtual.push(split as u32);
    }
    // Freed before Tarjan allocates, so the two peaks do not add up.
    drop(w);
    drop(targets);
    // Logged before Tarjan runs, so a run that is about to be very large says so first.
    log::info!(
        "report: CHA call graph has {n} nodes and {edges} deduplicated edges \
         ({class_virtual_edges} of them without interface dispatch)"
    );

    let graph = ChaCallGraph {
        succ: &succ,
        limit: None,
    };
    let (nontrivial_sccs, functions_in_nontrivial_sccs, largest_scc) = sccs_of(&graph, n);
    // The comparison graph, over the same successor array truncated per caller. Skipped
    // when the program has no interface dispatch, where it would be the same graph twice.
    let without_interface_edges = any_interface.then(|| {
        let graph = ChaCallGraph {
            succ: &succ,
            limit: Some(&class_virtual),
        };
        let (nontrivial_sccs, functions_in_nontrivial_sccs, largest_scc) = sccs_of(&graph, n);
        RecursionCore {
            edges: class_virtual_edges,
            self_recursive: class_virtual_self_recursive,
            nontrivial_sccs,
            functions_in_nontrivial_sccs,
            largest_scc,
        }
    });
    Recursion {
        nodes: n,
        edges,
        self_recursive,
        nontrivial_sccs,
        functions_in_nontrivial_sccs,
        largest_scc,
        without_interface_edges,
    }
}

/// Runs Tarjan over one view of the graph. Returns `(nontrivial components, functions in
/// one, largest)`.
fn sccs_of(graph: &ChaCallGraph<'_>, n: usize) -> (usize, usize, usize) {
    let sccs: Sccs<u32, u32> = Sccs::new(graph);
    let mut members = vec![0u32; sccs.num_sccs()];
    for node in 0..n as u32 {
        members[sccs.scc(node) as usize] += 1;
    }
    let nontrivial: Vec<usize> = members
        .into_iter()
        .filter(|&m| m > 1)
        .map(|m| m as usize)
        .collect();
    (
        nontrivial.len(),
        nontrivial.iter().sum(),
        nontrivial.iter().copied().max().unwrap_or(1),
    )
}

/// Adapter that lets [`Sccs`], which is generic over [`Successors`], run on the call graph.
/// The nodes are the dense remap the walk built.
///
/// `limit` selects the view. `None` is the whole graph. `Some(prefix)` cuts each caller's
/// successors down to the targets it reaches without interface dispatch, which the builder
/// above placed first. Both views borrow one successor array.
struct ChaCallGraph<'a> {
    succ: &'a [Vec<u32>],
    limit: Option<&'a [u32]>,
}

impl DirectedGraph for ChaCallGraph<'_> {
    type Node = u32;
    fn num_nodes(&self) -> usize {
        self.succ.len()
    }
}

impl Successors for ChaCallGraph<'_> {
    fn successors(&self, node: u32) -> impl Iterator<Item = u32> {
        let out = &self.succ[node as usize];
        let end = match self.limit {
            None => out.len(),
            Some(limit) => limit[node as usize] as usize,
        };
        out[..end].iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctadl_ir::mir::builder::FunctionBuilder;
    use ctadl_ir::mir::call::{
        CallObject, JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
    };
    use ctadl_ir::mir::{Exp, FunctionData, Program};

    /// A program with one interface `LI;` (one abstract `m()V`, so it is functional), two
    /// implementers, a class `LC;` with a subclass, and one caller making one call of each
    /// dispatch kind.
    ///
    /// Small enough to check by hand. Every count below is written out in the assertions
    /// rather than computed from the fixture, so the two cannot drift together.
    fn dispatch_program() -> ProgramInfo {
        let method = |cls: &str, name: &str| {
            (
                JavaClass(cls.into()),
                JavaSimpleName(name.into()),
                JavaSignature("()V".into()),
                JavaMethod(format!("{cls}->{name}()V").into()),
            )
        };
        let vmt = VirtualMethodTable::Java {
            // `LI;` itself declares no implementation. An interface method has no body, so
            // it goes in `abstract_methods` below instead.
            methods: vec![
                method("LA;", "m"),
                method("LB;", "m"),
                method("LC;", "n"),
                method("LD;", "n"),
            ],
            hierarchy: [
                (
                    JavaClass("LA;".into()),
                    smallvec::smallvec![JavaClass("LI;".into())],
                ),
                (
                    JavaClass("LB;".into()),
                    smallvec::smallvec![JavaClass("LI;".into())],
                ),
                (
                    JavaClass("LD;".into()),
                    smallvec::smallvec![JavaClass("LC;".into())],
                ),
            ]
            .into_iter()
            .collect(),
            interfaces: vec![JavaClass("LI;".into())],
            abstract_methods: vec![(
                JavaClass("LI;".into()),
                JavaSimpleName("m".into()),
                JavaSignature("()V".into()),
            )],
            natives: Vec::new(),
        };

        let mut f = FunctionData {
            name: "Lmain;->run()V".to_string(),
            ..Default::default()
        };
        let mut fb = FunctionBuilder::new(&mut f);
        let body = fb.add_block();
        let mut b = fb.at_block(body);
        let x = b.new_local_var("x");
        b.create_assign(
            x.clone(),
            vec![Exp::ObjectRef(CallObject::JavaObject(JavaClass(
                "LA;".into(),
            )))],
        );
        for (cls, name, dispatch) in [
            ("LI;", "m", JavaDispatch::Interface),
            ("LC;", "n", JavaDispatch::Virtual),
            ("LC;", "n", JavaDispatch::Super),
        ] {
            b.create_call(
                CallStyle::JavaCall {
                    receiver: x.clone(),
                    cls: cls.into(),
                    simple_name: name.into(),
                    descriptor: "()V".into(),
                    dispatch,
                    super_start: None,
                },
                Vec::new(),
                Vec::new(),
            );
        }
        b.create_ret(Vec::<Exp>::new());
        f.verify().expect("function does not verify");

        let mut program = Program::default();
        let idx = program.new_function();
        program[idx] = f;
        ProgramInfo {
            program,
            vmt,
            ..Default::default()
        }
    }

    /// The whole of phase 2 in one assertion set. Three calls that used to be recorded as
    /// one indistinguishable kind are now counted, distributed, and ranked separately.
    #[test]
    fn dispatch_kinds_are_measured_separately() {
        let info = dispatch_program();
        let report = measure(
            "t",
            &info,
            super::super::ReportOptions::default(),
            &Default::default(),
        );

        let census = report
            .census
            .by_dispatch
            .expect("a Java program splits its census");
        assert_eq!(
            (census.virtual_, census.interface, census.super_),
            (1, 1, 1)
        );
        assert_eq!(census.unknown, 0, "every call here came from a real opcode");
        assert_eq!(census.total(), report.census.virtual_);

        let v = report.virtual_targets.expect("virtual targets");
        // `LI;.m` reaches both implementers, and `LC;.n` reaches `LC;` and `LD;`. What
        // matters here is not the totals but that each kind gets its own row.
        let rows: Vec<_> = v
            .by_dispatch
            .iter()
            .map(|d| (d.dispatch, d.sites, d.total_edges))
            .collect();
        assert_eq!(
            rows,
            vec![("virtual", 1, 2), ("interface", 1, 2), ("super", 1, 2)],
            "one site of each kind, each reaching two methods"
        );
        assert_eq!(v.sites, 3);
        assert_eq!(v.total_edges, 6);

        // The interface call and the two class-virtual ones are ranked within their kinds.
        let worst = report.worst_signatures.expect("worst signatures");
        let by_kind: Vec<_> = worst
            .by_dispatch
            .iter()
            .map(|d| (d.dispatch, d.excess_edges, d.top_by_excess[0].name.as_str()))
            .collect();
        assert_eq!(
            by_kind,
            vec![
                ("virtual", 1, "n"),
                ("interface", 1, "m"),
                ("super", 1, "n")
            ]
        );
        assert_eq!(
            worst.excess_edges, 3,
            "each of the three sites carries one edge it would not need if resolution were exact"
        );

        // `LI;` is the one interface and declares one abstract method, so it is the one
        // functional interface. Found without matching any name.
        let f = report.functional_interfaces.expect("functional interfaces");
        assert_eq!((f.interfaces_declared, f.single_abstract_method), (1, 1));
        assert_eq!(f.sites, 1);
        assert_eq!(f.interface_sites_on_unknown_type, 0);
        assert_eq!(f.top[0].name, "m");
        assert_eq!(
            f.top[0].receiver_is_known_interface,
            Some(true),
            "the receiver type is the interface the import declares"
        );

        // Removing the interface edge leaves the rest of the graph alone. There is no cycle
        // here either way, and the comparison pass has to report that rather than crash.
        let rec = report.recursion.expect("recursion");
        let cv = rec
            .without_interface_edges
            .expect("the program has interface dispatch");
        assert!(cv.edges < rec.edges, "the interface call contributed edges");
        assert_eq!((rec.nontrivial_sccs, cv.nontrivial_sccs), (0, 0));
    }

    /// The policy section is built from the complete key table, so a signature with no excess
    /// -- one target, or none -- is still counted into a bucket. Building it from
    /// `top_by_excess`, which drops those, would make every site percentage wrong.
    #[test]
    fn the_policy_section_counts_every_site() {
        let info = dispatch_program();
        let report = measure(
            "t",
            &info,
            super::super::ReportOptions::default(),
            &Default::default(),
        );
        let policy = report.policy.expect("a Java program simulates the policy");
        assert_eq!(
            policy.buckets.java_sites, report.census.virtual_,
            "every Java call site is in a bucket"
        );
        assert_eq!(
            policy.buckets.modelled
                + policy.buckets.skipped
                + policy.buckets.cha
                + policy.buckets.inlined,
            policy.buckets.java_sites
        );
        // Three sites, each with two targets, all under the default threshold. The super site
        // resolves exactly, so it contributes one edge instead of two.
        assert_eq!(policy.cha_edges, 6);
        assert_eq!(policy.policy_edges, 5);
        assert_eq!(policy.buckets.cha_super_exact, 1);
        assert!(
            policy.top_modelled.is_empty() && policy.refused.is_empty(),
            "no model file was given"
        );
    }

    #[test]
    fn kotlin_function_types_match_both_spellings() {
        assert!(is_kotlin_function_type("Lkotlin/jvm/functions/Function1;"));
        assert!(is_kotlin_function_type("kotlin/jvm/functions/Function0"));
        assert!(is_kotlin_function_type("Lkotlin/Function22;"));
        // The arity digits are required, so the base interface and a similar-looking
        // package do not match.
        assert!(!is_kotlin_function_type("Lkotlin/jvm/functions/Function;"));
        assert!(!is_kotlin_function_type("Lkotlinx/Function1;"));
        assert!(!is_kotlin_function_type("Ljava/lang/Object;"));
    }
}
