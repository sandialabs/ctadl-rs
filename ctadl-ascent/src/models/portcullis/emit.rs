/*! Head instantiation: groundings → [`ProgramModelMatches`].

A rule whose body is one component runs per import, over that import's solutions; a bridging
rule runs once, after the import loop, over the union (see [`super::PortcullisMatcher`]).
Everything it writes is named rather than resolved — a function name, not a `FunctionId` —
because a model names functions that may live in any import, and the id map that would resolve
them does not hold every import's functions until the loop is over.

# Deduplication

Two groundings that differ only in a variable a head does not mention would emit that head's row
twice. Each head is therefore grounded over its own variables only — see
[`RuleSolutions::groundings`] — so it fires once per distinct key. Without it,

```text
source(F::return) :- fun(F, name = "read"), param(F, I);
```

would emit one source per parameter of `read`. A rule emitted per import can meet the same key
in two imports (a library function both declare); [`RuleEmitter`] remembers the keys it emitted
for that case.

The rules of one migrated JSON generator are one generator, so a source, sink or propagation two
of them derive is derived once: [`Emitted`] holds what each has emitted. A generator becomes
one rule for each disjunct of its `where`, and those may overlap. Two generators, like two
files, each contribute their rows, deliberately: see [`ProgramModelMatches`]'s `endpoints`.
*/

use std::collections::BTreeSet;

use ctadl_ir::mir::PathSegment;
use hashbrown::{HashMap, HashSet};

use crate::facts::TaintDirection;
use crate::facts::{self, FormalIndex};
use crate::models::FormalIndexTypeTag;
use crate::models::matches::{
    DispatchKey, DispatchModel, Disposition, EndpointMatch, ModelPort, ProgramModelMatches,
    PropagationMatch,
};
use crate::models::spec::{BridgePort, Direction, PortPair, ResolvedBridge};

use super::ast::*;
use super::check::Plan;
use super::eval::{Binding, RuleSolutions, Value};
use super::{PortcullisError, PortcullisErrors};

/// What one rule contributed, for the per-file report.
///
/// The `*_heads` counters and the row counters are the two ends of one fan-out, and they are
/// what separates "this rule declares no source" from "this rule declares a source and matched
/// nothing" — the second is the condition a model author is hunting for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleStats {
    /// Heads the running phase kept. Zero means the rule belongs entirely to the other phase,
    /// which is reported as *skipped*, never as *matched nothing* — the two are different
    /// problems and only one of them is a problem.
    pub live_heads: usize,
    /// Groundings of the head variables. Summed over imports for a rule emitted per import, so
    /// one two imports share counts twice; a count of work, not of rows.
    pub groundings: usize,
    /// Distinct functions the rule's heads anchored at, exactly: what `functions_matched`
    /// reports. Not `groundings`, which counts `name_of(F, N)` once per name of `F`.
    pub functions: usize,
    /// `source` heads the rule declares, whether or not the body matched.
    pub source_heads: usize,
    /// `sink` heads the rule declares.
    pub sink_heads: usize,
    /// `propagation` heads the rule declares.
    pub propagation_heads: usize,
    pub sources: usize,
    pub sinks: usize,
    pub propagations: usize,
    pub bridges: usize,
    pub access_paths: usize,
    /// `dispatch` heads the rule declares.
    pub dispatch_heads: usize,
    /// Signature keys a `dispatch` head attached a model, skip, inline or marker to.
    pub dispatch: usize,
    /// Functions a mode head (`skip_analysis`, `mem2reg_full`, …) marked.
    pub modes: usize,
    /// The `local` atoms that ran and never matched: the name each asked for (see
    /// [`super::check::Plan::local_probes`]) and how many functions it was asked about.
    pub missed_locals: Vec<(String, usize)>,
    /// Distinct functions the rule's heads anchored at, capped at [`MATCHED_SAMPLE_CAP`].
    /// This is the DSL's answer to "what did this rule select", which `CTADL0011` reports.
    pub matched_functions: BTreeSet<String>,
}

