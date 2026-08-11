/*! Load-time checking, and the plan the engine executes.

Three things happen here, all of them before any program is in hand:

1. **Shape.** Relation names, arities and attribute names are checked against the built-ins.
   Every relation name is reserved, so an unrecognized one is a typo, not a user relation.
2. **Modes.** A rule is well-moded when every *operator* — `regex_match`, a comparison, a set
   test, a negated atom — has all its variables bound by atoms **to its left**. This is checked
   strictly left to right, which is what makes `regex_match(F, ".*"), fun(F)` an error while the
   same two atoms the other way round are fine. The engine is then free to reorder (see
   [`Plan`]); modedness is a property the author can see, join order is not.
3. **Components.** The body is split into connected components by shared variables. A rule whose
   body mentions two programs — the bridging case — has one component per side, and the engine
   accumulates each side across imports and joins them once at the end. A single-component rule,
   which is nearly all of them, pays nothing for this.
*/

use std::collections::{BTreeMap, BTreeSet};

use super::ast::*;
use super::{DslError, DslErrors};

/// What a variable ranges over. Used only for the handful of checks that need it; an
/// [`VarType::Unknown`] never provokes a diagnostic.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VarType {
    Unknown,
    Function,
    /// A call site, bound by `callsite`'s second column.
    Site,
    Class,
    Str,
    Int,
    Bool,
}

impl VarType {
    fn name(self) -> &'static str {
        match self {
            VarType::Unknown => "value",
            VarType::Function => "function",
            VarType::Site => "call site",
            VarType::Class => "class",
            VarType::Str => "string",
            VarType::Int => "integer",
            VarType::Bool => "boolean",
        }
    }

    /// Two bindings of one variable agree when either is [`Self::Unknown`] or they are equal.
    /// Strings and the string-shaped domains (function, class) are deliberately compatible: a
    /// `callee_string` really is joinable with a `fun` column.
    fn unify(self, other: VarType) -> Option<VarType> {
        use VarType::*;
        match (self, other) {
            (Unknown, x) | (x, Unknown) => Some(x),
            (a, b) if a == b => Some(a),
            (Str, x) | (x, Str) if matches!(x, Function | Class) => Some(x),
            _ => None,
        }
    }
}

/// The execution plan for one rule.
#[derive(Clone, Debug)]
pub struct Plan {
    /// One per connected component of the body, in a deterministic order.
    pub components: Vec<Component>,
    /// Every variable the body binds, with its inferred type.
    pub var_types: BTreeMap<String, VarType>,
}

/// One connected component: a set of body items sharing variables, plus the order to run them
/// in and the variables they bind.
#[derive(Clone, Debug)]
pub struct Component {
    /// Indices into `Rule::body`, in the order the engine should evaluate them.
    pub steps: Vec<usize>,
    /// The variables this component binds, sorted. Also the tuple layout of its solutions.
    pub vars: Vec<String>,
}

/// Checks one rule and builds its plan. Errors accumulate into `errors`; a rule with errors
/// yields `None`.
pub fn plan_rule(rule: &Rule, errors: &mut DslErrors) -> Option<Plan> {
    let before = errors.len();
    let mut types: BTreeMap<String, VarType> = BTreeMap::new();

    // ---- 1. shape, and left-to-right modedness ----
    let mut bound: BTreeSet<String> = BTreeSet::new();
    for (i, item) in rule.body.iter().enumerate() {
        let elsewhere = vars_outside(rule, i);
        check_item(item, &elsewhere, &mut bound, &mut types, errors);
    }

    // ---- 2. head variables ----
    for head in &rule.heads {
        check_head(rule, head, &bound, &types, errors);
    }

    if errors.len() != before {
        return None;
    }

    // ---- 3. components ----
    let components = components_of(rule);
    Some(Plan {
        components,
        var_types: types,
    })
}

// ---------------------------------------------------------------------------
// Body checking
// ---------------------------------------------------------------------------

/// Whether an item can *generate* bindings, or only filter existing ones.
pub fn is_generator(item: &BodyItem) -> bool {
    match item {
        BodyItem::Atom(atom) => relation_arity(&atom.name).is_some() && atom.name != "regex_match",
        // A negation, a disjunction and a conjunction of filters are all filters: nothing in
        // them enumerates. (An `And` of generators would be one, but the grammar only builds
        // `And`/`Or` out of `&&`/`||`, which are boolean tests.)
        _ => false,
    }
}

