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

use hashbrown::HashSet;
use regex::Regex;

use crate::facts::Str;

use super::ast::*;
use super::eval::Value;
use super::{PortcullisError, PortcullisErrors};

/// What a variable ranges over. Used only for the handful of checks that need it; an
/// [`VarType::Unknown`] never provokes a diagnostic.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VarType {
    Unknown,
    Function,
    /// A call site, bound by `callsite`'s second column.
    Site,
    /// A static call *signature*, bound by `callsig`'s only column. Distinct from
    /// [`Self::Function`]: the two universes overlap but neither contains the other, and only
    /// this one can anchor a `dispatch` head.
    CallSig,
    Class,
    Str,
    Int,
    Bool,
    /// A local variable of one function, bound by `local`'s second column. Opaque: it is not
    /// an integer, so it cannot index an argument, nor an argument index a local.
    Local,
}

impl VarType {
    fn name(self) -> &'static str {
        match self {
            VarType::Unknown => "value",
            VarType::Function => "function",
            VarType::Site => "call site",
            VarType::CallSig => "call signature",
            VarType::Class => "class",
            VarType::Str => "string",
            VarType::Int => "integer",
            VarType::Bool => "boolean",
            VarType::Local => "local variable",
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
    /// What each positive `local` atom asks for, by [`AtomPlan::probe`]: the name it constrains,
    /// as the unmatched-local diagnostic reports it.
    pub local_probes: Vec<String>,
}

/// One connected component: a set of body items sharing variables, compiled into the steps the
/// engine runs.
#[derive(Clone, Debug)]
pub struct Component {
    /// Indices into `Rule::body`, in the order the engine evaluates them. Parallel to `body`.
    pub steps: Vec<usize>,
    /// The head variables this component binds, sorted: the tuple layout of its solutions. Body
    /// variables no head reads are projected away, so a component no head reads has no `vars`
    /// and stores at most the empty tuple — it is an existence check.
    pub vars: Vec<String>,
    /// The slot each of `vars` lives in.
    pub outputs: Vec<Slot>,
    pub body: Vec<Step>,
    /// How many slots the environment has. Every variable of the rule gets one, group locals
    /// included, so one environment serves the component and all its subqueries.
    pub slots: usize,
    /// From this step on nothing binds an output, so one witness of the remaining steps is as
    /// good as all of them.
    pub witness_from: usize,
}

/// A variable's index in the evaluation environment.
pub type Slot = usize;

/// One compiled body item.
#[derive(Clone, Debug)]
pub enum Step {
    /// A relation atom that enumerates rows and binds what it outputs.
    Atom(AtomPlan),
    /// A test of what is already bound.
    Filter(Filter),
}

/// A relation atom, resolved against what is bound when it runs.
#[derive(Clone, Debug)]
pub struct AtomPlan {
    pub rel: Relation,
    /// How the candidate rows are found.
    pub access: Access,
    /// Parallel to the atom's columns.
    pub columns: Vec<ColumnPat>,
    /// In written order. Columns are matched first, then attributes, which is the order the mode
    /// check binds in: a variable's first occurrence binds it and every later one tests it.
    pub attrs: Vec<(Attr, AttrPat)>,
    /// For a `local` atom the rule's solutions depend on, its index in [`Plan::local_probes`]:
    /// the engine records whether it ever matched, so a rule that matched nothing because of it
    /// can say so.
    pub probe: Option<usize>,
}

impl AtomPlan {
    /// The slots this atom binds.
    fn binds(&self) -> impl Iterator<Item = Slot> + '_ {
        let columns = self.columns.iter().filter_map(|c| match c {
            ColumnPat::Bind(s) => Some(*s),
            ColumnPat::Test(_) => None,
        });
        let attrs = self.attrs.iter().filter_map(|(_, a)| match a {
            AttrPat::Bind(s) => Some(*s),
            _ => None,
        });
        columns.chain(attrs)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Relation {
    Fun,
    /// A function and one of its names: the simple name, then the other names the frontend
    /// knows it by.
    NameOf,
    CallSig,
    Param,
    /// A function and one of its declared locals.
    Local,
    CallSite,
    /// `subclass` is neither, `subclass*` both, `subclass+` only `deep`.
    Subclass {
        reflexive: bool,
        deep: bool,
    },
    UsesField,
}

impl Relation {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "fun" => Relation::Fun,
            "name_of" => Relation::NameOf,
            "callsig" => Relation::CallSig,
            "param" => Relation::Param,
            "local" => Relation::Local,
            "callsite" => Relation::CallSite,
            "subclass" => Relation::Subclass {
                reflexive: false,
                deep: false,
            },
            "subclass*" => Relation::Subclass {
                reflexive: true,
                deep: true,
            },
            "subclass+" => Relation::Subclass {
                reflexive: false,
                deep: true,
            },
            "uses_field" => Relation::UsesField,
            _ => return None,
        })
    }

    /// The columns `ProgramFacts` can look rows up by, most selective first.
    ///
    /// This and [`Self::indexed_attrs`] are the one statement of what is indexed: the cost model
    /// ranks atoms by the [`Access`] they allow, and the engine follows that same `Access`.
    fn indexed_columns(self) -> &'static [usize] {
        match self {
            Relation::Fun
            | Relation::CallSig
            | Relation::Param
            | Relation::Local
            | Relation::UsesField => &[0],
            // A function's names, the functions with a name.
            Relation::NameOf => &[0, 1],
            // By id, then by caller.
            Relation::CallSite => &[1, 0],
            // `supers` of a known sub, `subs` of a known super.
            Relation::Subclass { .. } => &[0, 1],
        }
    }

    /// The attributes `ProgramFacts` can look rows up by.
    fn indexed_attrs(self) -> &'static [Attr] {
        match self {
            Relation::Fun => &[Attr::Name, Attr::Parent, Attr::Signature],
            Relation::CallSig => &[Attr::Name, Attr::Parent, Attr::Signature, Attr::QualifiedId],
            Relation::CallSite => &[Attr::CalleeString],
            _ => &[],
        }
    }

    /// The relative cost of a full scan; see [`Access::cost`].
    fn scan_cost(self) -> u32 {
        match self {
            Relation::Subclass { .. } => 10,
            Relation::UsesField => 20,
            Relation::Param => 25,
            // Ranked beside `fun`: the same four attributes are indexed, over a universe that is
            // typically an order of magnitude smaller.
            Relation::CallSig => 28,
            Relation::Fun => 30,
            // Every function, once per name.
            Relation::NameOf => 32,
            Relation::CallSite => 40,
            // Every function, once per local: the biggest table there is. Not indexed by name,
            // so that whatever selects the function runs first and the unmatched-local report
            // can say which functions it asked about.
            Relation::Local => 45,
        }
    }
}

