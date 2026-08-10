/*! Head instantiation: groundings → [`ProgramModelMatches`].

Runs once, after the import loop, over the bindings the engine accumulated. Everything it writes
is named rather than resolved — a function name, not a `FunctionId` — for the same reason the
JSON path does it: a model names functions that may live in any import, and the id map that
would resolve them does not hold every import's functions until the loop is over.

# Deduplication

A grounding binds *every* variable in the rule, so two groundings that differ only in a variable
no head mentions would emit the same row twice. Each head is therefore keyed on its own
variables and emitted once per distinct key. Without it,

```text
source(F::return) :- fun(F, name = "read"), param(F, I);
```

would emit one source per parameter of `read`.
*/

use std::collections::BTreeSet;

use ctadl_ir::mir::PathSegment;

use crate::facts::TaintDirection;
use crate::facts::{self, FormalIndex};
use crate::models::FormalIndexTypeTag;
use crate::models::matches::{EndpointMatch, ModelPort, ProgramModelMatches, PropagationMatch};
use crate::models::spec::{BridgePort, Direction, PortPair, ResolvedBridge};

use super::ast::*;
use super::check::Plan;
use super::eval::{Binding, RuleSolutions, Value};
use super::{DslError, DslErrors};

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
    /// Distinct groundings of the whole body.
    pub groundings: usize,
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
        self.sources + self.sinks + self.propagations + self.bridges + self.access_paths
    }

    pub fn merge(&mut self, other: &Self) {
        self.live_heads = self.live_heads.max(other.live_heads);
        self.groundings += other.groundings;
        self.source_heads = self.source_heads.max(other.source_heads);
        self.sink_heads = self.sink_heads.max(other.sink_heads);
        self.propagation_heads = self.propagation_heads.max(other.propagation_heads);
        self.sources += other.sources;
        self.sinks += other.sinks;
        self.propagations += other.propagations;
        self.bridges += other.bridges;
        self.access_paths += other.access_paths;
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
    fn wants(self, head: &HeadKind) -> bool {
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
                )
        )
    }
}

/// Instantiates one rule's heads for `phase`.
pub fn emit_rule(
    rule: &Rule,
    plan: &Plan,
    solutions: &RuleSolutions,
    provenance: &str,
    phase: Phase,
    out: &mut ProgramModelMatches,
    errors: &mut DslErrors,
) -> RuleStats {
    let mut stats = RuleStats::default();
    let live: Vec<&Head> = rule.heads.iter().filter(|h| phase.wants(&h.kind)).collect();
    stats.live_heads = live.len();
    for head in &live {
        match head.kind {
            HeadKind::Source { .. } => stats.source_heads += 1,
            HeadKind::Sink { .. } => stats.sink_heads += 1,
            HeadKind::Propagation { .. } => stats.propagation_heads += 1,
            _ => {}
        }
    }
    if live.is_empty() {
        return stats;
    }
    let groundings: Vec<Binding> = solutions.groundings(plan).collect();
    stats.groundings = groundings.len();
    for head in live {
        let vars = head_vars(head);
        let mut seen: BTreeSet<Vec<Value>> = BTreeSet::new();
        for binding in &groundings {
            let key: Vec<Value> = vars
                .iter()
                .map(|v| binding.get(v).unwrap_or(Value::Int(0)))
                .collect();
            if !seen.insert(key) {
                continue;
            }
            emit_head(rule, head, binding, provenance, out, &mut stats, errors);
        }
    }
    stats
}

