/*! Phase 2 of codegen: turning [`ProgramModelMatches`] into facts.

Codegen runs in two phases. Phase 1 is what codegen has always done -- lower one import's IR into
facts, once per import. Phase 2 runs **after every import has been codegen'd** and emits the
facts for all the models at once.

Two things depend on that ordering and are improved by it:

- **`Argument(*)` expansion** consults `compute_arg_arity`, which takes the maximum arity over
  actual parameters *and call sites*. Running per import, as summary codegen used to,
  under-expands a function whose call sites span imports; running here sees all of them.
- **A bridge** pins matches in two different programs, so both sides' functions have to be in
  one [`crate::facts::IdMap`] before either can be resolved to an id.

Nothing here modifies the IR, and nothing synthesizes a function. A bridge is one `call` row,
one temporary per mapped callee index, and one `assign` row per port direction; everything
downstream is ordinary summary instantiation, and no inference rule knows a bridge was involved.
*/

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;

use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::error::Error;
use crate::facts::{
    self, FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, PackedInsnSiteId,
};
use crate::index_engine::IndexFacts;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::models::matches::{BridgeSideMatches, ModelPort, PropagationMatch, diagnose};
use crate::models::spec::{BridgePort, BridgeSpec, PortPair, Severity};
use crate::models::{FormalIndexTypeTag, ProgramModelMatches};

/// What phase 2 did with one bridge generator, for the unconditional `info` line.
///
/// A bridge that fired on nothing produces an analysis with fewer flows rather than an error,
/// and a bridge-only generator appears on no other diagnostic surface: it declares no endpoint,
/// so it has no `endpoint_stats` entry and can never raise `CTADL0004`. This line is the only
/// place it shows up, which makes it load-bearing rather than cosmetic. It also covers the
/// *mis-paired* case -- wrong slot, wrong path, wrong function matched -- which warn-on-empty
/// cannot see.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BridgeStats {
    /// `file:generator-index`.
    pub provenance: String,
    pub from_matched: usize,
    pub to_matched: usize,
    /// Pairs emitted: the full cross product, minus any pair whose two halves were not both in
    /// the fact base.
    pub pairs: usize,
}

impl std::fmt::Display for BridgeStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} from, {} to, {} pair(s) bridged",
            self.provenance, self.from_matched, self.to_matched, self.pairs
        )
    }
}

/// What phase 2 emitted, in total.
#[derive(Debug, Clone, Default)]
pub struct ModelCodegenReport {
    pub summaries: usize,
    pub declared_paths: usize,
    /// Functions `modes: ["skip-analysis"]` matched *and* resolved to an id in this project.
    pub skipped: usize,
    pub bridges: Vec<BridgeStats>,
}