/// Every attribute a built-in relation honors.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Attr {
    Name,
    Parent,
    Signature,
    QualifiedId,
    Language,
    Import,
    Arity,
    HasCode,
    CalleeString,
}

impl Attr {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "name" => Attr::Name,
            "parent" => Attr::Parent,
            "signature" => Attr::Signature,
            "qualified-id" => Attr::QualifiedId,
            "language" => Attr::Language,
            "import" => Attr::Import,
            "arity" => Attr::Arity,
            "has_code" => Attr::HasCode,
            "callee_string" => Attr::CalleeString,
            _ => return None,
        })
    }
}

/// How an atom finds its candidate rows.
#[derive(Clone, Debug)]
pub enum Access {
    Scan,
    /// Through the index on this column, which is a [`ColumnPat::Test`] of a known value.
    Column(usize),
    /// Through the index on this attribute.
    Attr(Attr, Key),
}

impl Access {
    /// A rough relative cost of enumerating an atom this way. Lower is better; the absolute
    /// numbers mean nothing outside this comparison.
    fn cost(&self, rel: Relation) -> u32 {
        match self {
            Access::Column(_) => 1,
            Access::Attr(_, Key::One(_)) => 2,
            Access::Attr(_, Key::Set(_)) => 3,
            Access::Scan => rel.scan_cost(),
        }
    }
}

/// What an attribute lookup keys on.
#[derive(Clone, Debug)]
pub enum Key {
    One(Operand),
    /// `in {...}`: the union of one lookup per member.
    Set(Vec<Str>),
}

/// A value a step reads.
#[derive(Clone, Debug)]
pub enum Operand {
    Const(Value),
    Var(Slot),
    /// `_`.
    Any,
}

#[derive(Clone, Debug)]
pub enum ColumnPat {
    /// The variable's first occurrence: the row's value binds it.
    Bind(Slot),
    /// The row's value must equal this one. [`Operand::Any`] accepts every row.
    Test(Operand),
}

#[derive(Clone, Debug)]
pub enum AttrPat {
    /// `= V` with `V` unbound: one alternative per value the attribute has.
    Bind(Slot),
    /// Some value compares true; for `!=`, no value is equal.
    Cmp(CmpOp, Operand),
    In(HashSet<Value>),
    /// `= _`: the attribute has a value.
    Present,
    /// A shape that matches nothing. The checker rejects it where it looks; a negated atom's
    /// attributes are checked for name only.
    Never,
}