/// How many matched function names a rule retains for reporting.
///
/// A cap and not a flag because the names are the expensive half: a rule with an unconstrained
/// `fun(F)` selects every function in the program, and a real APK has tens of thousands.
pub const MATCHED_SAMPLE_CAP: usize = 32;

impl RuleStats {
    pub fn total_rows(&self) -> usize {
        self.sources
            + self.sinks
            + self.propagations
            + self.bridges
            + self.access_paths
            + self.dispatch
            + self.modes
    }

    pub fn merge(&mut self, other: &Self) {
        self.live_heads = self.live_heads.max(other.live_heads);
        self.groundings = self.groundings.saturating_add(other.groundings);
        self.functions += other.functions;
        self.source_heads = self.source_heads.max(other.source_heads);
        self.sink_heads = self.sink_heads.max(other.sink_heads);
        self.propagation_heads = self.propagation_heads.max(other.propagation_heads);
        self.dispatch_heads = self.dispatch_heads.max(other.dispatch_heads);
        self.sources += other.sources;
        self.sinks += other.sinks;
        self.propagations += other.propagations;
        self.bridges += other.bridges;
        self.access_paths += other.access_paths;
        self.dispatch += other.dispatch;
        self.modes += other.modes;
        for miss in &other.missed_locals {
            if !self.missed_locals.contains(miss) {
                self.missed_locals.push(miss.clone());
            }
        }
        for name in &other.matched_functions {
            if self.matched_functions.len() >= MATCHED_SAMPLE_CAP {
                break;
            }
            self.matched_functions.insert(name.clone());
        }
    }
}

/// Which half of a model file a load consumes. The other half is counted and reported, not
/// silently dropped: `ctadl index` says how many query-time rules it skipped and `ctadl query`
/// says how many index-time ones.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Phase {
    Index,
    Query,
    /// Both halves, for a caller that wants everything (tests, and the model checker).
    All,
}

impl Phase {
    /// Whether this phase keeps a head of this kind.
    pub fn wants(self, head: &HeadKind) -> bool {
        matches!(
            (self, head),
            (Phase::All, _)
                | (
                    Phase::Query,
                    HeadKind::Source { .. } | HeadKind::Sink { .. }
                )
                | (
                    Phase::Index,
                    HeadKind::Propagation { .. }
                        | HeadKind::Bridge { .. }
                        | HeadKind::AccessPath { .. }
                        | HeadKind::Dispatch { .. }
                        | HeadKind::Mode(_)
                )
        )
    }
}

/// One rule's emission state, kept across calls so that a rule emitted once per import emits
/// each row once over the whole run, and its stats describe the run rather than the last import.
pub struct RuleEmitter {
    heads: Vec<LiveHead>,
    /// Whether the rule is emitted once per import rather than once after the loop. Only then
    /// can a head key recur from one call to the next, so only then is [`LiveHead::seen`] kept.
    per_import: bool,
    tally: Tally,
}

/// A head the phase keeps, with the positions of the variables it reads.
struct LiveHead {
    /// Index into `Rule::heads`.
    head: usize,
    /// The variables the head reads that some component binds, sorted: the layout of its keys.
    vars: Vec<String>,
    /// Parallel to `vars`: the `(component, column)` each is read from.
    columns: Vec<(usize, usize)>,
    /// Keys already emitted, for a rule emitted per import. A key holds only the head's own
    /// variables, so this is no larger than what the head emitted.
    seen: HashSet<Vec<Value>>,
}

/// The sources, sinks and propagations one file has emitted, keyed by the label
/// ([`PortcullisFile::label`](super::PortcullisFile::label)) of the rule that emitted them, so each
/// label emits each row once.
#[derive(Default)]
pub struct Emitted {
    endpoints: HashSet<(usize, EndpointMatch)>,
    propagations: HashSet<(usize, PropagationMatch)>,
}

/// What emission accumulates besides the rows themselves.
#[derive(Default)]
struct Tally {
    stats: RuleStats,
    /// Every function a head anchored at, for [`RuleStats::functions`].
    anchored: HashSet<facts::Str>,
    /// `(from, to)` -> the entry of `ProgramModelMatches::resolved_bridges` its ports join.
    bridge_slots: HashMap<(facts::Str, facts::Str), usize>,
}