fn check_item(
    item: &BodyItem,
    elsewhere: &BTreeSet<String>,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut DslErrors,
) {
    match item {
        BodyItem::Atom(atom) if is_generator(item) => {
            check_relation_atom(atom, bound, types, errors);
        }
        _ => {
            // Every filter is checked against what is bound so far, and binds nothing itself.
            let span = item_span(item);
            for var in required_vars(item, elsewhere) {
                if !bound.contains(&var) {
                    errors.push(DslError::Rule {
                        message: format!(
                            "'{var}' is not bound at this point. An operator can only test \
                             variables that an atom to its left has already bound; move the atom \
                             that binds '{var}' earlier in the body."
                        ),
                        span,
                    });
                }
            }
            check_filter_shape(item, errors);
        }
    }
}

/// The variables an item needs bound before it can run.
///
/// A generator needs none: it binds. A test needs all of them. A **negated group** needs only
/// the ones that occur elsewhere in the rule — the rest are local to the subquery and are
/// existentially quantified by it, which is what lets "no name of `F` matches this pattern" be
/// written at all:
///
/// ```text
/// source(F::return) :- fun(F), !(fun(F, name = N) && regex_match(N, "^get"));
/// ```
///
/// A negated *atom* is deliberately stricter, and matches the design: every variable in it must
/// be bound, with `_` as the way to say "any value at all" (`!fun(F, parent = _)`).
pub fn required_vars(item: &BodyItem, elsewhere: &BTreeSet<String>) -> BTreeSet<String> {
    if is_generator(item) {
        return BTreeSet::new();
    }
    let mut used = BTreeSet::new();
    collect_vars(item, &mut used);
    match item {
        BodyItem::Not(inner) if !matches!(&**inner, BodyItem::Atom(_)) => {
            used.retain(|v| elsewhere.contains(v));
            used
        }
        _ => used,
    }
}

/// Every variable the rule mentions outside body item `skip`: the other body items and every
/// head. A variable not in here is local to that item.
fn vars_outside(rule: &Rule, skip: usize) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, item) in rule.body.iter().enumerate() {
        if i != skip {
            collect_vars(item, &mut out);
        }
    }
    for head in &rule.heads {
        collect_head_vars(head, &mut out);
    }
    out
}

fn collect_head_vars(head: &Head, out: &mut BTreeSet<String>) {
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
}

fn check_relation_atom(
    atom: &Atom,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut DslErrors,
) {
    if !check_relation_shape(atom, errors) {
        return;
    }
    let col_types = column_types(&atom.name);
    for (i, col) in atom.columns.iter().enumerate() {
        if let Term::Var(v) = col {
            bind(v, col_types[i], bound, types, atom.span, errors);
        }
    }
    for attr in &atom.attrs {
        let want = attribute_type(&atom.name, &attr.name);
        check_attr_rhs(atom, attr, want, bound, types, errors);
    }
}