/// A test of the current bindings.
#[derive(Clone, Debug)]
pub enum Filter {
    /// A relation atom in filter position: some row matches. Variables it finds unbound are
    /// existential.
    Exists(Box<AtomPlan>),
    /// A `&&` group: the subquery has a solution. Its locals are existential.
    Group(Vec<Step>),
    Not(Box<Filter>),
    Or(Vec<Filter>),
    Cmp(Operand, CmpOp, Operand),
    In(Operand, HashSet<Value>),
    /// `regex_match(subject, pattern)`.
    Regex(Operand, Pattern),
    Never,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Compiled(Regex),
    /// A pattern that arrives through a variable, compiled at use.
    Var(Slot),
}

/// Checks one rule and builds its plan. Errors accumulate into `errors`; a rule with errors
/// yields `None`.
pub fn plan_rule(rule: &Rule, errors: &mut PortcullisErrors) -> Option<Plan> {
    let before = errors.len();
    let mut types: BTreeMap<String, VarType> = BTreeMap::new();
    let visible = visible_vars(&rule.heads, &rule.body);

    // ---- 1. shape, and left-to-right modedness ----
    let mut bound: BTreeSet<String> = BTreeSet::new();
    for item in &rule.body {
        check_item(item, &visible, &mut bound, &mut types, errors);
    }

    // ---- 2. head variables ----
    for head in &rule.heads {
        check_head(head, &bound, &types, errors);
    }

    if errors.len() != before {
        return None;
    }

    // ---- 3. components ----
    let components = components_of(rule, &visible);
    // A mode is read as each import is lowered (SSA, then codegen), so it has to be known while
    // that import is in hand. A rule of two components is joined only after the import loop.
    if components.len() > 1
        && let Some((head, mode)) = rule.heads.iter().find_map(|h| match h.kind {
            HeadKind::Mode(mode) => Some((h, mode)),
            _ => None,
        })
    {
        let name = mode.relation_name();
        errors.push(PortcullisError::Rule {
            message: format!(
                "a rule with a '{name}' head must be satisfied within one import: its body has \
                 parts that share no variable, and those are joined only after every import is \
                 lowered, too late to change how a body is lowered. Connect the parts, or move \
                 the '{name}' head to a rule of its own."
            ),
            span: head.span,
        });
        return None;
    }
    let mut components = components;
    let mut local_probes = Vec::new();
    for component in &mut components {
        for (step, &item) in component.body.iter_mut().zip(&component.steps) {
            if let (Step::Atom(atom), BodyItem::Atom(written)) = (step, &rule.body[item])
                && atom.rel == Relation::Local
            {
                atom.probe = Some(local_probes.len());
                local_probes.push(local_name_constraint(written));
            }
        }
    }
    Some(Plan {
        components,
        var_types: types,
        local_probes,
    })
}

