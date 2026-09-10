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
use ctadl_ir::mir::{FunctionIdx, ProgramInfo, StatementKind, Symbol, call::CallStyle};

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
    pub fan_in: Option<FanIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursion: Option<Recursion>,
}

/// Section 1: how many call sites there are, by kind.
///
/// `invoke-super` is not its own row. The Dex frontend lowers `invoke-virtual`,
/// `invoke-super` and `invoke-interface` all to one `JavaCall` and keeps no record of which
/// it was, so `virtual` below folds all three together -- interfaces included. Telling them
/// apart is an IR change (a `dispatch` field on `CallStyle::JavaCall`), not a report change.
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
}

/// One `(class, name, descriptor)` signature, with what it costs.
#[derive(Debug, Serialize)]
pub struct SignatureRow {
    pub class: String,
    pub name: String,
    pub descriptor: String,
    /// Call sites in the program that dispatch on this signature.
    pub sites: usize,
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
}

/// Section 8: one named hard case, aggregated over every class that declares it.
#[derive(Debug, Serialize)]
pub struct NamedCase {
    pub name: String,
    pub descriptor: String,
    /// Distinct declared receiver classes this was called on.
    pub signatures: usize,
    pub sites: usize,
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
    pub top: Vec<FanInRow>,
}

#[derive(Debug, Serialize)]
pub struct FanInRow {
    pub method: String,
    pub callers: usize,
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
    /// Dense id per distinct signature, and the reverse table.
    key_ids: HashMap<SignatureKey, u32>,
    keys: Vec<SignatureKey>,
    /// Call sites per signature, indexed by key id.
    sites_per_key: Vec<usize>,
    /// Dense id per function *name*. Covers the program's own functions and every name a
    /// call names, since a CHA target or a direct-call edge may name a method this import
    /// does not define (a library method); those become nodes with no successors.
    node_ids: HashMap<Symbol, u32>,
    nodes: Vec<Symbol>,
    /// Per caller function, the signatures it dispatches on and the functions it calls
    /// directly. Indexed by the caller's node id.
    caller_keys: Vec<Vec<u32>>,
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