impl RuleEmitter {
    /// `per_import` says the caller hands over each import's solutions separately, rather than
    /// their union once.
    pub fn new(rule: &Rule, plan: &Plan, phase: Phase, per_import: bool) -> Self {
        let mut stats = RuleStats::default();
        let mut heads = Vec::new();
        for (hi, head) in rule.heads.iter().enumerate() {
            if !phase.wants(&head.kind) {
                continue;
            }
            match head.kind {
                HeadKind::Source { .. } => stats.source_heads += 1,
                HeadKind::Sink { .. } => stats.sink_heads += 1,
                HeadKind::Propagation { .. } => stats.propagation_heads += 1,
                HeadKind::Dispatch { .. } => stats.dispatch_heads += 1,
                _ => {}
            }
            let mut vars = Vec::new();
            let mut columns = Vec::new();
            for var in head_vars(head) {
                // A head variable no component binds is one `check` rejects. Leaving it out
                // reads it as unbound, which emits nothing for the head.
                let position =
                    plan.components.iter().enumerate().find_map(|(ci, c)| {
                        c.vars.iter().position(|v| *v == var).map(|col| (ci, col))
                    });
                if let Some(position) = position {
                    vars.push(var);
                    columns.push(position);
                }
            }
            heads.push(LiveHead {
                head: hi,
                vars,
                columns,
                seen: HashSet::new(),
            });
        }
        stats.live_heads = heads.len();
        Self {
            heads,
            per_import,
            tally: Tally {
                stats,
                ..Default::default()
            },
        }
    }

    /// Whether the phase keeps any of the rule's heads. A rule it keeps none of need not be
    /// evaluated at all.
    pub fn is_live(&self) -> bool {
        !self.heads.is_empty()
    }

    /// Instantiates the live heads over `solutions`.
    pub fn emit(
        &mut self,
        rule: &Rule,
        solutions: &RuleSolutions,
        provenance: &str,
        label: usize,
        out: &mut ProgramModelMatches,
        emitted: &mut Emitted,
        errors: &mut PortcullisErrors,
    ) {
        let Self {
            heads,
            per_import,
            tally,
        } = self;
        if heads.is_empty() {
            return;
        }
        tally.stats.groundings = tally
            .stats
            .groundings
            .saturating_add(solutions.grounding_count());
        for live in heads {
            let head = &rule.heads[live.head];
            for key in solutions.groundings(&live.columns) {
                if *per_import && !live.seen.insert(key.clone()) {
                    continue;
                }
                let binding = Binding {
                    vars: &live.vars,
                    values: &key,
                };
                let rows = Rows {
                    out: &mut *out,
                    emitted: &mut *emitted,
                    label,
                };
                emit_head(rule, head, &binding, provenance, rows, tally, errors);
            }
        }
    }

    /// The rule's stats over every call.
    pub fn finish(self) -> RuleStats {
        let mut stats = self.tally.stats;
        stats.functions = self.tally.anchored.len();
        stats
    }
}

/// Where a head's rows go, and what its label has emitted.
struct Rows<'r> {
    out: &'r mut ProgramModelMatches,
    emitted: &'r mut Emitted,
    label: usize,
}