/// Emits every matched model into the fact base.
///
/// Call once, after the import loop and before the facts are saved.
pub fn codegen_model_matches(
    matches: &ProgramModelMatches,
    specs: &[BridgeSpec],
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> Result<ModelCodegenReport, Error> {
    // Both arity snapshots are taken up front, before this phase pushes a single row.
    //
    // `arg_arity` is what `Argument(*)` expands over, and taking it here is what makes the
    // expansion see call sites from every import. `num_params` is what the bridge arity warning
    // compares against, so it must report what the *frontend* recovered rather than what the
    // bridge is about to synthesize. Neither may observe this phase's own output: a bridge's
    // actual parameters would otherwise widen a wildcard on the function it targets.
    let arg_arity = facts.compute_arg_arity();
    let num_params = facts.compute_num_params();

    Ok(ModelCodegenReport {
        summaries: codegen_propagations(&matches.propagations, &arg_arity, facts, source_info),
        declared_paths: codegen_declared_paths(matches, facts),
        skipped: codegen_skip_analysis(matches, facts, source_info),
        bridges: codegen_bridges(specs, matches, &num_params, facts, source_info)?,
    })
}

// ---------------------------------------------------------------------------
// Propagations
// ---------------------------------------------------------------------------

/// Turns matched propagation models into `facts.summary` rows.
///
/// This is where a propagation's access paths enter `model_paths` -- the bucket that
/// concatenates with every program path -- so the port semantics documented for `propagation`
/// depend on the rows landing here rather than in `facts.paths`.
fn codegen_propagations(
    propagations: &[PropagationMatch],
    arg_arity: &HashMap<FunctionId, i16>,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> usize {
    let mut emitted = 0usize;
    for prop in propagations {
        log::trace!("{:?}", prop);
        let Some(func_id) = source_info
            .sites
            .get_function_id(facts::Function(prop.function))
        else {
            // Skip functions that don't occur in the facts. A model file names functions that
            // may or may not be in this project; that is not an error.
            continue;
        };
        let dst_index = expand_port(&prop.dst, func_id, arg_arity);
        let src_index = expand_port(&prop.src, func_id, arg_arity);
        for dst_index in &dst_index {
            // Ensure formal_param exists for the indices used in summaries: `locals` is seeded
            // from formals and the summary rule joins on them, so without these rows a
            // modelled function is silently lost.
            facts.formal_param.push((
                func_id,
                FlowVariable::formal_index(*dst_index),
                FormalType::ByRef,
            ));
            for src_index in &src_index {
                facts.formal_param.push((
                    func_id,
                    FlowVariable::formal_index(*src_index),
                    FormalType::ByRef,
                ));
                if dst_index == src_index {
                    continue;
                }
                facts.summary.push((
                    func_id,
                    *dst_index,
                    prop.dst.path,
                    *src_index,
                    prop.src.path,
                ));
                emitted += 1;
            }
        }
    }
    emitted
}

/// The formal indices one model port denotes. `Argument(*)` is the only one that fans out, and
/// it is why this cannot run before every import is codegen'd.
fn expand_port(
    port: &ModelPort,
    func_id: FunctionId,
    arg_arity: &HashMap<FunctionId, i16>,
) -> Vec<FormalIndex> {
    use FormalIndexTypeTag::*;
    match port.tag {
        Index => vec![port.index.expect("an Index port carries one").into()],
        Return => vec![RETURN_INDEX.into()],
        Global => vec![GLOBALS_INDEX.into()],
        AnyArgument => arg_arity
            .get(&func_id)
            .map(|n| (0..*n).map(|i| i.into()).collect())
            .unwrap_or_default(),
        // `Variable(name)` ports are rejected on propagations in Stage 1 (see
        // `json::visit_propagation`), so they never reach summary codegen.
        Local => unreachable!("Variable(name) is not valid on a propagation port"),
    }
}

// ---------------------------------------------------------------------------
// Declared access paths
// ---------------------------------------------------------------------------

/// Seeds user-declared access paths into the initial indexer paths.
///
/// This registry exists because path *composition* past a bridge seam is exact-match only: a
/// pathful bridge composes with its callee's derived summary only where the summary's endpoints
/// land exactly on the port's `to_path`. The residues a deeper summary produces are fixpoint
/// output, so nothing can enumerate them -- but a user who knows the callee's summary shape can
/// name them by hand. Flows needing residues nobody declared are dropped, silently.
fn codegen_declared_paths(matches: &ProgramModelMatches, facts: &mut IndexFacts) -> usize {
    for path in &matches.access_paths {
        facts.paths.push((*path,));
    }
    matches.access_paths.len()
}

// ---------------------------------------------------------------------------
// Analysis modes
// ---------------------------------------------------------------------------

/// Turns matched `modes: ["skip-analysis"]` generators into `facts.skip_analysis` rows.
///
/// The engine reads that relation in one place -- it refuses to seed `locals` for a skipped
/// function -- and everything else follows: with no `locals` there, no body-derived `summary`,
/// no `critical_summary`, and no `context_locals` can be derived inside it either. The rows this
/// phase's `codegen_propagations` pushed are untouched, so a skipped function's behaviour is
/// exactly what its model says and nothing more.
///
/// Names that resolve to no function are skipped in silence, exactly as a propagation's are: a
/// model file names functions that may or may not be in this project.
fn codegen_skip_analysis(
    matches: &ProgramModelMatches,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> usize {
    let mut emitted = 0usize;
    for name in &matches.skip_analysis {
        let Some(func_id) = source_info.sites.get_function_id(facts::Function(*name)) else {
            continue;
        };
        facts.skip_analysis.push((func_id,));
        emitted += 1;
    }
    emitted
}

// ---------------------------------------------------------------------------
// Bridges
// ---------------------------------------------------------------------------

/// Pairs each bridge spec's two match sets, reports what it found, and emits the facts.
fn codegen_bridges(
    specs: &[BridgeSpec],
    matches: &ProgramModelMatches,
    num_params: &HashMap<FunctionId, i16>,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> Result<Vec<BridgeStats>, Error> {
    let mut stats = Vec::with_capacity(specs.len());
    for (i, spec) in specs.iter().enumerate() {
        let side = matches.bridges.get(i);
        classify(spec, side)?;

        let mut pairs = 0usize;
        for (a_name, b_name) in side.pairs() {
            // Both sides must be interned by now; they are, unless the matched function was
            // named in a VMT but never reached codegen (a method table entry with no lowered
            // function and no call site).
            let (Some(a), Some(b)) = (
                source_info.sites.get_function_id(facts::Function(a_name)),
                source_info.sites.get_function_id(facts::Function(b_name)),
            ) else {
                log::warn!(
                    "bridge {}: '{}' or '{}' is not in the fact base, so this pair is not \
                     bridged",
                    spec.provenance(),
                    a_name,
                    b_name
                );
                continue;
            };
            let ports = resolve_ports(spec, a, b, num_params);
            emit_bridge(spec, a, b, &ports, num_params, facts, source_info);
            pairs += 1;
        }
        stats.push(BridgeStats {
            provenance: spec.provenance(),
            from_matched: side.from.len(),
            to_matched: side.to.len(),
            pairs,
        });
    }
    Ok(stats)
}

/// Acts on the verdict [`diagnose`] returns: the reporting semantics deferred from streaming.
///
/// The verdict itself needs no fact base -- see [`diagnose`] -- so it lives next to the match
/// sets it reads. All that is left here is turning a severity into a warning, an error, or
/// silence.
fn classify(spec: &BridgeSpec, side: &BridgeSideMatches) -> Result<(), Error> {
    diagnose(spec, side).map_or(Ok(()), |(severity, message)| report(severity, message))
}

fn report(severity: Severity, message: String) -> Result<(), Error> {
    match severity {
        Severity::Ignore => Ok(()),
        Severity::Warn => {
            log::warn!("{message}");
            Ok(())
        }
        Severity::Error => Err(Error::Model { message }),
    }
}

/// The port map to emit for one pair, including the ports the user never writes.
///
/// The globals pseudo-parameter is mapped **unconditionally** and is not user-visible: without
/// it heap flows do not cross the boundary at all. When `arguments` was omitted entirely the map
/// is the identity over the arity the two sides share, plus `Return`.
fn resolve_ports(
    spec: &BridgeSpec,
    a: FunctionId,
    b: FunctionId,
    num_params: &HashMap<FunctionId, i16>,
) -> Vec<PortPair> {
    let mut ports = if spec.ports_given {
        spec.ports.clone()
    } else {
        let shared = num_params
            .get(&a)
            .copied()
            .unwrap_or(0)
            .min(num_params.get(&b).copied().unwrap_or(0));
        if shared <= 0 {
            // The two sides share no recovered parameter, so an identity map is empty. That is
            // exactly the case a bodyless stub produces, and it is the silent-failure mode this
            // whole design is about, so say it.
            log::warn!(
                "bridge {}: no 'arguments' port map was given and the two sides share no \
                 recovered parameter, so only the return value and globals cross. Write an \
                 'arguments' map naming the argument correspondence.",
                spec.provenance()
            );
        }
        (0..shared.max(0))
            .map(|i| {
                let port = BridgePort {
                    index: FormalIndex::new(i),
                    path: facts::Path::empty(),
                };
                PortPair {
                    from: port,
                    to: port,
                    direction: crate::models::Direction::Both,
                }
            })
            .chain(std::iter::once(PortPair::ret()))
            .collect()
    };
    ports.push(PortPair::globals());
    ports
}

/// Emits the facts for one bridged pair `(a, b)`.
///
/// Read a port as *"the caller's `from` vertex is the callee's `to` vertex"*, and the emission
/// as the three steps that make that true: name the callee's parameter locally, wire the
/// caller's port to it, pass it.
fn emit_bridge(
    spec: &BridgeSpec,
    a: FunctionId,
    b: FunctionId,
    ports: &[PortPair],
    num_params: &HashMap<FunctionId, i16>,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    // A *fresh* site. Call-argument pseudo-variables are keyed on the instruction id, so
    // reusing an existing site's id would alias its argument n to this bridge's argument n --
    // a spurious bidirectional flow between two unrelated arguments. Under cross-product
    // pairing several bridge sites routinely share one caller, which is also why the
    // temporaries below are keyed on `(site, index)` rather than on the index alone.
    //
    // The site gets no `source_map` entry: a synthetic site has no span, and the SARIF step
    // emitter returns early for a location-less site rather than inventing one. Attributing the
    // crossing to the stub's own declaration span is a scoped follow-up.
    let site = source_info.add_insn_site(a);
    let insn = site.insn_id;
    let site: PackedInsnSiteId = site.try_into().expect("packing a fresh bridge site");
    facts.call.push((site, b));

    // A callee index collapses to a direct `actual_param` only when it carries exactly one
    // port, that port names the callee's parameter as a whole (`to_path` empty), and it flows
    // both ways. That is every JNI port, and the special case is what keeps the shipped JNI
    // fact shape byte-identical. Requiring the index to be *sole* matters: the temporary and a
    // direct row would both bind the same call-argument pseudo-variable whole, re-aliasing the
    // ports the temporary exists to keep apart.
    let mut port_count: HashMap<i16, usize> = HashMap::new();
    for port in ports {
        *port_count.entry(*port.to.index).or_default() += 1;
    }
    let collapses = |port: &PortPair| {
        port.to.path.is_empty()
            && port.direction == crate::models::Direction::Both
            && port_count.get(&*port.to.index).copied() == Some(1)
    };

    let mut temp_made: HashSet<i16> = HashSet::new();
    for port in ports {
        // Both sides get a `formal_param` row for every mapped port, trusting the model over
        // the recovered prototype. A bodyless stub has no formals at all; Ghidra gives a
        // function with no recovered prototype zero parameters. Emitting the row asserts the
        // parameter the model names, so an argument that crosses the bridge lands in the
        // callee's `Argument(n)` whatever the disassembler recovered. Side B's rows are a
        // cross-function emission: this loop walks side A's bridge and writes against `b` too.
        //
        // A synthesized formal nothing in the body reads is inert -- it produces a `locals`
        // seed and no flow beyond it -- which is precisely what withholding the row already
        // produces. The one visible side effect is that rows past `b`'s real arity feed
        // `compute_num_params`/`compute_arg_arity`, so a later `Argument(*)` source/sink model
        // on `b` expands over phantom parameters. Accepted as noise; the arity warning below is
        // what tells the author the prototype needs recovering.
        facts.formal_param.push((
            a,
            FlowVariable::formal_index(port.from.index),
            FormalType::ByRef,
        ));
        facts.formal_param.push((
            b,
            FlowVariable::formal_index(port.to.index),
            FormalType::ByRef,
        ));

        if collapses(port) {
            facts.actual_param.push((
                site,
                port.to.index,
                FlowVertex(FlowVariable::formal_index(port.from.index), port.from.path),
            ));
            continue;
        }

        // One temporary per distinct callee index, standing for that parameter as the callee
        // sees it. `actual_param` has one vertex column and no second path, so it cannot say
        // that the caller's port corresponds to a *sub-path* of the callee's parameter; the
        // temporary is what expresses it. Ports sharing a callee index are then separated by
        // field-sensitivity instead of collapsing onto one pseudo-variable and aliasing.
        let t = temp_for(a, insn, port.to.index);
        if temp_made.insert(*port.to.index) {
            // The temporary IS the callee's parameter, so it is passed whole. It needs no
            // `formal_param` row of its own: `locals` seeds from formals and a temporary is a
            // conduit, not a source.
            facts
                .actual_param
                .push((site, port.to.index, FlowVertex(t, facts::Path::empty())));
        }

        // Every path here is literal: `from_path` on the caller's formal, `to_path` on the
        // temporary. Nothing is concatenated, so `program_paths` -- seeded from both endpoints
        // of every `assign` and every `actual_param` vertex -- registers them for free. There
        // is no `facts.paths` push anywhere in a bridge.
        let caller = FlowVertex(FlowVariable::formal_index(port.from.index), port.from.path);
        let callee = FlowVertex(t, port.to.path);
        // `assign` is keyed on the packed instruction site, from which the function is derived.
        // (The function-keyed shape is the persisted parquet form only.)
        if port.direction.inward() {
            facts.assign.push((site, callee.clone(), caller.clone()));
        }
        if port.direction.outward() {
            facts.assign.push((site, caller, callee));
        }
    }

    // An incomplete prototype is worth reporting -- as a warning, never as the reason a fact is
    // missing. The taint edge is emitted either way; this is what says how far it will actually
    // travel.
    let expected = ports
        .iter()
        .map(|p| *p.to.index)
        .filter(|i| *i >= 0)
        .max()
        .map_or(0, |highest| highest + 1);
    let recovered = num_params.get(&b).copied().unwrap_or(0);
    if recovered < expected {
        log::warn!(
            "bridge {}: the callee has {} recovered parameter(s) but the port map names {}; \
             the prototype is incomplete, so some argument(s) will not flow past it",
            spec.provenance(),
            recovered,
            expected
        );
    }
}

/// The local standing for callee parameter `index` at one bridge site.
///
/// Keyed on `(site, index)`, not on the index alone: cross-product pairing puts several bridge
/// sites in one caller, and a temporary named only for its index would merge their parameters.
fn temp_for(func: FunctionId, insn: crate::facts::InsnId, index: FormalIndex) -> FlowVariable {
    FlowVariable::local(facts::Str::from(
        format!("$bridge{}_{}#{}", func.id, insn.id, *index).as_str(),
    ))
}

/// Unpacks a site the way the fact base does, for tests.
#[cfg(test)]
pub(crate) fn site_function(site: PackedInsnSiteId) -> FunctionId {
    crate::facts::InsnSiteId::try_from(site).unwrap().func_id
}

#[cfg(test)]
mod tests;