fn emit_head(
    rule: &Rule,
    head: &Head,
    binding: &Binding,
    provenance: &str,
    out: &mut ProgramModelMatches,
    stats: &mut RuleStats,
    errors: &mut DslErrors,
) {
    match &head.kind {
        HeadKind::Source {
            port,
            saturating,
            label,
        } => {
            let Some(site) = resolve_anchor(head, port, binding, errors) else {
                return;
            };
            let Some((tag, index)) = resolve_base(&port.port, binding, port.span, errors) else {
                return;
            };
            out.endpoints.push(EndpointMatch {
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
                local_index: None,
            });
            stats.sources += 1;
            note_match(stats, site.function);
        }
        HeadKind::Sink {
            port,
            wildcard,
            label,
        } => {
            let Some(site) = resolve_anchor(head, port, binding, errors) else {
                return;
            };
            let Some((tag, index)) = resolve_base(&port.port, binding, port.span, errors) else {
                return;
            };
            out.endpoints.push(EndpointMatch {
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
                local_index: None,
            });
            stats.sinks += 1;
            note_match(stats, site.function);
        }
        HeadKind::Propagation { flow } => {
            let Some(anchor) = resolve_anchor(head, &flow.left, binding, errors) else {
                return;
            };
            if anchor.callsite_scoped {
                errors.push(DslError::Rule {
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
                out.propagations.push(PropagationMatch {
                    function: anchor.function,
                    dst,
                    src,
                });
            };
            match flow.op {
                FlowOp::ToRight => push(right, left),
                FlowOp::ToLeft => push(left, right),
                FlowOp::Both => {
                    push(left, right);
                    push(right, left);
                }
            }
            stats.propagations += if flow.op == FlowOp::Both { 2 } else { 1 };
            note_match(stats, anchor.function);
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
                errors.push(DslError::Rule {
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
            // Ports of one (rule, from, to) triple accumulate into one bridge, so the design's
            // three-line stack map is one bridge with three pairs rather than three bridges.
            let key = (rule.index, from.function, to.function);
            match out
                .resolved_bridges
                .iter_mut()
                .find(|b| (b.rule, b.from, b.to) == key)
            {
                Some(existing) => existing.ports.push(PortPair {
                    from: left,
                    to: right,
                    direction,
                }),
                None => out.resolved_bridges.push(ResolvedBridge {
                    provenance: provenance.to_string(),
                    rule: rule.index,
                    from: from.function,
                    to: to.function,
                    ports: vec![PortPair {
                        from: left,
                        to: right,
                        direction,
                    }],
                }),
            }
            stats.bridges += 1;
            note_match(stats, from.function);
        }
        HeadKind::AccessPath { segments, .. } => {
            out.access_paths
                .insert(facts::Path::from_accesses(segments.iter().cloned()));
            stats.access_paths += 1;
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
    binding: &Binding,
    errors: &mut DslErrors,
) -> Option<Anchor> {
    let term = port.anchor.as_ref().or(head.anchor.as_ref())?;
    let value = match term {
        Term::Lit(Literal::Str(s)) => Value::Str(facts::Str::from(s.as_str())),
        Term::Var(v) => binding.get(v)?,
        _ => {
            errors.push(DslError::Rule {
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
            errors.push(DslError::Rule {
                message: format!(
                    "'{other}' is not a function or call site, so no port hangs off it"
                ),
                span: port.span,
            });
            None
        }
    }
}

/// The `(tag, index)` a port names, grounding an `arg(I)` selector if there is one.
fn resolve_base(
    port: &Port,
    binding: &Binding,
    span: Span,
    errors: &mut DslErrors,
) -> Option<(FormalIndexTypeTag, Option<i16>)> {
    match &port.base {
        PortBase::Return => Some((FormalIndexTypeTag::Return, None)),
        PortBase::Arg(i) => Some((FormalIndexTypeTag::Index, Some(*i))),
        PortBase::AnyArg => Some((FormalIndexTypeTag::AnyArgument, None)),
        PortBase::ArgVar(v) => match binding.get(v) {
            Some(Value::Int(i)) => match i16::try_from(i) {
                Ok(i) if i >= 0 => Some((FormalIndexTypeTag::Index, Some(i))),
                _ => {
                    errors.push(DslError::Rule {
                        message: format!("'{v}' is bound to {i}, which is not an argument index"),
                        span,
                    });
                    None
                }
            },
            _ => {
                errors.push(DslError::Rule {
                    message: format!("'{v}' indexes an argument but is not bound to an integer"),
                    span,
                });
                None
            }
        },
    }
}

fn model_port(
    port: &Port,
    binding: &Binding,
    span: Span,
    errors: &mut DslErrors,
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
    binding: &Binding,
    span: Span,
    errors: &mut DslErrors,
) -> Option<BridgePort> {
    let (tag, index) = resolve_base(port, binding, span, errors)?;
    let formal = match tag {
        FormalIndexTypeTag::Index => FormalIndex::new(index.expect("Index carries one")),
        FormalIndexTypeTag::Return => crate::codegen::RETURN_INDEX.into(),
        FormalIndexTypeTag::AnyArgument => {
            errors.push(DslError::Rule {
                message: "'arg(_)' is not valid in a bridge: a wildcard has no correspondent on \
                          the other side"
                    .to_string(),
                span,
            });
            return None;
        }
        FormalIndexTypeTag::Global | FormalIndexTypeTag::Local => {
            errors.push(DslError::Rule {
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
fn note_match(stats: &mut RuleStats, function: facts::Str) {
    if stats.matched_functions.len() < MATCHED_SAMPLE_CAP {
        stats.matched_functions.insert(function.to_string());
    }
}

fn path_of(segments: &[PathSegment]) -> facts::Path {
    facts::Path::from_accesses(segments.iter().cloned())
}

/// The variables a head reads: its anchors and any `arg(I)` selector.
fn head_vars(head: &Head) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    if let Some(Term::Var(v)) = &head.anchor {
        out.insert(v.clone());
    }
    let ports: Vec<&PortExpr> = match &head.kind {
        HeadKind::Source { port, .. } | HeadKind::Sink { port, .. } => vec![port],
        HeadKind::Propagation { flow } | HeadKind::Bridge { flow } => vec![&flow.left, &flow.right],
        HeadKind::AccessPath { .. } => vec![],
    };
    for port in ports {
        if let Some(Term::Var(v)) = &port.anchor {
            out.insert(v.clone());
        }
        if let PortBase::ArgVar(v) = &port.port.base {
            out.insert(v.clone());
        }
    }
    out.into_iter().collect()
}