fn emit_head(
    rule: &Rule,
    head: &Head,
    binding: &Binding<'_>,
    provenance: &str,
    rows: Rows<'_>,
    tally: &mut Tally,
    errors: &mut PortcullisErrors,
) {
    let Rows {
        out,
        emitted,
        label: rule_label,
    } = rows;
    match &head.kind {
        HeadKind::Source {
            port,
            saturating,
            label,
        } => {
            let Some(site) = resolve_anchor(head, port, binding, errors) else {
                return;
            };
            let Some((tag, index, local_index)) = endpoint_base(port, &site, binding, errors)
            else {
                return;
            };
            let endpoint = EndpointMatch {
                function: site.function,
                selector_ty: tag,
                index,
                path: path_of(&port.port.path),
                label: facts::Str::from(label.as_str()),
                direction: TaintDirection::Forward,
                wildcard: false,
                saturating: *saturating,
                in_function: site.in_function,
                callsite_scoped: site.callsite_scoped,
                local_index,
            };
            if !emitted.endpoints.insert((rule_label, endpoint)) {
                return;
            }
            out.endpoints.push(endpoint);
            tally.stats.sources += 1;
            note_match(tally, site.function);
        }
        HeadKind::Sink {
            port,
            wildcard,
            label,
        } => {
            let Some(site) = resolve_anchor(head, port, binding, errors) else {
                return;
            };
            let Some((tag, index, local_index)) = endpoint_base(port, &site, binding, errors)
            else {
                return;
            };
            let endpoint = EndpointMatch {
                function: site.function,
                selector_ty: tag,
                index,
                path: path_of(&port.port.path),
                label: facts::Str::from(label.as_str()),
                direction: TaintDirection::Backward,
                wildcard: *wildcard,
                saturating: false,
                in_function: site.in_function,
                callsite_scoped: site.callsite_scoped,
                local_index,
            };
            if !emitted.endpoints.insert((rule_label, endpoint)) {
                return;
            }
            out.endpoints.push(endpoint);
            tally.stats.sinks += 1;
            note_match(tally, site.function);
        }
        HeadKind::Propagation { flow } => {
            let Some(anchor) = resolve_anchor(head, &flow.left, binding, errors) else {
                return;
            };
            if anchor.callsite_scoped {
                errors.push(PortcullisError::Rule {
                    message: "a propagation is a whole-function summary and cannot be anchored at \
                              a call site"
                        .to_string(),
                    span: flow.span,
                });
                return;
            }
            let (Some(left), Some(right)) = (
                model_port(&flow.left.port, binding, flow.span, errors),
                model_port(&flow.right.port, binding, flow.span, errors),
            ) else {
                return;
            };
            let mut push = |dst: ModelPort, src: ModelPort| {
                let propagation = PropagationMatch {
                    function: anchor.function,
                    dst,
                    src,
                };
                if emitted.propagations.insert((rule_label, propagation)) {
                    out.propagations.push(propagation);
                }
            };
            match flow.op {
                FlowOp::ToRight => push(right, left),
                FlowOp::ToLeft => push(left, right),
                FlowOp::Both => {
                    push(left, right);
                    push(right, left);
                }
            }
            tally.stats.propagations += if flow.op == FlowOp::Both { 2 } else { 1 };
            note_match(tally, anchor.function);
        }
        HeadKind::Bridge { flow } => {
            let (Some(from), Some(to)) = (
                resolve_anchor(head, &flow.left, binding, errors),
                resolve_anchor(head, &flow.right, binding, errors),
            ) else {
                return;
            };
            if from.callsite_scoped || to.callsite_scoped {
                // `check` already rejects a statically-known site anchor; this covers the case
                // where the type could not be inferred. See the checker for why.
                errors.push(PortcullisError::Rule {
                    message: "a bridge attaches inside a function rather than at one call, so \
                              both its ports must be anchored at functions. Anchor at the callee \
                              — 'callsite(_, S, callee_string = F)' then 'F::' — which covers \
                              every call site of it."
                        .to_string(),
                    span: flow.span,
                });
                return;
            }
            let (Some(left), Some(right)) = (
                bridge_port(&flow.left.port, binding, flow.span, errors),
                bridge_port(&flow.right.port, binding, flow.span, errors),
            ) else {
                return;
            };
            let direction = match flow.op {
                FlowOp::ToRight => Direction::In,
                FlowOp::ToLeft => Direction::Out,
                FlowOp::Both => Direction::Both,
            };
            // Ports of one (rule, from, to) triple accumulate into one bridge, so a rule with
            // three `bridge` heads is one bridge with three pairs rather than three bridges.
            // The slot is this rule's own, so two files numbering their rules alike stay apart;
            // it is re-checked because nothing but convention keeps `out` the same accumulator
            // from one call to the next.
            let key = (from.function, to.function);
            let slot = tally.bridge_slots.get(&key).copied().filter(|&i| {
                out.resolved_bridges.get(i).is_some_and(|b| {
                    (b.rule, b.from, b.to) == (rule.index, key.0, key.1)
                        && b.provenance == provenance
                })
            });
            match slot {
                Some(i) => out.resolved_bridges[i].ports.push(PortPair {
                    from: left,
                    to: right,
                    direction,
                }),
                None => {
                    tally.bridge_slots.insert(key, out.resolved_bridges.len());
                    out.resolved_bridges.push(ResolvedBridge {
                        provenance: provenance.to_string(),
                        rule: rule.index,
                        from: from.function,
                        to: to.function,
                        ports: vec![PortPair {
                            from: left,
                            to: right,
                            direction,
                        }],
                    })
                }
            }
            tally.stats.bridges += 1;
            note_match(tally, from.function);
        }
        HeadKind::AccessPath { segments, .. } => {
            out.access_paths
                .insert(facts::Path::from_accesses(segments.iter().cloned()));
            tally.stats.access_paths += 1;
        }
        HeadKind::Dispatch { disposition } => {
            let Some((key, id)) = resolve_callsig(rule, head, disposition, binding, errors) else {
                return;
            };
            match disposition {
                // A marker, not a model: it names what hybrid inlining is *for* so the report
                // can say which closure-shaped signatures no model covers. Nothing resolves
                // differently for being on this list.
                DispatchHead::ClosureShaped => {
                    out.closure_shaped.insert(key);
                }
                _ => {
                    let disposition = match disposition {
                        DispatchHead::Inline => Disposition::Inline(Vec::new()),
                        DispatchHead::Skip => Disposition::Skip,
                        DispatchHead::ClosureShaped => unreachable!("handled above"),
                        DispatchHead::Flow(flow) | DispatchHead::InlineFlow(flow) => {
                            let (Some(left), Some(right)) = (
                                model_port(&flow.left.port, binding, flow.span, errors),
                                model_port(&flow.right.port, binding, flow.span, errors),
                            ) else {
                                return;
                            };
                            // `(destination, source)`, which is what `Disposition::Model`
                            // holds -- the same order, and the same reading of the arrow, as
                            // the `propagation` arm above.
                            let ports = match flow.op {
                                FlowOp::ToRight => vec![(right, left)],
                                FlowOp::ToLeft => vec![(left, right)],
                                FlowOp::Both => vec![(left, right), (right, left)],
                            };
                            if matches!(disposition, DispatchHead::InlineFlow(_)) {
                                Disposition::Inline(ports)
                            } else {
                                Disposition::Model(ports)
                            }
                        }
                    };
                    out.add_dispatch(
                        key,
                        DispatchModel {
                            disposition,
                            // Already `file:rule`; see `PortcullisFile::provenance`.
                            provenance: vec![provenance.to_string()],
                        },
                    );
                }
            }
            tally.stats.dispatch += 1;
            note_match(tally, id);
        }
        HeadKind::Mode(mode_head) => {
            let mode = mode_head.mode;
            let function = match head.anchor.as_ref() {
                Some(Term::Lit(Literal::Str(s))) => facts::Str::from(s.as_str()),
                Some(Term::Var(v)) => match binding.get(v) {
                    Some(Value::Str(f)) => f,
                    Some(other) => {
                        errors.push(PortcullisError::Rule {
                            message: format!(
                                "'{other}' is not a function, so '{}' cannot mark it",
                                mode.relation_name()
                            ),
                            span: head.span,
                        });
                        return;
                    }
                    None => return,
                },
                _ => return,
            };
            out.claim_mode(function, *mode_head, provenance);
            tally.stats.modes += 1;
            note_match(tally, function);
        }
    }
}