/// Arity and attribute names, with no binding.
///
/// Split out because a **negated** atom is checked for shape but binds nothing: `!fun(F, parent
/// = _)` must still be told that `parnet` is a typo.
fn check_relation_shape(atom: &Atom, errors: &mut DslErrors) -> bool {
    let Some(arity) = relation_arity(&atom.name) else {
        errors.push(DslError::Rule {
            message: unknown_relation_message(&atom.name),
            span: atom.span,
        });
        return false;
    };
    if atom.columns.len() != arity {
        errors.push(DslError::Rule {
            message: format!(
                "'{}' takes {arity} column(s); {} were given",
                atom.name,
                atom.columns.len()
            ),
            span: atom.span,
        });
        return false;
    }
    let honored = relation_attributes(&atom.name);
    let mut ok = true;
    for attr in &atom.attrs {
        if !honored.contains(&attr.name.as_str()) {
            errors.push(DslError::Rule {
                message: if honored.is_empty() {
                    format!("'{}' has no attributes", atom.name)
                } else {
                    format!(
                        "'{}' is not an attribute of '{}'; expected one of {}",
                        attr.name,
                        atom.name,
                        honored
                            .iter()
                            .map(|h| format!("'{h}'"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                span: attr.span,
            });
            ok = false;
        }
    }
    ok
}

/// An attribute's right-hand side: a constant, a bound variable (a test), or — for `=` only —
/// a fresh variable, which binds it.
fn check_attr_rhs(
    atom: &Atom,
    attr: &AttrConstraint,
    want: VarType,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut DslErrors,
) {
    match &attr.rhs {
        Rhs::Set(items) => {
            if attr.op != CmpOp::In {
                errors.push(DslError::Rule {
                    message: format!(
                        "a set is only valid with 'in'; '{}' takes a single value",
                        attr.op
                    ),
                    span: attr.span,
                });
                return;
            }
            for lit in items {
                check_literal_type(lit, want, attr.span, errors);
            }
        }
        Rhs::Term(Term::Lit(lit)) => {
            if attr.op == CmpOp::In {
                errors.push(DslError::Rule {
                    message: "'in' takes a set, e.g. in {\"a\", \"b\"}".to_string(),
                    span: attr.span,
                });
                return;
            }
            check_literal_type(lit, want, attr.span, errors);
        }
        Rhs::Term(Term::Wildcard) => {
            // `parent = _` reads as "has a parent, whatever it is". Only equality makes sense.
            if attr.op != CmpOp::Eq {
                errors.push(DslError::Rule {
                    message: format!("'_' is only meaningful with '='; found '{}'", attr.op),
                    span: attr.span,
                });
            }
        }
        Rhs::Term(Term::Var(v)) => {
            if attr.op == CmpOp::In {
                errors.push(DslError::Rule {
                    message: "'in' takes a set, e.g. in {\"a\", \"b\"}".to_string(),
                    span: attr.span,
                });
                return;
            }
            if attr.op == CmpOp::Eq && !bound.contains(v) {
                bind(v, want, bound, types, attr.span, errors);
            } else if !bound.contains(v) {
                errors.push(DslError::Rule {
                    message: format!(
                        "'{v}' is not bound at this point. Only '=' can bind a variable through \
                         an attribute; '{}' compares against one that is already bound.",
                        attr.op
                    ),
                    span: attr.span,
                });
            }
        }
    }
    let _ = atom;
}

fn check_literal_type(lit: &Literal, want: VarType, span: Span, errors: &mut DslErrors) {
    let got = match lit {
        Literal::Str(_) => VarType::Str,
        Literal::Int(_) => VarType::Int,
        Literal::Bool(_) => VarType::Bool,
    };
    if got.unify(want).is_none() {
        errors.push(DslError::Rule {
            message: format!("expected {}, found {} ({lit})", want.name(), got.name()),
            span,
        });
    }
}

fn bind(
    var: &str,
    ty: VarType,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    span: Span,
    errors: &mut DslErrors,
) {
    let entry = types.entry(var.to_string()).or_insert(VarType::Unknown);
    match entry.unify(ty) {
        Some(t) => *entry = t,
        None => errors.push(DslError::Rule {
            message: format!(
                "'{var}' is used as a {} here but was bound as a {} earlier",
                ty.name(),
                entry.name()
            ),
            span,
        }),
    }
    bound.insert(var.to_string());
}

/// Structural checks a filter needs beyond modedness: known operator, right arity.
fn check_filter_shape(item: &BodyItem, errors: &mut DslErrors) {
    match item {
        BodyItem::Atom(atom) => {
            if atom.name.starts_with('$') {
                // A comparison, built by the parser. Both sides must be comparable; the
                // parser guaranteed the shape.
                return;
            }
            if atom.name == "regex_match" {
                if atom.columns.len() != 2 {
                    errors.push(DslError::Rule {
                        message: format!(
                            "'regex_match' takes 2 columns (subject, pattern); {} were given",
                            atom.columns.len()
                        ),
                        span: atom.span,
                    });
                    return;
                }
                if !atom.attrs.is_empty() {
                    errors.push(DslError::Rule {
                        message: "'regex_match' has no attributes".to_string(),
                        span: atom.span,
                    });
                }
                match &atom.columns[1] {
                    Term::Lit(Literal::Str(pattern)) => {
                        if let Err(e) = regex::Regex::new(pattern) {
                            errors.push(DslError::Rule {
                                message: format!("invalid regex {pattern:?}: {e}"),
                                span: atom.span,
                            });
                        }
                    }
                    Term::Var(_) => {}
                    _ => errors.push(DslError::Rule {
                        message: "the second argument of 'regex_match' must be a string pattern"
                            .to_string(),
                        span: atom.span,
                    }),
                }
                return;
            }
            // A relation atom in filter position: negated, or inside a `&&` / `||`. It binds
            // nothing here, but its shape is still checked, and an unknown name still reads as
            // the typo it is.
            check_relation_shape(atom, errors);
        }
        BodyItem::Not(inner) => check_filter_shape(inner, errors),
        BodyItem::And(items) | BodyItem::Or(items) => {
            for i in items {
                check_filter_shape(i, errors);
            }
        }
    }
}

fn unknown_relation_message(name: &str) -> String {
    let mut known: Vec<&str> = BUILTIN_RELATIONS.to_vec();
    known.extend_from_slice(BUILTIN_OPERATORS);
    if OUTPUT_RELATIONS.contains(&name) {
        return format!(
            "'{name}' is an output relation and can only appear in a rule head, not in a body"
        );
    }
    let hint = known
        .iter()
        .find(|k| k.eq_ignore_ascii_case(name) || levenshtein_close(k, name))
        .map(|k| format!(" Did you mean '{k}'?"))
        .unwrap_or_default();
    format!(
        "'{name}' is not a built-in relation. Every relation name is built in, so this is a \
         typo rather than a relation this file could define. Known: {}.{hint}",
        known
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// One edit apart, cheaply: enough for a "did you mean" on a hand-typed relation name.
fn levenshtein_close(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

// ---------------------------------------------------------------------------
// Head checking
// ---------------------------------------------------------------------------

fn check_head(
    rule: &Rule,
    head: &Head,
    bound: &BTreeSet<String>,
    types: &BTreeMap<String, VarType>,
    errors: &mut DslErrors,
) {
    let require = |term: &Term, span: Span, what: &str, errors: &mut DslErrors| {
        if let Term::Var(v) = term
            && !bound.contains(v)
        {
            errors.push(DslError::Rule {
                message: format!(
                    "'{v}' appears in the head ({what}) but is not bound in the body. Every head \
                     variable must be bound by a body atom."
                ),
                span,
            });
        }
        if matches!(term, Term::Wildcard) {
            errors.push(DslError::Rule {
                message: format!("'_' cannot be used in a head ({what}): it binds nothing"),
                span,
            });
        }
    };

    let ports: Vec<&PortExpr> = match &head.kind {
        HeadKind::Source { port, .. } | HeadKind::Sink { port, .. } => vec![port],
        HeadKind::Propagation { flow } | HeadKind::Bridge { flow } => vec![&flow.left, &flow.right],
        HeadKind::AccessPath { .. } => vec![],
    };

    // Resolve each port's anchor: its own, else the head atom's.
    let mut anchors: Vec<Option<&Term>> = Vec::new();
    for port in &ports {
        let anchor = port.anchor.as_ref().or(head.anchor.as_ref());
        match anchor {
            Some(term) => {
                require(term, port.span, "the port anchor", errors);
                if let Term::Var(v) = term {
                    match types.get(v).copied().unwrap_or(VarType::Unknown) {
                        VarType::Function | VarType::Site | VarType::Str | VarType::Unknown => {}
                        other => errors.push(DslError::Rule {
                            message: format!(
                                "'{v}' is a {} and cannot anchor a port; a port hangs off a \
                                 function or a call site",
                                other.name()
                            ),
                            span: port.span,
                        }),
                    }
                }
            }
            None => errors.push(DslError::Rule {
                message: "this port has no anchor. Write 'F::return' (or put the anchor on the \
                          atom, as in 'F::source(return)') so it names the function it belongs to."
                    .to_string(),
                span: port.span,
            }),
        }
        anchors.push(anchor);
        if let PortBase::ArgVar(v) = &port.port.base {
            if !bound.contains(v) {
                errors.push(DslError::Rule {
                    message: format!(
                        "'{v}' indexes an argument in the head but is not bound in the body; bind \
                         it with 'param(F, {v})'"
                    ),
                    span: port.span,
                });
            } else if let Some(t) = types.get(v)
                && t.unify(VarType::Int).is_none()
            {
                errors.push(DslError::Rule {
                    message: format!("'{v}' indexes an argument but was bound as a {}", t.name()),
                    span: port.span,
                });
            }
        }
    }

    match &head.kind {
        HeadKind::Propagation { flow } => {
            // A propagation is one function's summary. Two anchors that are not the same
            // syntactic term cannot be the same function, and a call-site anchor is not a
            // function at all.
            let same = match (
                anchors.first().copied().flatten(),
                anchors.get(1).copied().flatten(),
            ) {
                (Some(a), Some(b)) => same_term(a, b),
                _ => true,
            };
            if !same {
                errors.push(DslError::Rule {
                    message: "a propagation is one function's summary, so both ports must be \
                              anchored at the same function. Use 'bridge' to connect two \
                              functions."
                        .to_string(),
                    span: flow.span,
                });
            }
            for anchor in anchors.iter().flatten() {
                if let Term::Var(v) = anchor
                    && types.get(v) == Some(&VarType::Site)
                {
                    errors.push(DslError::Rule {
                        message: format!(
                            "'{v}' is a call site; a propagation is a whole-function summary and \
                             cannot depend on one. Use 'bridge', or anchor at the callee."
                        ),
                        span: flow.span,
                    });
                }
            }
        }
        HeadKind::Bridge { flow } => {
            let same = match (
                anchors.first().copied().flatten(),
                anchors.get(1).copied().flatten(),
            ) {
                (Some(a), Some(b)) => same_term(a, b),
                _ => false,
            };
            if same {
                errors.push(DslError::Rule {
                    message: "both ports of this bridge are anchored at the same function, which \
                              is a propagation. Write 'propagation' instead."
                        .to_string(),
                    span: flow.span,
                });
            }
            // A bridge emits `call` rows, which the index fixpoint *consumes*. Anchoring one at
            // a site would name only the statically emitted calls — precisely not the ones
            // (a call through a table field, a `dlsym`'d pointer, a virtual dispatch) a bridge
            // exists for. Anchoring at the callee covers every call site of it instead,
            // because the bridge attaches inside the callee and its summary composes.
            for anchor in anchors.iter().flatten() {
                if let Term::Var(v) = anchor
                    && types.get(v) == Some(&VarType::Site)
                {
                    errors.push(DslError::Rule {
                        message: format!(
                            "'{v}' is a call site, and a bridge attaches inside a function rather \
                             than at one call. Anchor at the callee — bind it with \
                             'callsite(_, {v}, callee_string = F)' and write 'F::' — which covers \
                             every call site of it."
                        ),
                        span: flow.span,
                    });
                }
            }
        }
        _ => {}
    }

    let _ = rule;
}

fn same_term(a: &Term, b: &Term) -> bool {
    match (a, b) {
        (Term::Var(x), Term::Var(y)) => x == y,
        (Term::Lit(x), Term::Lit(y)) => x == y,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Components and ordering
// ---------------------------------------------------------------------------

fn components_of(rule: &Rule) -> Vec<Component> {
    let n = rule.body.len();
    let mut vars: Vec<BTreeSet<String>> = Vec::with_capacity(n);
    for item in &rule.body {
        let mut s = BTreeSet::new();
        collect_vars(item, &mut s);
        vars.push(s);
    }
    // Union-find over items sharing a variable.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut by_var: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, set) in vars.iter().enumerate() {
        for v in set {
            match by_var.get(v.as_str()) {
                Some(&j) => {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    by_var.insert(v.as_str(), i);
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups
        .into_values()
        .map(|items| {
            let mut component_vars: BTreeSet<String> = BTreeSet::new();
            for &i in &items {
                component_vars.extend(vars[i].iter().cloned());
            }
            Component {
                steps: order_steps(rule, &items),
                vars: component_vars.into_iter().collect(),
            }
        })
        .collect()
}

/// Greedy, binding-consistent join order.
///
/// Filters run as soon as their variables are bound — that is the whole point of reordering —
/// and generators are picked cheapest-first given what is already bound. An indexed lookup
/// (`fun(F, name = "append")`) beats a scan, and a scan of the smallest relation beats a scan of
/// the biggest. The order is a heuristic; correctness rests only on never scheduling a filter
/// before its variables exist, which the loop enforces.
fn order_steps(rule: &Rule, items: &[usize]) -> Vec<usize> {
    let mut remaining: Vec<usize> = items.to_vec();
    let mut bound: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let mut best: Option<(usize, u32)> = None;
        for (pos, &i) in remaining.iter().enumerate() {
            let item = &rule.body[i];
            let elsewhere = vars_outside(rule, i);
            let cost = if !is_generator(item) {
                if !required_vars(item, &elsewhere)
                    .iter()
                    .all(|v| bound.contains(v))
                {
                    continue;
                }
                0
            } else {
                generator_cost(item, &bound)
            };
            if best.is_none_or(|(_, c)| cost < c) {
                best = Some((pos, cost));
            }
            if cost == 0 {
                break;
            }
        }
        // Every remaining item is a filter whose variables are still unbound. The left-to-right
        // check above already reported that, so this can only be reached on an errored rule;
        // keep the order stable rather than looping.
        let Some((pos, _)) = best else {
            out.append(&mut remaining);
            break;
        };
        let i = remaining.remove(pos);
        // Only a generator binds. A filter's variables are already bound (the loop above only
        // schedules it then), so adding them would be a no-op — except for a negated group's
        // locals, which are *not* bound and must not be treated as such.
        if is_generator(&rule.body[i]) {
            collect_vars(&rule.body[i], &mut bound);
        }
        out.push(i);
    }
    out
}

/// A rough relative cost of enumerating one generator given the current bindings. Lower is
/// better; the absolute numbers mean nothing outside this comparison.
fn generator_cost(item: &BodyItem, bound: &BTreeSet<String>) -> u32 {
    let BodyItem::Atom(atom) = item else {
        return 100;
    };
    let column_bound = atom.columns.iter().any(|c| match c {
        Term::Lit(_) => true,
        Term::Var(v) => bound.contains(v),
        Term::Wildcard => false,
    });
    // An equality on an indexed attribute is as good as a bound column: `fun(F, name = "x")` is
    // a hash lookup, not a scan.
    let keyed = atom.attrs.iter().any(|a| {
        a.op == CmpOp::Eq
            && matches!(
                a.name.as_str(),
                "name" | "parent" | "signature" | "qualified-id"
            )
            && match &a.rhs {
                Rhs::Term(Term::Lit(_)) => true,
                Rhs::Term(Term::Var(v)) => bound.contains(v),
                _ => false,
            }
    });
    let set_keyed = atom.attrs.iter().any(|a| {
        a.op == CmpOp::In
            && matches!(
                a.name.as_str(),
                "name" | "parent" | "signature" | "qualified-id"
            )
            && matches!(&a.rhs, Rhs::Set(_))
    });
    let base = match atom.name.as_str() {
        "subclass" | "subclass*" | "subclass+" => 10,
        "uses_field" => 20,
        "param" => 25,
        "fun" => 30,
        "callsite" => 40,
        _ => 50,
    };
    if column_bound {
        1
    } else if keyed {
        2
    } else if set_keyed {
        3
    } else {
        base
    }
}

/// The types the built-in relations' positional columns range over.
fn column_types(name: &str) -> &'static [VarType] {
    match name {
        "fun" => &[VarType::Function],
        "param" => &[VarType::Function, VarType::Int],
        "callsite" => &[VarType::Function, VarType::Site],
        "subclass" | "subclass*" | "subclass+" => &[VarType::Class, VarType::Class],
        "uses_field" => &[VarType::Function, VarType::Str],
        _ => &[VarType::Unknown, VarType::Unknown],
    }
}

/// The type one relation's attribute takes.
pub fn attribute_type(relation: &str, attr: &str) -> VarType {
    match (relation, attr) {
        ("fun", "arity") => VarType::Int,
        ("fun", "has_code") => VarType::Bool,
        ("fun", "parent") => VarType::Class,
        ("callsite", "callee_string") => VarType::Function,
        _ => VarType::Str,
    }
}

/// Every variable mentioned anywhere in an item, including inside attribute right-hand sides.
pub fn collect_vars(item: &BodyItem, out: &mut BTreeSet<String>) {
    match item {
        BodyItem::Atom(atom) => {
            for c in &atom.columns {
                if let Term::Var(v) = c {
                    out.insert(v.clone());
                }
            }
            for a in &atom.attrs {
                if let Rhs::Term(Term::Var(v)) = &a.rhs {
                    out.insert(v.clone());
                }
            }
        }
        BodyItem::Not(inner) => collect_vars(inner, out),
        BodyItem::And(items) | BodyItem::Or(items) => {
            for i in items {
                collect_vars(i, out);
            }
        }
    }
}

fn item_span(item: &BodyItem) -> Span {
    match item {
        BodyItem::Atom(atom) => atom.span,
        BodyItem::Not(inner) => item_span(inner),
        BodyItem::And(items) | BodyItem::Or(items) => {
            items.first().map(item_span).unwrap_or_default()
        }
    }
}