/// How a `local` atom constrains the name, for a diagnostic: `buf` for `name = "buf"`, else the
/// constraint as written, else `_`.
fn local_name_constraint(atom: &Atom) -> String {
    let Some(attr) = atom.attrs.iter().find(|a| a.name == "name") else {
        return "_".to_string();
    };
    match (&attr.rhs, attr.op) {
        (Rhs::Term(Term::Lit(Literal::Str(s))), CmpOp::Eq) => s.clone(),
        (Rhs::Term(t), op) => format!("name {op} {}", term_text(t)),
        (Rhs::Set(items), _) => format!(
            "name in {{{}}}",
            items
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn term_text(term: &Term) -> String {
    match term {
        Term::Var(v) => v.clone(),
        Term::Lit(lit) => lit.to_string(),
        Term::Wildcard => "_".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Body checking
// ---------------------------------------------------------------------------

/// Whether an item can *generate* bindings, or only filter existing ones.
fn is_generator(item: &BodyItem) -> bool {
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
    visible: &BTreeSet<String>,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut PortcullisErrors,
) {
    match item {
        BodyItem::Atom(atom) if is_generator(item) => {
            check_relation_atom(atom, bound, types, errors);
        }
        _ => {
            // Every filter is checked against what is bound so far, and binds nothing itself.
            let span = item_span(item);
            for var in required_vars(item, visible) {
                if !bound.contains(&var) {
                    errors.push(PortcullisError::Rule {
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
/// the ones visible outside it (see [`visible_vars`]) — the rest are local to the subquery and
/// are existentially quantified by it, which is what lets "no name of `F` matches this pattern"
/// be written at all:
///
/// ```text
/// source(F::return) :- fun(F), !(name_of(F, N) && regex_match(N, "^get"));
/// ```
///
/// A negated *atom* is deliberately stricter, and matches the design: every variable in it must
/// be bound, with `_` as the way to say "any value at all" (`!fun(F, parent = _)`).
fn required_vars(item: &BodyItem, visible: &BTreeSet<String>) -> BTreeSet<String> {
    if is_generator(item) {
        return BTreeSet::new();
    }
    let mut used = BTreeSet::new();
    collect_vars(item, &mut used);
    if is_negated_group(item) {
        used.retain(|v| visible.contains(v));
    }
    used
}

/// `!(...)` over anything but a single atom: the one item that has locals.
fn is_negated_group(item: &BodyItem) -> bool {
    matches!(item, BodyItem::Not(inner) if !matches!(&**inner, BodyItem::Atom(_)))
}

/// The variables a negated group among `items` can see from outside itself: every head variable,
/// and every variable of an item that is not itself a negated group. A group's other variables
/// are its locals, even when another group happens to use the same name — two groups cannot
/// share a local, since neither binds anything the other sees.
fn visible_vars(heads: &[Head], items: &[BodyItem]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for head in heads {
        collect_head_vars(head, &mut out);
    }
    add_positive_vars(items, &mut out);
    out
}

fn add_positive_vars(items: &[BodyItem], out: &mut BTreeSet<String>) {
    for item in items {
        if !is_negated_group(item) {
            collect_vars(item, out);
        }
    }
}

fn collect_head_vars(head: &Head, out: &mut BTreeSet<String>) {
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
}

fn check_relation_atom(
    atom: &Atom,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut PortcullisErrors,
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
        check_attr_rhs(attr, want, bound, types, errors);
    }
}

/// Arity and attribute names, with no binding.
///
/// Split out because a **negated** atom is checked for shape but binds nothing: `!fun(F, parent
/// = _)` must still be told that `parnet` is a typo.
fn check_relation_shape(atom: &Atom, errors: &mut PortcullisErrors) -> bool {
    let Some(arity) = relation_arity(&atom.name) else {
        errors.push(PortcullisError::Rule {
            message: unknown_relation_message(&atom.name),
            span: atom.span,
        });
        return false;
    };
    if atom.columns.len() != arity {
        errors.push(PortcullisError::Rule {
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
            errors.push(PortcullisError::Rule {
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
    attr: &AttrConstraint,
    want: VarType,
    bound: &mut BTreeSet<String>,
    types: &mut BTreeMap<String, VarType>,
    errors: &mut PortcullisErrors,
) {
    match &attr.rhs {
        Rhs::Set(items) => {
            if attr.op != CmpOp::In {
                errors.push(PortcullisError::Rule {
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
                errors.push(PortcullisError::Rule {
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
                errors.push(PortcullisError::Rule {
                    message: format!("'_' is only meaningful with '='; found '{}'", attr.op),
                    span: attr.span,
                });
            }
        }
        Rhs::Term(Term::Var(v)) => {
            if attr.op == CmpOp::In {
                errors.push(PortcullisError::Rule {
                    message: "'in' takes a set, e.g. in {\"a\", \"b\"}".to_string(),
                    span: attr.span,
                });
                return;
            }
            if attr.op == CmpOp::Eq && !bound.contains(v) {
                bind(v, want, bound, types, attr.span, errors);
            } else if !bound.contains(v) {
                errors.push(PortcullisError::Rule {
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
}

fn check_literal_type(lit: &Literal, want: VarType, span: Span, errors: &mut PortcullisErrors) {
    let got = match lit {
        Literal::Str(_) => VarType::Str,
        Literal::Int(_) => VarType::Int,
        Literal::Bool(_) => VarType::Bool,
    };
    if got.unify(want).is_none() {
        errors.push(PortcullisError::Rule {
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
    errors: &mut PortcullisErrors,
) {
    let entry = types.entry(var.to_string()).or_insert(VarType::Unknown);
    match entry.unify(ty) {
        Some(t) => *entry = t,
        None => errors.push(PortcullisError::Rule {
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
fn check_filter_shape(item: &BodyItem, errors: &mut PortcullisErrors) {
    match item {
        BodyItem::Atom(atom) => {
            if atom.name.starts_with('$') {
                // A comparison, built by the parser. Both sides must be comparable; the
                // parser guaranteed the shape.
                return;
            }
            if atom.name == "regex_match" {
                if atom.columns.len() != 2 {
                    errors.push(PortcullisError::Rule {
                        message: format!(
                            "'regex_match' takes 2 columns (subject, pattern); {} were given",
                            atom.columns.len()
                        ),
                        span: atom.span,
                    });
                    return;
                }
                if !atom.attrs.is_empty() {
                    errors.push(PortcullisError::Rule {
                        message: "'regex_match' has no attributes".to_string(),
                        span: atom.span,
                    });
                }
                match &atom.columns[1] {
                    Term::Lit(Literal::Str(pattern)) => {
                        if let Err(e) = regex::Regex::new(pattern) {
                            errors.push(PortcullisError::Rule {
                                message: format!("invalid regex {pattern:?}: {e}"),
                                span: atom.span,
                            });
                        }
                    }
                    Term::Var(_) => {}
                    _ => errors.push(PortcullisError::Rule {
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

/// A `local(L)` port: only a source or sink names one, the way `Variable(...)` was the JSON's
/// source/sink-only port, and it hangs off the function `L` is a local of, never a call site.
fn check_local_port(
    head: &Head,
    port: &PortExpr,
    anchor: Option<&Term>,
    v: &str,
    bound: &BTreeSet<String>,
    types: &BTreeMap<String, VarType>,
    errors: &mut PortcullisErrors,
) {
    let mut error = |message: String| {
        errors.push(PortcullisError::Rule {
            message,
            span: port.span,
        })
    };
    if !matches!(head.kind, HeadKind::Source { .. } | HeadKind::Sink { .. }) {
        error("a 'local(L)' port is valid only on a source or sink".to_string());
    }
    if let Some(Term::Var(a)) = anchor
        && types.get(a) == Some(&VarType::Site)
    {
        error(format!(
            "'{a}' is a call site, and a local belongs to a function; anchor 'local({v})' at the \
             function 'local' bound it with"
        ));
    }
    if !bound.contains(v) {
        error(format!(
            "'{v}' names a local in the head but is not bound in the body; bind it with \
             'local(F, {v})'"
        ));
    } else if let Some(t) = types.get(v)
        && t.unify(VarType::Local).is_none()
    {
        error(format!(
            "'{v}' names a local but was bound as a {}; bind it with 'local(F, {v})'",
            t.name()
        ));
    }
}

fn check_head(
    head: &Head,
    bound: &BTreeSet<String>,
    types: &BTreeMap<String, VarType>,
    errors: &mut PortcullisErrors,
) {
    let require = |term: &Term, span: Span, what: &str, errors: &mut PortcullisErrors| {
        if let Term::Var(v) = term
            && !bound.contains(v)
        {
            errors.push(PortcullisError::Rule {
                message: format!(
                    "'{v}' appears in the head ({what}) but is not bound in the body. Every head \
                     variable must be bound by a body atom."
                ),
                span,
            });
        }
        if matches!(term, Term::Wildcard) {
            errors.push(PortcullisError::Rule {
                message: format!("'_' cannot be used in a head ({what}): it binds nothing"),
                span,
            });
        }
    };

    let ports: Vec<&PortExpr> = match &head.kind {
        HeadKind::Source { port, .. } | HeadKind::Sink { port, .. } => vec![port],
        kind => kind
            .flow()
            .map_or_else(Vec::new, |f| vec![&f.left, &f.right]),
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
                        VarType::Function
                        | VarType::Site
                        | VarType::CallSig
                        | VarType::Str
                        | VarType::Unknown => {}
                        other => errors.push(PortcullisError::Rule {
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
            None => errors.push(PortcullisError::Rule {
                message: "this port has no anchor. Write 'F::return' (or put the anchor on the \
                          atom, as in 'F::source(return)') so it names the function it belongs to."
                    .to_string(),
                span: port.span,
            }),
        }
        anchors.push(anchor);
        if let PortBase::Local(v) = &port.port.base {
            check_local_port(head, port, anchor, v, bound, types, errors);
        }
        if let PortBase::ArgVar(v) = &port.port.base {
            if !bound.contains(v) {
                errors.push(PortcullisError::Rule {
                    message: format!(
                        "'{v}' indexes an argument in the head but is not bound in the body; bind \
                         it with 'param(F, {v})'"
                    ),
                    span: port.span,
                });
            } else if let Some(t) = types.get(v)
                && t.unify(VarType::Int).is_none()
            {
                errors.push(PortcullisError::Rule {
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
                errors.push(PortcullisError::Rule {
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
                    errors.push(PortcullisError::Rule {
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
                errors.push(PortcullisError::Rule {
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
                    errors.push(PortcullisError::Rule {
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
        HeadKind::Dispatch { disposition } => {
            // Every form anchors at a signature key. The anchor is the whole subject of the
            // attribute forms, so a missing one is reported here rather than by the port loop
            // above, which those forms never enter.
            let span = disposition.flow().map_or(head.span, |f| f.span);
            match head
                .anchor
                .as_ref()
                .or_else(|| anchors.iter().flatten().copied().next())
            {
                Some(Term::Var(v)) => {
                    if let Some(t) = types.get(v)
                        && !matches!(t, VarType::CallSig | VarType::Unknown)
                    {
                        errors.push(PortcullisError::Rule {
                            message: format!(
                                "'{v}' is a {} and cannot anchor a 'dispatch' head; a dispatch \
                                 model attaches to a call signature. Bind it with 'callsig({v})'.",
                                t.name()
                            ),
                            span,
                        });
                    }
                }
                Some(_) => {}
                None => errors.push(PortcullisError::Rule {
                    message: "this 'dispatch' head has no anchor. Write 'K::dispatch(...)' so it \
                              names the call signature it models."
                        .to_string(),
                    span,
                }),
            }
            // Both ports of a dispatch flow describe the *same* signature's summary, exactly as
            // a propagation describes one function's.
            if let Some(flow) = disposition.flow() {
                let same = match (
                    anchors.first().copied().flatten(),
                    anchors.get(1).copied().flatten(),
                ) {
                    (Some(a), Some(b)) => same_term(a, b),
                    _ => true,
                };
                if !same {
                    errors.push(PortcullisError::Rule {
                        message: "a dispatch model is one signature's summary, so both ports \
                                  must be anchored at the same 'callsig' key."
                            .to_string(),
                        span: flow.span,
                    });
                }
            }
        }
        HeadKind::Mode(mode) => {
            let Some(term) = head.anchor.as_ref() else {
                return;
            };
            let name = mode.relation_name();
            require(
                term,
                head.span,
                &format!("the function '{name}' marks"),
                errors,
            );
            if let Term::Var(v) = term
                && let Some(t) = types.get(v)
                && !matches!(t, VarType::Function | VarType::Str | VarType::Unknown)
            {
                errors.push(PortcullisError::Rule {
                    message: format!(
                        "'{v}' is a {} and cannot be marked '{name}'; the mark applies to a \
                         function's body. Bind it with 'fun({v})'.",
                        t.name()
                    ),
                    span: head.span,
                });
            }
        }
        _ => {}
    }
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

fn components_of(rule: &Rule, visible: &BTreeSet<String>) -> Vec<Component> {
    let n = rule.body.len();
    // What connects an item to the rest of the body. A negated group's locals connect nothing:
    // they are quantified inside it, so two groups that reuse a local's name are unrelated.
    let mut vars: Vec<BTreeSet<String>> = Vec::with_capacity(n);
    for item in &rule.body {
        let mut s = BTreeSet::new();
        collect_vars(item, &mut s);
        if is_negated_group(item) {
            s.retain(|v| visible.contains(v));
        }
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

    let mut head_vars: BTreeSet<String> = BTreeSet::new();
    for head in &rule.heads {
        collect_head_vars(head, &mut head_vars);
    }
    let compiler = Compiler::new(rule);
    groups
        .into_values()
        .map(|items| {
            let refs: Vec<&BodyItem> = items.iter().map(|&i| &rule.body[i]).collect();
            let (order, body) = compiler.plan(&refs, BTreeSet::new(), visible);
            let output_vars: Vec<String> = items
                .iter()
                .flat_map(|&i| vars[i].iter())
                .filter(|v| head_vars.contains(*v))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let outputs: Vec<Slot> = output_vars.iter().map(|v| compiler.slot(v)).collect();
            let witness_from = body
                .iter()
                .rposition(|step| match step {
                    Step::Atom(atom) => atom.binds().any(|s| outputs.contains(&s)),
                    Step::Filter(_) => false,
                })
                .map_or(0, |i| i + 1);
            Component {
                steps: order.into_iter().map(|i| items[i]).collect(),
                vars: output_vars,
                outputs,
                body,
                slots: compiler.slots.len(),
                witness_from,
            }
        })
        .collect()
}

/// Turns body items into [`Step`]s, choosing their order on the way.
struct Compiler {
    /// Every variable of the rule, body and head.
    slots: BTreeMap<String, Slot>,
}

impl Compiler {
    fn new(rule: &Rule) -> Self {
        let mut names = BTreeSet::new();
        for item in &rule.body {
            collect_vars(item, &mut names);
        }
        for head in &rule.heads {
            collect_head_vars(head, &mut names);
        }
        Self {
            slots: names.into_iter().enumerate().map(|(i, v)| (v, i)).collect(),
        }
    }

    fn slot(&self, var: &str) -> Slot {
        self.slots[var]
    }

    /// Greedy, binding-consistent join order over `items`, compiled as it is chosen.
    ///
    /// Filters run as soon as their variables are bound — that is the whole point of reordering —
    /// and generators are picked cheapest-first given what is already bound. An indexed lookup
    /// (`fun(F, name = "append")`) beats a scan, and a scan of the smallest relation beats a scan
    /// of the biggest. The order is a heuristic; correctness rests only on never scheduling a
    /// filter before its variables exist, which the loop enforces.
    ///
    /// Returns the chosen order, as indices into `items`, and the steps in that order.
    fn plan(
        &self,
        items: &[&BodyItem],
        mut bound: BTreeSet<String>,
        visible: &BTreeSet<String>,
    ) -> (Vec<usize>, Vec<Step>) {
        let mut remaining: Vec<usize> = (0..items.len()).collect();
        let mut order = Vec::with_capacity(items.len());
        let mut steps = Vec::with_capacity(items.len());
        while !remaining.is_empty() {
            let mut best: Option<(usize, u32)> = None;
            for (pos, &i) in remaining.iter().enumerate() {
                let item = items[i];
                let cost = match item {
                    BodyItem::Atom(atom) if is_generator(item) => self.access(atom, &bound).1,
                    _ => {
                        if !required_vars(item, visible)
                            .iter()
                            .all(|v| bound.contains(v))
                        {
                            continue;
                        }
                        0
                    }
                };
                if best.is_none_or(|(_, c)| cost < c) {
                    best = Some((pos, cost));
                }
                if cost == 0 {
                    break;
                }
            }
            // Every remaining item is a filter whose variables are still unbound. At the top
            // level the mode check has already reported that; inside a negated group, which it
            // does not look into, the filter runs where it stands and fails on its unbound
            // operands.
            let pos = best.map_or(0, |(pos, _)| pos);
            let i = remaining.remove(pos);
            order.push(i);
            steps.push(self.step(items[i], &mut bound, visible));
        }
        (order, steps)
    }

    /// Compiles one item given what is bound before it, and adds what it binds to `bound`.
    fn step(
        &self,
        item: &BodyItem,
        bound: &mut BTreeSet<String>,
        visible: &BTreeSet<String>,
    ) -> Step {
        match item {
            BodyItem::Atom(atom) if is_generator(item) => Step::Atom(self.atom(atom, bound)),
            _ => Step::Filter(self.filter(item, bound, visible)),
        }
    }

    fn filter(
        &self,
        item: &BodyItem,
        bound: &BTreeSet<String>,
        visible: &BTreeSet<String>,
    ) -> Filter {
        match item {
            BodyItem::Atom(atom) if atom.name.starts_with('$') => {
                // A comparison, built by the parser: the operator's left operand is the column,
                // its right one the only attribute.
                let lhs = self.operand(&atom.columns[0]);
                let attr = &atom.attrs[0];
                match (&attr.rhs, attr.op) {
                    (Rhs::Set(items), CmpOp::In) => Filter::In(lhs, value_set(items)),
                    (Rhs::Set(_), _) | (Rhs::Term(_), CmpOp::In) => Filter::Never,
                    (Rhs::Term(t), op) => Filter::Cmp(lhs, op, self.operand(t)),
                }
            }
            BodyItem::Atom(atom) if atom.name == "regex_match" => {
                let pattern = match &atom.columns[1] {
                    // The checker rejected an invalid literal pattern.
                    Term::Lit(Literal::Str(p)) => match Regex::new(p) {
                        Ok(rx) => Pattern::Compiled(rx),
                        Err(_) => return Filter::Never,
                    },
                    Term::Var(v) => Pattern::Var(self.slot(v)),
                    _ => return Filter::Never,
                };
                Filter::Regex(self.operand(&atom.columns[0]), pattern)
            }
            // Whatever it would bind stays local to the test.
            BodyItem::Atom(atom) => Filter::Exists(Box::new(self.atom(atom, &mut bound.clone()))),
            BodyItem::Not(inner) => Filter::Not(Box::new(self.filter(inner, bound, visible))),
            BodyItem::Or(items) => Filter::Or(
                items
                    .iter()
                    .map(|i| self.filter(i, bound, visible))
                    .collect(),
            ),
            BodyItem::And(items) => {
                // Planned once, here, against the bindings it will see: the group's position in
                // the enclosing order is already fixed.
                let mut inner_visible = visible.clone();
                add_positive_vars(items, &mut inner_visible);
                let refs: Vec<&BodyItem> = items.iter().collect();
                Filter::Group(self.plan(&refs, bound.clone(), &inner_visible).1)
            }
        }
    }

    /// Compiles a relation atom. Each variable's first occurrence — columns left to right, then
    /// attributes — binds it, and is added to `bound`; every later occurrence, in this atom or
    /// after it, tests against that binding.
    fn atom(&self, atom: &Atom, bound: &mut BTreeSet<String>) -> AtomPlan {
        let (access, _) = self.access(atom, bound);
        let rel = Relation::of(&atom.name).expect("the checker accepted only built-in relations");
        let mut columns = Vec::with_capacity(atom.columns.len());
        for col in &atom.columns {
            columns.push(match col {
                Term::Var(v) if !bound.contains(v) => {
                    bound.insert(v.clone());
                    ColumnPat::Bind(self.slot(v))
                }
                t => ColumnPat::Test(self.operand(t)),
            });
        }
        let mut attrs = Vec::with_capacity(atom.attrs.len());
        for a in &atom.attrs {
            let attr = Attr::of(&a.name).expect("the checker accepted only honored attributes");
            let pat = match (&a.rhs, a.op) {
                (Rhs::Set(items), CmpOp::In) => AttrPat::In(value_set(items)),
                (Rhs::Set(_), _) | (Rhs::Term(_), CmpOp::In) => AttrPat::Never,
                (Rhs::Term(Term::Wildcard), CmpOp::Eq) => AttrPat::Present,
                (Rhs::Term(Term::Wildcard), _) => AttrPat::Never,
                (Rhs::Term(Term::Var(v)), op) if !bound.contains(v) => {
                    if op == CmpOp::Eq {
                        bound.insert(v.clone());
                        AttrPat::Bind(self.slot(v))
                    } else {
                        AttrPat::Never
                    }
                }
                (Rhs::Term(t), op) => AttrPat::Cmp(op, self.operand(t)),
            };
            attrs.push((attr, pat));
        }
        AtomPlan {
            rel,
            access,
            columns,
            attrs,
            probe: None,
        }
    }

    /// The cheapest way to find `atom`'s rows given `bound`, and its cost.
    ///
    /// A known value in an indexed column is a lookup; failing that, an equality (or a set) on an
    /// indexed attribute is; failing that, a scan.
    fn access(&self, atom: &Atom, bound: &BTreeSet<String>) -> (Access, u32) {
        let rel = Relation::of(&atom.name).expect("the checker accepted only built-in relations");
        let known = |t: &Term| match t {
            Term::Lit(_) => true,
            Term::Var(v) => bound.contains(v),
            Term::Wildcard => false,
        };
        let access = if let Some(&i) = rel
            .indexed_columns()
            .iter()
            .find(|&&i| known(&atom.columns[i]))
        {
            Access::Column(i)
        } else {
            let keyed = |a: &AttrConstraint| {
                Attr::of(&a.name).filter(|attr| rel.indexed_attrs().contains(attr))
            };
            let one = atom.attrs.iter().find_map(|a| match (&a.rhs, a.op) {
                (Rhs::Term(t), CmpOp::Eq) if known(t) => {
                    Some(Access::Attr(keyed(a)?, Key::One(self.operand(t))))
                }
                _ => None,
            });
            let set = || {
                atom.attrs.iter().find_map(|a| match (&a.rhs, a.op) {
                    (Rhs::Set(items), CmpOp::In) => {
                        let strs = items
                            .iter()
                            .filter_map(|l| match l {
                                Literal::Str(s) => Some(Str::from(s.as_str())),
                                _ => None,
                            })
                            .collect();
                        Some(Access::Attr(keyed(a)?, Key::Set(strs)))
                    }
                    _ => None,
                })
            };
            one.or_else(set).unwrap_or(Access::Scan)
        };
        let cost = access.cost(rel);
        (access, cost)
    }

    fn operand(&self, term: &Term) -> Operand {
        match term {
            Term::Var(v) => Operand::Var(self.slot(v)),
            Term::Lit(lit) => Operand::Const(literal_value(lit)),
            Term::Wildcard => Operand::Any,
        }
    }
}

fn literal_value(lit: &Literal) -> Value {
    match lit {
        Literal::Str(s) => Value::Str(Str::from(s.as_str())),
        Literal::Int(i) => Value::Int(*i),
        Literal::Bool(b) => Value::Bool(*b),
    }
}

fn value_set(items: &[Literal]) -> HashSet<Value> {
    items.iter().map(literal_value).collect()
}

/// The types the built-in relations' positional columns range over.
fn column_types(name: &str) -> &'static [VarType] {
    match name {
        "fun" => &[VarType::Function],
        "name_of" => &[VarType::Function, VarType::Str],
        "callsig" => &[VarType::CallSig],
        "param" => &[VarType::Function, VarType::Int],
        "local" => &[VarType::Function, VarType::Local],
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
        ("callsig", "parent") => VarType::Class,
        ("callsite", "callee_string") => VarType::Function,
        _ => VarType::Str,
    }
}

/// Every variable mentioned anywhere in an item, including inside attribute right-hand sides.
fn collect_vars(item: &BodyItem, out: &mut BTreeSet<String>) {
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