/// The signature key a `dispatch` head is anchored at, as the `(cls, name, descriptor)` triple
/// codegen keys on, plus the composed id for reporting.
///
/// The triple rides on the binding rather than being split back out of the id: see
/// [`Value::CallSig`].
fn resolve_callsig(
    rule: &Rule,
    head: &Head,
    disposition: &DispatchHead,
    binding: &Binding<'_>,
    errors: &mut PortcullisErrors,
) -> Option<(DispatchKey, facts::Str)> {
    let span = disposition.flow().map_or(head.span, |f| f.span);
    // A flow's ports may carry the anchor instead of the head atom, exactly as a propagation's
    // may.
    let term = head
        .anchor
        .as_ref()
        .or_else(|| {
            disposition
                .flow()
                .and_then(|flow| flow.left.anchor.as_ref().or(flow.right.anchor.as_ref()))
        })
        .or_else(|| {
            errors.push(PortcullisError::Rule {
                message: format!(
                    "rule {}: a 'dispatch' head must be anchored at a 'callsig' key",
                    rule.index
                ),
                span,
            });
            None
        })?;
    let value = match term {
        Term::Var(v) => binding.get(v)?,
        _ => {
            errors.push(PortcullisError::Rule {
                message: "a 'dispatch' head is anchored at a variable bound by 'callsig'"
                    .to_string(),
                span,
            });
            return None;
        }
    };
    match value {
        Value::CallSig {
            id,
            cls,
            name,
            desc,
        } => Some(((cls, name, desc), id)),
        other => {
            errors.push(PortcullisError::Rule {
                message: format!(
                    "'{other}' is not a call signature, so no dispatch model attaches to it; \
                     bind the anchor with 'callsig(K)'"
                ),
                span,
            });
            None
        }
    }
}