    fn key(&mut self, key: SignatureKey) -> u32 {
        if let Some(&id) = self.key_ids.get(&key) {
            self.sites_per_key[id as usize] += 1;
            return id;
        }
        let id = u32::try_from(self.keys.len()).expect("more than 4 billion signatures");
        self.key_ids.insert(key.clone(), id);
        self.keys.push(key);
        self.sites_per_key.push(1);
        id
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
                        ..
                    } => {
                        w.census.virtual_ += 1;
                        let id = w.key((cls.clone(), simple_name.clone(), descriptor.clone()));
                        w.caller_keys[caller as usize].push(id);
                    }
                    CallStyle::LuaCall { method, .. } => {
                        w.census.lua += 1;
                        // Lua has no declared receiver class and no overloading, so the
                        // method name alone is the key. The empty class and descriptor
                        // mirror the sentinel the Lua CHA arm itself uses.
                        let id = w.key((Symbol::from(""), method.clone(), Symbol::from("")));
                        w.caller_keys[caller as usize].push(id);
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

    report.virtual_targets = Some(virtual_targets(&w, &targets));
    report.worst_signatures = Some(worst_signatures(&w, &targets, top));
    report.rta = Some(rta_comparison(&w, &targets, allocated_classes));
    if language == Language::Java {
        report.hard_cases = Some(hard_cases(&w, &targets));
        report.kotlin_lambdas = Some(kotlin_lambdas(&w, &targets));
    }
    report.fan_in = Some(fan_in(&w, &targets, top));
    if opts.recursion {
        report.recursion = Some(recursion(w, targets));
    }
    report
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
/// is computed over.
fn weighted(w: &Walk, targets: &Targets) -> Vec<(usize, usize)> {
    (0..w.keys.len())
        .map(|k| (targets.cha[k].len(), w.sites_per_key[k]))
        .collect()
}

fn virtual_targets(w: &Walk, targets: &Targets) -> VirtualTargets {
    let mut pairs = weighted(w, targets);
    let sites: usize = w.sites_per_key.iter().sum();
    let total_edges: usize = pairs.iter().map(|(v, n)| v * n).sum();
    let count_where = |pred: fn(usize) -> bool| -> usize {
        pairs
            .iter()
            .filter(|(v, _)| pred(*v))
            .map(|(_, n)| *n)
            .sum()
    };
    VirtualTargets {
        signatures: w.keys.len(),
        sites,
        total_edges,
        sites_with_one_target: count_where(|v| v == 1),
        sites_with_zero_targets: count_where(|v| v == 0),
        sites_deferred_to_hybrid_inlining: count_where(|v| v >= 2),
        targets_per_site: Distribution::from_weighted(&mut pairs),
    }
}

fn worst_signatures(w: &Walk, targets: &Targets, top: usize) -> WorstSignatures {
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
    let row = |k: u32| {
        let k = k as usize;
        let (cls, name, desc) = &w.keys[k];
        SignatureRow {
            class: cls.to_string(),
            name: name.to_string(),
            descriptor: desc.to_string(),
            sites: w.sites_per_key[k],
            cha_targets: targets.cha[k].len(),
            rta_targets: targets.rta_counts[k],
            edges: targets.cha[k].len() * w.sites_per_key[k],
            excess: excess_of_key(w, targets, k),
        }
    };
    // By site: the ten worst *sites* may all share one signature, so the ranked signature
    // list is expanded by its site counts and `top_n_share` takes an entry partially.
    let by_site: Vec<(usize, usize)> = order
        .iter()
        .map(|&k| (targets.cha[k as usize].len(), w.sites_per_key[k as usize]))
        .collect();

    // The same signatures ranked by the excess each contributes. Weight 1 apiece, because a
    // signature is one thing to special-case however many sites dispatch on it -- which is
    // the whole reason this ranking exists beside the one above.
    let mut by_excess: Vec<u32> = (0..w.keys.len() as u32).collect();
    by_excess.sort_by(|&a, &b| {
        excess_of_key(w, targets, b as usize)
            .cmp(&excess_of_key(w, targets, a as usize))
            .then_with(|| w.keys[a as usize].cmp(&w.keys[b as usize]))
    });
    let excess: Vec<(usize, usize)> = by_excess
        .iter()
        .map(|&k| (excess_of_key(w, targets, k as usize), 1))
        .collect();

    WorstSignatures {
        top_by_targets: order.iter().take(top).map(|&k| row(k)).collect(),
        top_by_excess: by_excess.iter().take(top).map(|&k| row(k)).collect(),
        excess_edges: excess.iter().map(|(v, _)| v).sum(),
        top_10_site_share: top_n_share(&by_site, 10),
        top_100_site_share: top_n_share(&by_site, 100),
        top_10_signature_excess_share: top_n_share(&excess, 10),
        top_100_signature_excess_share: top_n_share(&excess, 100),
    }
}

/// `sites x (targets - 1)`: the edges this signature contributes over and above the one each
/// of its sites would have if resolution were exact. Zero for a signature with no targets,
/// since there is no call edge there to be excessive about -- an unresolved site is the
/// separate finding that [`VirtualTargets::sites_with_zero_targets`] reports.
fn excess_of_key(w: &Walk, targets: &Targets, k: usize) -> usize {
    targets.cha[k].len().saturating_sub(1) * w.sites_per_key[k]
}

fn rta_comparison(w: &Walk, targets: &Targets, allocated_classes: usize) -> RtaComparison {
    let mut gaps: Vec<(usize, usize)> = (0..w.keys.len())
        .map(|k| {
            (
                targets.cha[k].len() - targets.rta_counts[k],
                w.sites_per_key[k],
            )
        })
        .collect();
    let cha_edges: usize = (0..w.keys.len())
        .map(|k| targets.cha[k].len() * w.sites_per_key[k])
        .sum();
    let rta_edges: usize = (0..w.keys.len())
        .map(|k| targets.rta_counts[k] * w.sites_per_key[k])
        .sum();
    RtaComparison {
        allocated_classes,
        cha_edges,
        rta_edges,
        edges_dropped: cha_edges - rta_edges,
        gap_per_site: Distribution::from_weighted(&mut gaps),
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
                total_edges: 0,
                max_targets: 0,
            };
            for k in 0..w.keys.len() {
                let (_, n, d) = &w.keys[k];
                if &**n != *name || &**d != *descriptor {
                    continue;
                }
                let count = targets.cha[k].len();
                case.signatures += 1;
                case.sites += w.sites_per_key[k];
                case.total_edges += count * w.sites_per_key[k];
                case.max_targets = case.max_targets.max(count);
            }
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
    };
    let mut matched = std::collections::BTreeSet::new();
    for i in 0..w.keys.len() {
        let (cls, name, _) = &w.keys[i];
        let sites = w.sites_per_key[i];
        let by_type = is_kotlin_function_type(cls);
        let by_name = KOTLIN_INVOKE_NAMES.contains(&&**name);
        if by_type {
            k.sites_by_receiver_type += sites;
            matched.insert(cls.to_string());
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
    k
}

/// Section 9. Counts *sites*, so it comes from the weighted table rather than from the
/// deduplicated graph: a method called from a thousand sites has fan-in a thousand even if
/// those sites all sit in one function. That also means it costs one pass over the target
/// sets and no graph at all, which is why it is not gated the way [`recursion`] is.
fn fan_in(w: &Walk, targets: &Targets, top: usize) -> FanIn {
    let nodes = &w.nodes;
    let mut fanin = vec![0usize; nodes.len()];
    for (k, targets_of_key) in targets.cha.iter().enumerate() {
        let sites = w.sites_per_key[k];
        for &t in targets_of_key {
            fanin[t as usize] += sites;
        }
    }
    for direct in &w.caller_direct {
        for &t in direct {
            fanin[t as usize] += 1;
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
        top: order
            .iter()
            .take(top)
            .map(|&i| FanInRow {
                method: nodes[i as usize].to_string(),
                callers: fanin[i as usize],
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
    let mut succ: Vec<Vec<u32>> = Vec::with_capacity(n);
    let mut edges = 0usize;
    let mut self_recursive = 0usize;
    for (caller, keys) in w.caller_keys.iter().enumerate() {
        let mut out = w.caller_direct[caller].clone();
        for &k in keys {
            out.extend_from_slice(&targets.cha[k as usize]);
        }
        out.sort_unstable();
        out.dedup();
        out.shrink_to_fit();
        edges += out.len();
        if out.binary_search(&(caller as u32)).is_ok() {
            self_recursive += 1;
        }
        succ.push(out);
    }
    // Freed before Tarjan allocates, so the two peaks do not add.
    drop(w);
    drop(targets);
    // Logged before Tarjan runs, so a run that is about to be enormous says so first.
    log::info!("report: CHA call graph has {n} nodes and {edges} deduplicated edges");

    let graph = ChaCallGraph { succ };
    let sccs: Sccs<u32, u32> = Sccs::new(&graph);
    let mut members = vec![0u32; sccs.num_sccs()];
    for node in 0..n as u32 {
        members[sccs.scc(node) as usize] += 1;
    }
    let nontrivial: Vec<usize> = members
        .into_iter()
        .filter(|&m| m > 1)
        .map(|m| m as usize)
        .collect();
    Recursion {
        nodes: n,
        edges,
        self_recursive,
        nontrivial_sccs: nontrivial.len(),
        functions_in_nontrivial_sccs: nontrivial.iter().sum(),
        largest_scc: nontrivial.iter().copied().max().unwrap_or(1),
    }
}

/// Adapter letting [`Sccs`] -- which is generic over [`Successors`] -- run on the call
/// graph. The nodes are the dense remap the walk built; there is no second Tarjan.
struct ChaCallGraph {
    succ: Vec<Vec<u32>>,
}

impl DirectedGraph for ChaCallGraph {
    type Node = u32;
    fn num_nodes(&self) -> usize {
        self.succ.len()
    }
}

impl Successors for ChaCallGraph {
    fn successors(&self, node: u32) -> impl Iterator<Item = u32> {
        self.succ[node as usize].iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
