/*! The call-graph measurements.

One walk over the imported IR, one CHA run, and everything else is a join. What the walk
keeps is deliberately small: [`ClassHierarchyAnalysis`] is keyed by *signature*
`(class, name, descriptor)`, not by call site, so every `Object.equals` site in the program
shares one target set. Storing per site would be millions of identical rows on a real APK
and would make a per-site top-10 ten copies of one line. So the walk holds one entry per
distinct signature plus the number of sites that use it, and every per-site number below is
the weighted expansion of that table -- see [`crate::stats`], whose helpers all take
`(value, weight)` pairs for this reason.

Everything here is the *static tier*: it needs an import and nothing else. No index is read.
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
    call::{CallStyle, JavaDispatch},
};

/// A method's simple name and descriptor, matched exactly. Obfuscation renames classes and
/// repackages them, but a `equals(Ljava/lang/Object;)Z` override still has to keep the name
/// and descriptor the JVM dispatches on, so this is the one matcher that survives it.
const HARD_CASES: &[(&str, &str)] = &[
    ("equals", "(Ljava/lang/Object;)Z"),
    ("hashCode", "()I"),
    ("toString", "()Ljava/lang/String;"),
];

/// Declared receiver types that mean "this is a Kotlin lambda / functional-interface call",
/// matched with the arity digits stripped so `Function0` through `FunctionN` are one case.
///
/// Both spellings occur in real dex type pools -- `kotlin.jvm.functions.FunctionN` is the
/// interface carrying `invoke`, and `kotlin.FunctionN` is the marker interface above it --
/// so both are matched. Neither is a guarantee: an obfuscated app repackages the interface
/// and its method, and then a lambda call site names something like `Lu7/p;.R(...)` and no
/// type list can find it. That is why [`KotlinLambdas`] reports the method-name test beside
/// this one instead of trusting either alone.
const KOTLIN_FUNCTION_PREFIXES: &[&str] = &["kotlin/jvm/functions/Function", "kotlin/Function"];

/// Method names a Kotlin lambda body is invoked through.
const KOTLIN_INVOKE_NAMES: &[&str] = &["invoke", "invokeSuspend"];

/// Which resolution scheme the program's virtual calls use, and therefore which sections of
/// the report mean anything. A Pcode or C import has no class hierarchy at all: it gets the
/// call census and nothing else, rather than a page of zeros that read as findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Java,
    Lua,
    /// No class hierarchy (pcode, C, flowy).
    Other,
}

// --- the report ------------------------------------------------------------

/// Everything the static tier measures. Sections that do not apply to the program are absent
/// from the JSON rather than zero; see [`Language`].
#[derive(Debug, Serialize)]
pub struct CallGraphReport {
    /// Which tier produced this. Always `"static"` in this version: no index is consulted.
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
    pub fan_in: Option<FanIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursion: Option<Recursion>,
}

/// One number per [`JavaDispatch`] kind.
///
/// Interface calls are kept apart from class-virtual ones everywhere they are counted,
/// because CHA behaves far worse on an interface -- it admits every unrelated class that
/// implements it -- and an average over the two describes neither.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct DispatchCensus {
    /// `invoke-virtual`. `virtual` is a Rust keyword, so the field carries the trailing
    /// underscore and the JSON does not.
    #[serde(rename = "virtual")]
    pub virtual_: usize,
    /// `invoke-interface`.
    pub interface: usize,
    /// `invoke-super`. Its target is fixed at the named class, but CTADL resolves it as an
    /// ordinary virtual call, so it is counted apart to show what that costs.
    #[serde(rename = "super")]
    pub super_: usize,
    /// The frontend recorded no dispatch instruction: a JVM `invokedynamic` with a receiver,
    /// or a synthesised call. Not a kind of dispatch -- a gap, reported as one.
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
/// `virtual` counts every `JavaCall`, and [`Self::by_dispatch`] splits it into the three
/// dispatch instructions the frontends now record. The split is the point: `invoke-interface`
/// and `invoke-virtual` are one `CallStyle` and one resolution path, but they are not one
/// measurement.
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
    /// Sum of target counts over sites: one (site, target) pair per unit, which is what a
    /// client of the call graph actually pays for.
    pub total_edges: usize,
    /// Targets per site, weighted by how many sites share each signature.
    pub targets_per_site: Distribution,
    /// Sites CHA resolves to exactly one method. The cheap ones; the percentage is how much
    /// work is already done.
    pub sites_with_one_target: usize,
    /// Sites CHA resolves to nothing. Codegen drops these silently under the default
    /// `mixed` strategy -- no edge, no `callee_info` -- so this count is where the call
    /// graph is unsound: missing library code, native methods, or reflection.
    pub sites_with_zero_targets: usize,
    /// Sites with two or more targets: what `mixed` hands to hybrid inlining.
    pub sites_deferred_to_hybrid_inlining: usize,
    /// The same numbers again, computed separately over the sites of each dispatch kind.
    ///
    /// This is the section the interface/class-virtual split exists for. Pooled, a program's
    /// `targets_per_site` is an average over two populations that behave nothing alike, and
    /// the pooled percentiles belong to neither. A kind with no sites in this program is
    /// absent rather than a row of zeros. Empty for a non-Java program.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_dispatch: Vec<DispatchTargets>,
}

/// [`VirtualTargets`] restricted to the call sites of one dispatch kind.
///
/// Every count is over those sites alone. The target *sets* do not change -- CHA is keyed by
/// signature and knows nothing about how a site dispatches -- so what differs between the
/// kinds is which signatures they reach and how often, which is exactly the thing worth
/// seeing.
#[derive(Debug, Serialize)]
pub struct DispatchTargets {
    pub dispatch: &'static str,
    /// Signatures with at least one site of this kind. A signature dispatched both ways --
    /// an `invoke-super` and an `invoke-virtual` naming one method -- is counted in both.
    pub signatures: usize,
    pub sites: usize,
    pub total_edges: usize,
    /// `sites x (targets - 1)`, summed: the edges owed to imprecision rather than to the
    /// call itself. See [`WorstSignatures`].
    pub excess_edges: usize,
    pub targets_per_site: Distribution,
    pub sites_with_one_target: usize,
    pub sites_with_zero_targets: usize,
    pub sites_deferred_to_hybrid_inlining: usize,
}

/// Sections 3 and 5: the worst signatures, and how much of the imprecision they own.
///
/// "Worst" splits three ways here, and the three answer different questions.
///
/// **By site** is what `intent.md` literally asks: the fraction of all call edges owned by
/// the ten individual call instructions with the most targets. On a real app it is
/// necessarily tiny -- ten instructions out of millions -- and that smallness is the finding,
/// not a measurement failure.
///
/// **By target count** is the least *precise* signature: `Object.toString` with 804 possible
/// targets. It is what the intent means by "the call sites with the most resolvents", but on
/// its own it over-weights a terrible signature that is called from one place.
///
/// **By excess** is the actionable one, and the denominator matters. Every resolved call site
/// must have at least one target; that edge is not imprecision, it is the call. What costs
/// is the rest: `sites x (targets - 1)`, summed. A monomorphic signature contributes zero
/// however often it is called -- which is why this ranking exists. Ranking by raw edge count
/// instead puts `StringBuilder.append` at the top of every small app, a signature with
/// exactly one target that no special case could improve.
#[derive(Debug, Serialize)]
pub struct WorstSignatures {
    /// Least precise: ranked by target count.
    pub top_by_targets: Vec<SignatureRow>,
    /// Most imprecision contributed: ranked by [`SignatureRow::excess`]. What the excess
    /// shares below are made of, and what there would be to special-case.
    pub top_by_excess: Vec<SignatureRow>,
    /// Call edges beyond the one each resolved site must have: `total_edges` minus the sites
    /// that resolved to anything. The size of the problem.
    pub excess_edges: usize,
    /// Share of all call edges owned by the 10 worst *sites*: the signature list ranked by
    /// target count, expanded by its site counts, and cut at 10.
    pub top_10_site_share: f64,
    pub top_100_site_share: f64,
    /// Share of [`Self::excess_edges`] owned by the 10 and 100 signatures contributing the
    /// most of it. The number that decides whether special-casing beats making the whole
    /// analysis more precise.
    pub top_10_signature_excess_share: f64,
    pub top_100_signature_excess_share: f64,
    /// The same ranking done again within each dispatch kind. Empty for a non-Java program.
    ///
    /// Worth having separately because the answer to "what would you special-case" differs
    /// by kind, and pooling hides it: the interface list is `Iterator.next` and
    /// `Runnable.run`, the class-virtual list is `Object.toString` and `hashCode`, and a
    /// single pooled top ten is neither list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_dispatch: Vec<DispatchWorst>,
}

/// [`WorstSignatures`] restricted to one dispatch kind.
#[derive(Debug, Serialize)]
pub struct DispatchWorst {
    pub dispatch: &'static str,
    /// Excess owed to sites of this kind. The kinds partition the sites, so these sum to
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
/// count that kind's sites only. [`Self::sites_by_dispatch`] is the exception and always
/// describes the whole program, so a row read out of the interface list still says how the
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
    /// `null` where the import declares no interfaces at all (a Lua program, or a dex that
    /// ships none), so "not an interface" is never confused with "nothing to compare against".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_is_known_interface: Option<bool>,
    /// Methods CHA says one such site could reach.
    pub cha_targets: usize,
    /// The same under RTA. A lower bound; see [`RtaComparison`].
    pub rta_targets: usize,
    /// `sites * cha_targets`: this signature's share of the total edge count.
    pub edges: usize,
    /// `sites * (cha_targets - 1)`, or zero when there are no targets: the edges that exist
    /// only because resolution is imprecise here. Zero for a monomorphic signature.
    pub excess: usize,
}

/// Section 7: what restricting to allocated types buys.
///
/// **RTA here is a lower bound, not a more precise answer.** The set of allocated classes
/// comes from `new`-like expressions in *imported* code only: an object created by a library
/// we did not import, by reflection, or by deserialization is invisible, and RTA drops
/// targets that are genuinely reachable. The number says how much CHA over-approximates
/// relative to what this import can see, which is a measurement, not a resolution strategy.
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
    /// Those sites by dispatch kind. `equals` and `hashCode` are declared on `Object` and so
    /// are class-virtual almost everywhere; a large interface count here means the app calls
    /// them through an interface type, which CHA resolves far more loosely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sites_by_dispatch: Option<DispatchCensus>,
    pub total_edges: usize,
    pub max_targets: usize,
}

/// Section 11: Kotlin lambda call sites, by both discriminators.
///
/// Neither is reliable on its own, and the corpus shows both failing. In TikTok the receiver
/// type finds 31,622 sites and the method name 57,383, so the type test misses 25,761 real
/// ones. In `com.noto_54.apk` the type test finds **none** -- its type pool does carry
/// `Lkotlin/Function0;`, but no call site dispatches on it, because the obfuscator
/// repackaged the functional interfaces (its worst signatures are `Lu7/p;.R(...)` and
/// `Lu7/l;.U(...)`, which are exactly those) along with their methods, so the 141 sites the
/// name test finds are a floor rather than a count.
///
/// Reporting both and their disagreement is what makes that visible as data instead of as a
/// silent zero.
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
    /// The receiver types actually matched, so a stale prefix list is visible rather than
    /// silent.
    pub matched_receiver_types: Vec<String>,
    /// The receiver-type-matched sites by dispatch kind. A Kotlin lambda call is an
    /// `invoke-interface` on `FunctionN`, so anything else here is worth seeing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_type_sites_by_dispatch: Option<DispatchCensus>,
}

/// Section 12: functional interfaces in general, not only Kotlin's.
///
/// A functional interface is an interface declaring exactly one abstract method -- the shape
/// a lambda, a method reference or a `Runnable` compiles against. Recognising them takes two
/// things the IR did not carry before this phase: which types are interfaces, and which of
/// their methods are abstract. Neither is derivable from the resolvent map, whose entries for
/// an interface hold every method of every *implementer* rather than the interface's own.
///
/// Unlike [`KotlinLambdas`] this needs no name list and no package prefix, so it survives
/// obfuscation: a repackaged `Lu7/p;` with one abstract method is still one abstract method.
/// What it cannot see is an interface the import does not declare -- `java/util/Iterator` in
/// an app that does not ship the framework -- which is what
/// [`Self::interface_sites_on_unknown_type`] measures, and why it is reported beside the rest
/// rather than left implicit.
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
    /// The coverage gap: on an app that does not ship the framework, most interface calls
    /// name a type nothing here has ever seen, and every measurement above is over the
    /// remainder.
    pub interface_sites_on_unknown_type: usize,
    /// The functional-interface signatures contributing the most excess.
    pub top: Vec<SignatureRow>,
}

/// Section 9: how many sites call each method.
///
/// Over the CHA call graph: a direct call contributes one, and a virtual site contributes
/// one to every method CHA says it could reach. This is the fan-in an inlining-based
/// approach has to contend with, which is why it is measured on the CHA graph rather than
/// on the monomorphic subset an index would record.
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
    /// Of those callers, the ones reaching this method through an `invoke-interface`. A
    /// method whose fan-in is almost all interface dispatch is one an inlining-based approach
    /// meets through the loosest resolution there is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_interface: Option<usize>,
}

/// Section 10: where inlining cannot terminate on its own.
///
/// SCCs of the CHA call graph, with edges deduplicated per (caller function, target). Since
/// the graph carries every CHA resolvent, this is the *upper* bound on recursion -- and it
/// is the bound inlining faces, because inlining has to be sound against every target the
/// resolution admits.
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
    /// The same graph with every edge from an `invoke-interface` site removed, and Tarjan run
    /// over it again.
    ///
    /// Phase 1 found one enormous cycle on every large app -- up to 37% of TikTok's functions
    /// in a single component -- and this is the counterfactual that says how much of it is
    /// interface dispatch. If the giant component survives, interfaces are not what makes
    /// inlining non-terminating; if it collapses, they are, and the two are worth telling
    /// apart before anyone tries to inline through them.
    ///
    /// The cost is a second Tarjan over the same successor array, which is stored with each
    /// caller's class-virtual targets first so that both views share one allocation. Absent
    /// for a program with no interface dispatch at all, where it would be the same numbers
    /// twice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub without_interface_edges: Option<RecursionCore>,
}

/// [`Recursion`] over a subgraph. `nodes` is not repeated: removing edges never removes a
/// node, so it is the same count.
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
/// cheap to hold for a whole APK; names are turned into strings only for the rows that print.
type SignatureKey = (Symbol, Symbol, Symbol);

/// What the single pass over the IR collects. Everything else is a join of this against the
/// CHA tables.
#[derive(Default)]
struct Walk {
    census: Census,
    /// Call sites by dispatch kind, indexed by [`JavaDispatch::index`].
    sites_by_dispatch: [usize; 4],
    /// Dense id per distinct signature, and the reverse table.
    key_ids: HashMap<SignatureKey, u32>,
    keys: Vec<SignatureKey>,
    /// Call sites per signature *and* dispatch kind, indexed by key id.
    ///
    /// Four counters rather than one, so that every measurement can be recomputed over the
    /// sites of one kind without a second walk and without a second key table. The signature
    /// is the same key either way -- CHA does not know how a site dispatches -- so splitting
    /// the *key* would only duplicate target sets.
    sites_per_key: Vec<[usize; 4]>,
    /// Dense id per function *name*. Covers the program's own functions and every name a
    /// call names, since a CHA target or a direct-call edge may name a method this import
    /// does not define (a library method); those become nodes with no successors.
    node_ids: HashMap<Symbol, u32>,
    nodes: Vec<Symbol>,
    /// Per caller function, the signatures it dispatches on -- with how each site dispatched
    /// -- and the functions it calls directly. Indexed by the caller's node id.
    ///
    /// The dispatch kind rides along because [`recursion`] needs to build the call graph both
    /// with and without the interface edges, and by then the statements are gone.
    caller_keys: Vec<Vec<(u32, JavaDispatch)>>,
    caller_direct: Vec<Vec<u32>>,
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
/// per virtual-call signature, and the caller-to-callee edges the fan-in and SCC sections
/// need. This is the only pass over the program.
fn walk(program: &ctadl_ir::mir::Program) -> Walk {
    let mut w = Walk::default();
    // Every function gets a node up front, so a program function with no calls is still a
    // node of the call graph and the node ids of the program's own functions are stable.
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
                        ..
                    } => {
                        w.census.virtual_ += 1;
                        let id = w.key(
                            (cls.clone(), simple_name.clone(), descriptor.clone()),
                            *dispatch,
                        );
                        w.caller_keys[caller as usize].push((id, *dispatch));
                    }
                    CallStyle::LuaCall { method, .. } => {
                        w.census.lua += 1;
                        // Lua has no declared receiver class and no overloading, so the
                        // method name alone is the key. The empty class and descriptor
                        // mirror the sentinel the Lua CHA arm itself uses. It has no dispatch
                        // instruction either, and the per-kind sections are suppressed for a
                        // Lua program rather than reporting every site as `unknown`.
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
    // `Unknown`, which is a fact about the IR rather than about the program, and a per-kind
    // section there would be one row saying so -- the report's rule is that a section which
    // does not apply is absent, not zero.
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
        fan_in: None,
        recursion: None,
    };

    if language == Language::Other {
        // No class hierarchy: the census and the indirect-call count are the whole report.
        // Printing zeros for the type-resolution sections would read as findings.
        return report;
    }

    // Allocated classes feed the RTA arm of the same Datalog run. Every function, skipped
    // ones included: the hierarchy is a property of the whole program.
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
    // What the VMT now says about *types*, as opposed to what the call sites say about
    // dispatch. Empty for a Lua program, which has neither.
    let types = TypeFacts::from_vmt(&program_info.vmt);

    report.virtual_targets = Some(virtual_targets(&w, &targets, split));
    report.worst_signatures = Some(worst_signatures(&w, &targets, &types, top, split));
    report.rta = Some(rta_comparison(&w, &targets, allocated_classes, split));
    if language == Language::Java {
        report.hard_cases = Some(hard_cases(&w, &targets));
        report.kotlin_lambdas = Some(kotlin_lambdas(&w, &targets));
        report.functional_interfaces = Some(functional_interfaces(&w, &targets, &types, top));
    }
    report.fan_in = Some(fan_in(&w, &targets, top, split));
    if opts.recursion {
        report.recursion = Some(recursion(w, targets));
    }
    report
}

/// What the virtual method table says about types, as opposed to what a call site says about
/// dispatch. Both are new in this phase and they answer different questions: a call records
/// the instruction it came from, whatever its receiver's type turns out to be, while this
/// records what the import declares a type to *be*.
///
/// Only the types this import declares are in here. An interface from code that was not
/// imported is simply absent, so every lookup means "declared an interface here", never
/// "known not to be one" -- which is why [`FunctionalInterfaces::interface_sites_on_unknown_type`]
/// is reported beside anything derived from it.
#[derive(Default)]
struct TypeFacts {
    interfaces: std::collections::HashSet<Symbol>,
    /// Interfaces declaring exactly one abstract method: the functional ones.
    single_abstract_method: std::collections::HashSet<Symbol>,
}

impl TypeFacts {
    fn from_vmt(vmt: &ctadl_ir::call::VirtualMethodTable) -> Self {
        let ctadl_ir::call::VirtualMethodTable::Java {
            interfaces,
            abstract_methods,
            ..
        } = vmt
        else {
            return Self::default();
        };
        let interfaces: std::collections::HashSet<Symbol> =
            interfaces.iter().map(|c| c.0.clone()).collect();
        // Distinct (name, descriptor) pairs per declaring type. Distinct rather than a
        // count: one class can be declared in two dex files of the same app, and a method
        // listed twice is still one method.
        let mut declared: HashMap<Symbol, std::collections::BTreeSet<(Symbol, Symbol)>> =
            HashMap::new();
        for (cls, name, desc) in abstract_methods {
            declared
                .entry(cls.0.clone())
                .or_default()
                .insert((name.0.clone(), desc.0.clone()));
        }
        let single_abstract_method = declared
            .into_iter()
            .filter(|(cls, methods)| methods.len() == 1 && interfaces.contains(cls))
            .map(|(cls, _)| cls)
            .collect();
        Self {
            interfaces,
            single_abstract_method,
        }
    }

    fn is_empty(&self) -> bool {
        self.interfaces.is_empty()
    }

    /// `None` when the import declares no interfaces at all, so that "this receiver is not an
    /// interface" cannot be read off a program that could not have said otherwise.
    fn receiver_is_interface(&self, cls: &Symbol) -> Option<bool> {
        (!self.is_empty()).then(|| self.interfaces.contains(cls))
    }
}

/// The CHA and RTA target sets of every signature the walk saw, as node ids.
struct Targets {
    /// Per key id, the CHA targets as node ids.
    cha: Vec<Vec<u32>>,
    /// Per key id, how many targets RTA keeps. Only the count is needed: RTA's targets are
    /// a subset of CHA's, so nothing downstream wants the set itself.
    rta_counts: Vec<usize>,
}

/// Looks up every signature the walk saw in both tables, interning the CHA targets as graph
/// nodes.
///
/// Takes the walk mutably rather than copying its node table. A CHA target may name a method
/// this import does not define -- a library method -- which has to become a node with no
/// successors, and on a two-million-function app the node map is large enough that cloning it
/// to add a few entries is worth not doing.
fn resolve_keys(w: &mut Walk, cha: &ClassHierarchyAnalysis) -> Targets {
    let mut t = Targets {
        cha: Vec::with_capacity(w.keys.len()),
        rta_counts: Vec::with_capacity(w.keys.len()),
    };
    let lua = cha.language() == ChaLanguage::Lua;
    // The key list does not change here, and interning a node needs the rest of the walk
    // mutably; hand it back at the end.
    let keys = std::mem::take(&mut w.keys);
    for key in &keys {
        let (cha_targets, rta_count) = if lua {
            // A Lua call names only a method; its static resolvent set is every method of
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
        // The per-key invariant, checked on every key rather than on an aggregate: RTA
        // restricts CHA, so it can only ever keep fewer targets. No output-level assertion
        // could catch a key where it did not.
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

/// `(targets, sites)` pairs, one per signature: the weighted form every distribution here
/// is computed over. `sites` counts the sites `population` selects, so passing a per-kind
/// selector recomputes any of these numbers over one dispatch kind without a second walk.
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
            // A kind with no sites in this program is absent rather than a row of zeros.
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
        // Signatures with at least one site in this population, so that a per-kind row does
        // not claim signatures only the other kinds reach.
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
    // Descending by target count, ties broken by the key itself so the list is stable
    // across runs (the walk's key ids follow IR order, which is stable, but two keys with
    // equal counts should not swap on an unrelated edit).
    order.sort_by(|&a, &b| {
        targets.cha[b as usize]
            .len()
            .cmp(&targets.cha[a as usize].len())
            .then_with(|| w.keys[a as usize].cmp(&w.keys[b as usize]))
    });
    let row = |k: u32| signature_row(w, targets, types, k as usize, Population::All, split);
    // By site: the ten worst *sites* may all share one signature, so the ranked signature
    // list is expanded by its site counts and `top_n_share` takes an entry partially.
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
                    // A kind that contributes no excess has nothing to rank: either it has no
                    // sites, or every one of them is already monomorphic, and both are said
                    // by the census and the per-kind target rows rather than by an empty list.
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
/// Weight 1 apiece, because a signature is one thing to special-case however many sites
/// dispatch on it -- which is the whole reason this ranking exists beside the by-site one.
/// Signatures contributing nothing are dropped: they cannot be special-cased, and leaving
/// them in would put a tail of zeros under the shares.
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

/// `sites x (targets - 1)`: the edges this signature contributes over and above the one each
/// of its sites would have if resolution were exact. Zero for a signature with no targets,
/// since there is no call edge there to be excessive about -- an unresolved site is the
/// separate finding that [`VirtualTargets::sites_with_zero_targets`] reports.
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
                    // Nothing resolved this way at all: no row rather than three zeros.
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

/// Section 12. Functional interfaces the general way: one interface, one abstract method.
///
/// Both halves of that test are new data (see [`TypeFacts`]), and neither is a name or a
/// package, so unlike [`kotlin_lambdas`] this survives an obfuscator. What it cannot see is an
/// interface the import does not declare, which is reported rather than left to be inferred
/// from a small number.
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
        // The coverage gap, counted over interface-dispatched sites only: those are the ones
        // whose receiver type *should* be an interface, so a type missing from the table is
        // one this import never saw declared.
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

/// Section 9. Counts *sites*, so it comes from the weighted table rather than from the
/// deduplicated graph: a method called from a thousand sites has fan-in a thousand even if
/// those sites all sit in one function. That also means it costs one pass over the target
/// sets and no graph at all, which is why it is not gated the way [`recursion`] is.
fn fan_in(w: &Walk, targets: &Targets, top: usize, split: bool) -> FanIn {
    let nodes = &w.nodes;
    let mut fanin = vec![0usize; nodes.len()];
    // A second array rather than four: what the intent asks is that interface dispatch not be
    // averaged in with the rest, and one method's fan-in split four ways is a column nobody
    // reads. The totals per kind are counted in scalars beside it.
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
    // Ties broken by name so the printed list does not reshuffle on an unrelated edit.
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
/// That graph is not the size of the program. TikTok's 1.9 million functions and 5.2 million
/// virtual sites expand to **1.21 billion** deduplicated edges, because a signature with
/// twenty thousand targets contributes twenty thousand of them from every function that
/// calls it. Measured on that app, this section is 34 s of an 89 s run -- which is why
/// `--no-recursion` exists, and why the edge count is logged *before* Tarjan starts.
///
/// It does not, however, set the peak: the run peaked at 24.9 GiB with this section and
/// 24.1 GiB without it. The high-water mark is reached earlier, inside [`run_cha`], whose
/// intermediate relations are freed before this runs -- so the graph is built underneath a
/// ceiling that already exists.
///
/// [`run_cha`]: crate::codegen::run_cha
///
/// Node and SCC indices are `u32` rather than `usize`, since the successor lists and
/// [`Sccs`]'s own concatenated successor array are each one machine word per edge. That
/// halves the graph's own footprint even though, for the reason above, it does not move the
/// measured peak.
///
/// Takes ownership, because the graph is the largest thing the report holds and nothing
/// after it needs the walk.
fn recursion(w: Walk, targets: Targets) -> Recursion {
    let n = w.nodes.len();
    // Every node has a (possibly empty) successor list: `Walk::node` extends the node table
    // and the two per-caller tables together, so a library method interned as a CHA target
    // is a node of the graph with nothing going out of it.
    debug_assert_eq!(w.caller_keys.len(), n);
    // Deduplicated per (caller, target): a caller with many sites on one signature
    // contributes that signature's targets once.
    //
    // Each successor list is ordered so that the targets reachable *without* interface
    // dispatch come first, with `class_virtual` recording where that prefix ends. One
    // allocation then serves both views of the graph -- the whole thing, and the same graph
    // with interface edges removed -- which is what makes the counterfactual affordable:
    // the expensive half of this section is building and deduplicating the lists, and it is
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
        // The interface half, minus anything the class-virtual half already reaches: an edge
        // is one (caller, target) pair however many sites produce it.
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
        // Both halves are sorted, so the self-edge is looked for in each rather than in the
        // concatenation, which is not.
        if out[..split].binary_search(&(caller as u32)).is_ok() {
            class_virtual_self_recursive += 1;
            self_recursive += 1;
        } else if out[split..].binary_search(&(caller as u32)).is_ok() {
            self_recursive += 1;
        }
        succ.push(out);
        class_virtual.push(split as u32);
    }
    // Freed before Tarjan allocates, so the two peaks do not add.
    drop(w);
    drop(targets);
    // Logged before Tarjan runs, so a run that is about to be enormous says so first.
    log::info!(
        "report: CHA call graph has {n} nodes and {edges} deduplicated edges \
         ({class_virtual_edges} of them without interface dispatch)"
    );

    let graph = ChaCallGraph {
        succ: &succ,
        limit: None,
    };
    let (nontrivial_sccs, functions_in_nontrivial_sccs, largest_scc) = sccs_of(&graph, n);
    // The counterfactual, over the same successor array truncated per caller. Skipped when
    // the program has no interface dispatch at all, where it is the same graph twice.
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

/// Tarjan over one view of the graph: `(nontrivial components, functions in one, largest)`.
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

/// Adapter letting [`Sccs`] -- which is generic over [`Successors`] -- run on the call
/// graph. The nodes are the dense remap the walk built; there is no second Tarjan.
///
/// `limit` selects the view: `None` is the whole graph, and `Some(prefix)` cuts each caller's
/// successors to the targets it reaches without interface dispatch, which the builder above
/// placed first. Both views borrow one successor array.
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
    /// Small enough to check by hand, which is the point: every count below is written out in
    /// the assertions rather than derived from a fixture that could drift with it.
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
            // `LI;` itself declares no implementation: an interface method has no body, and
            // it is in `abstract_methods` below instead.
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

    /// The whole of phase 2 in one assertion set: three calls that used to be one
    /// indistinguishable kind are counted, distributed and ranked apart.
    #[test]
    fn dispatch_kinds_are_measured_separately() {
        let info = dispatch_program();
        let report = measure("t", &info, super::super::ReportOptions::default());

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
        // `LI;.m` reaches both implementers; `LC;.n` reaches `LC;` and `LD;`. Pooled that is
        // an average over two sites of two targets and one of two -- the point is not the
        // number but that each kind is reported on its own row.
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

        // `LI;` is the one interface, and it declares one abstract method, so it is the one
        // functional interface -- found without matching a single name.
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

        // Removing the interface edge leaves the rest of the graph alone; there is no cycle
        // here either way, and the counterfactual has to say so rather than crash.
        let rec = report.recursion.expect("recursion");
        let cv = rec
            .without_interface_edges
            .expect("the program has interface dispatch");
        assert!(cv.edges < rec.edges, "the interface call contributed edges");
        assert_eq!((rec.nontrivial_sccs, cv.nontrivial_sccs), (0, 0));
    }

    #[test]
    fn kotlin_function_types_match_both_spellings() {
        assert!(is_kotlin_function_type("Lkotlin/jvm/functions/Function1;"));
        assert!(is_kotlin_function_type("kotlin/jvm/functions/Function0"));
        assert!(is_kotlin_function_type("Lkotlin/Function22;"));
        // The arity is required, so the base interface and a lookalike package do not match.
        assert!(!is_kotlin_function_type("Lkotlin/jvm/functions/Function;"));
        assert!(!is_kotlin_function_type("Lkotlinx/Function1;"));
        assert!(!is_kotlin_function_type("Ljava/lang/Object;"));
    }
}