/// What a resolved anchor denotes.
struct Anchor {
    /// The function the port hangs off. For a call-site anchor this is the *callee*, which is
    /// what a callsite-scoped endpoint names.
    function: facts::Str,
    /// The caller a call-site anchor restricts to.
    in_function: Option<facts::Str>,
    callsite_scoped: bool,
}

fn resolve_anchor(
    head: &Head,
    port: &PortExpr,
    binding: &Binding<'_>,
    errors: &mut PortcullisErrors,
) -> Option<Anchor> {
    let term = port.anchor.as_ref().or(head.anchor.as_ref())?;
    let value = match term {
        Term::Lit(Literal::Str(s)) => Value::Str(facts::Str::from(s.as_str())),
        Term::Var(v) => binding.get(v)?,
        _ => {
            errors.push(PortcullisError::Rule {
                message: "a port anchor must be a function name or a variable".to_string(),
                span: port.span,
            });
            return None;
        }
    };
    match value {
        Value::Str(f) => Some(Anchor {
            function: f,
            in_function: None,
            callsite_scoped: false,
        }),
        Value::Site { caller, callee, .. } => Some(Anchor {
            function: callee,
            in_function: Some(caller),
            callsite_scoped: true,
        }),
        other => {
            errors.push(PortcullisError::Rule {
                message: format!(
                    "'{other}' is not a function or call site, so no port hangs off it"
                ),
                span: port.span,
            });
            None
        }
    }
}

/// [`resolve_base`] for a source or sink, which may also name a local: the `(tag, index,
/// local_index)` an [`EndpointMatch`] carries.
///
/// A local is one of a particular function's, so it only means something anchored there.
fn endpoint_base(
    port: &PortExpr,
    site: &Anchor,
    binding: &Binding<'_>,
    errors: &mut PortcullisErrors,
) -> Option<(FormalIndexTypeTag, Option<i16>, Option<u32>)> {
    let PortBase::Local(v) = &port.port.base else {
        let (tag, index) = resolve_base(&port.port, binding, port.span, errors)?;
        return Some((tag, index, None));
    };
    match binding.get(v) {
        Some(Value::Local { function, index }) if function == site.function => {
            Some((FormalIndexTypeTag::Local, None, Some(index)))
        }
        Some(Value::Local { function, .. }) => {
            errors.push(PortcullisError::Rule {
                message: format!(
                    "'{v}' is a local of '{function}', but the port is anchored at '{}'; anchor \
                     it at the function 'local' bound it with",
                    site.function
                ),
                span: port.span,
            });
            None
        }
        _ => {
            errors.push(PortcullisError::Rule {
                message: format!("'{v}' names a local but is not bound by 'local(F, {v})'"),
                span: port.span,
            });
            None
        }
    }
}

/// The `(tag, index)` a port names, grounding an `arg(I)` selector if there is one.
fn resolve_base(
    port: &Port,
    binding: &Binding<'_>,
    span: Span,
    errors: &mut PortcullisErrors,
) -> Option<(FormalIndexTypeTag, Option<i16>)> {
    match &port.base {
        PortBase::Return => Some((FormalIndexTypeTag::Return, None)),
        PortBase::Arg(i) => Some((FormalIndexTypeTag::Index, Some(*i))),
        PortBase::AnyArg => Some((FormalIndexTypeTag::AnyArgument, None)),
        PortBase::ArgVar(v) => match binding.get(v) {
            Some(Value::Int(i)) => match i16::try_from(i) {
                Ok(i) if i >= 0 => Some((FormalIndexTypeTag::Index, Some(i))),
                _ => {
                    errors.push(PortcullisError::Rule {
                        message: format!("'{v}' is bound to {i}, which is not an argument index"),
                        span,
                    });
                    None
                }
            },
            _ => {
                errors.push(PortcullisError::Rule {
                    message: format!("'{v}' indexes an argument but is not bound to an integer"),
                    span,
                });
                None
            }
        },
        // The checker allows a local port only on a source or sink, which `endpoint_base`
        // resolves.
        PortBase::Local(_) => {
            errors.push(PortcullisError::Rule {
                message: "a 'local(L)' port is valid only on a source or sink".to_string(),
                span,
            });
            None
        }
    }
}

fn model_port(
    port: &Port,
    binding: &Binding<'_>,
    span: Span,
    errors: &mut PortcullisErrors,
) -> Option<ModelPort> {
    let (tag, index) = resolve_base(port, binding, span, errors)?;
    Some(ModelPort {
        tag,
        index,
        path: path_of(&port.path),
    })
}

/// A bridge port. Unlike a source or a propagation port, this one has to name a *slot*: a
/// wildcard has no correspondent on the other side of the bridge.
fn bridge_port(
    port: &Port,
    binding: &Binding<'_>,
    span: Span,
    errors: &mut PortcullisErrors,
) -> Option<BridgePort> {
    let (tag, index) = resolve_base(port, binding, span, errors)?;
    let formal = match tag {
        FormalIndexTypeTag::Index => FormalIndex::new(index.expect("Index carries one")),
        FormalIndexTypeTag::Return => crate::codegen::RETURN_INDEX.into(),
        FormalIndexTypeTag::AnyArgument => {
            errors.push(PortcullisError::Rule {
                message: "'arg(_)' is not valid in a bridge: a wildcard has no correspondent on \
                          the other side"
                    .to_string(),
                span,
            });
            return None;
        }
        FormalIndexTypeTag::Global | FormalIndexTypeTag::Local => {
            errors.push(PortcullisError::Rule {
                message: "this port is not valid in a bridge".to_string(),
                span,
            });
            return None;
        }
    };
    Some(BridgePort {
        index: formal,
        path: path_of(&port.path),
    })
}

/// Records a function a head anchored at, up to the cap.
fn note_match(tally: &mut Tally, function: facts::Str) {
    tally.anchored.insert(function);
    let stats = &mut tally.stats;
    if stats.matched_functions.len() < MATCHED_SAMPLE_CAP {
        stats.matched_functions.insert(function.to_string());
    }
}

fn path_of(segments: &[PathSegment]) -> facts::Path {
    facts::Path::from_accesses(segments.iter().cloned())
}

/// The variables a head reads: its anchors and any `arg(I)` / `local(L)` selector.
fn head_vars(head: &Head) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(Term::Var(v)) = &head.anchor {
        out.insert(v.clone());
    }
    let ports: Vec<&PortExpr> = match &head.kind {
        HeadKind::Source { port, .. } | HeadKind::Sink { port, .. } => vec![port],
        kind => kind
            .flow()
            .map_or_else(Vec::new, |f| vec![&f.left, &f.right]),
    };
    for port in ports {
        if let Some(Term::Var(v)) = &port.anchor {
            out.insert(v.clone());
        }
        if let Some(v) = port.port.base.var() {
            out.insert(v.to_string());
        }
    }
    out.into_iter().collect()
}
