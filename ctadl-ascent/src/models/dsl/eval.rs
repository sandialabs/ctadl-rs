/*! The evaluator.

The language has no recursion, so the fixpoint of a rule set is reached in one round: every
rule's body reads only built-in input relations, and no head feeds another body. A semi-naive
loop over such a program does exactly one iteration with the whole input as the delta, which is
what this is — one pass per rule, per component, per import.

# Why components, and why the join happens later

A rule's body is split into connected components by shared variables (see
[`super::check::Plan`]). Each component is evaluated **per import** and its solutions
accumulated across the whole import loop; the components are joined only after the loop ends.

That is not an optimization. A bridging rule names a callee in one program and its
implementation in another:

```text
S::bridge(arg(1).baz -> G::arg(0).stack[2]) :-
  callsite(_, S, callee_string = F),
  fun(F, name = "luaCallNativeAdd", language = "lua"),
  fun(G, name = "luaNativeAdd", language = "pcode");
```

No single import satisfies that body — the lua side and the pcode side are different programs,
and under streaming only one is ever in memory. The two components each match in their own
import, and the cross product after the loop is the pairing. A rule with one component, which is
nearly all of them, gets the same treatment and notices nothing: its solution set is the union
over imports.

# Join order

Within a component the planner has already chosen an order (filters as soon as their variables
exist, cheapest generator otherwise) and compiled each step against what is bound when it runs:
which index to look rows up by, which columns and attributes test and which bind. This walks
those steps with ordinary backtracking over a slot per variable, undoing bindings off a trail.
Correctness does not depend on the order — only on never running a filter before its variables
are bound, which the planner guarantees and the load-time mode check reports on.

Only head variables are kept (see [`super::check::Component::vars`]), so once the last step that
binds one has run, the rest of the body needs one witness rather than every solution.

# Negation

A negated atom is an existence check under the current bindings, which is why the mode check
requires its variables to be bound. A negated *group* — `!(fun(F, name = N) && regex_match(N,
"^get"))` — is an existence check over a whole subquery, and the variables local to it (`N`
here) are existentially quantified. That distinction is what lets "no name of F matches this
pattern" be expressed at all; without it the group would decompose into two independent tests
and mean something else entirely. Either kind stops at its first witness.
*/

use std::borrow::Cow;
use std::collections::BTreeSet;

use hashbrown::HashSet;
use regex::Regex;

use crate::facts::Str;

use super::ast::*;
use super::check::{
    Access, AtomPlan, Attr, AttrPat, ColumnPat, Filter, Key, Operand, Pattern, Plan, Relation,
    Slot, Step,
};
use super::relations::{FunKey, ProgramFacts, SiteId};

/// A value a variable can take.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Value {
    Str(Str),
    Int(i64),
    Bool(bool),
    /// A call site: its identity ([`SiteId`]'s fields) and the callee of the edge that bound
    /// it. Carries the callee because a port anchored at a site is emitted as "the callee, at
    /// the sites of it inside this caller" — see `emit.rs`.
    Site {
        caller: Str,
        block: u32,
        stmt: u32,
        callee: Str,
    },
    /// A static call signature. Carries its three components for the same reason a [`Self::Site`]
    /// carries its caller and callee: `emit` runs after the last import's facts are dropped, and
    /// a dispatch model is keyed on the triple rather than on the composed id.
    CallSig {
        id: Str,
        cls: Str,
        name: Str,
        desc: Str,
    },
}

impl Value {
    pub fn as_str(&self) -> Option<Str> {
        match self {
            Value::Str(s) => Some(*s),
            // A site's identity is a position, not an interned string; see [`text`].
            Value::CallSig { id, .. } => Some(*id),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{s}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Site {
                caller,
                block,
                stmt,
                ..
            } => write!(
                f,
                "{}",
                SiteId {
                    caller: *caller,
                    block: *block,
                    stmt: *stmt
                }
            ),
            Value::CallSig { id, .. } => write!(f, "{id}"),
        }
    }
}

/// One rule's accumulated solutions: per component, the set of variable tuples that satisfied
/// it, in that component's `vars` order.
///
/// A component whose `vars` is empty binds nothing a head reads. It is a *gate*: its set holds
/// the empty tuple once anything satisfied it, and nothing until then.
#[derive(Clone, Debug, Default)]
pub struct RuleSolutions {
    pub components: Vec<BTreeSet<Vec<Value>>>,
}

impl RuleSolutions {
    pub fn for_plan(plan: &Plan) -> Self {
        Self {
            components: vec![BTreeSet::new(); plan.components.len()],
        }
    }

    /// True when some component matched nothing anywhere, so the join is empty. A gate that
    /// never opened is such a component.
    pub fn is_dead(&self) -> bool {
        self.components.iter().any(|c| c.is_empty())
    }

    /// Forgets every solution, keeping the shape.
    pub fn clear(&mut self) {
        for c in &mut self.components {
            c.clear();
        }
    }

    /// How many groundings the join produces, saturating at `usize::MAX`. An open gate is a
    /// factor of one.
    pub fn grounding_count(&self) -> usize {
        self.components
            .iter()
            .map(|c| c.len())
            .try_fold(1usize, |a, b| a.checked_mul(b))
            .unwrap_or(usize::MAX)
    }

    /// The distinct values the join gives `columns`, each a `(component, column)` position, as
    /// tuples in `columns` order.
    ///
    /// Components share no variables by construction, so the join is a cross product, and only
    /// the components `columns` names take part in it: every other one is a gate, consulted
    /// for emptiness alone. Each one taking part is projected onto its columns and
    /// deduplicated first, which is what makes the product distinct without a seen-set — a
    /// variable the caller does not read must not multiply its tuples. Lazy: what is held is
    /// those projections and one counter per component, never the product.
    pub fn groundings<'a>(
        &'a self,
        columns: &[(usize, usize)],
    ) -> impl Iterator<Item = Vec<Value>> + 'a {
        // The components taking part, the columns read from each, and for each requested
        // column, which component's projection holds it and where.
        let mut parts: Vec<usize> = Vec::new();
        let mut part_columns: Vec<Vec<usize>> = Vec::new();
        let mut slots: Vec<(usize, usize)> = Vec::with_capacity(columns.len());
        for &(ci, col) in columns {
            let part = match parts.iter().position(|&c| c == ci) {
                Some(part) => part,
                None => {
                    parts.push(ci);
                    part_columns.push(Vec::new());
                    parts.len() - 1
                }
            };
            slots.push((part, part_columns[part].len()));
            part_columns[part].push(col);
        }
        let projections: Vec<Vec<Vec<Value>>> = parts
            .iter()
            .zip(&part_columns)
            .map(|(&ci, cols)| {
                let distinct: BTreeSet<Vec<Value>> = self.components[ci]
                    .iter()
                    .map(|row| cols.iter().map(|&col| row[col]).collect())
                    .collect();
                distinct.into_iter().collect()
            })
            .collect();
        let mut counters = vec![0usize; projections.len()];
        let mut done = self.is_dead();
        std::iter::from_fn(move || {
            if done {
                return None;
            }
            let tuple: Vec<Value> = slots
                .iter()
                .map(|&(part, at)| projections[part][counters[part]][at])
                .collect();
            // Odometer. It is exhausted when the most significant digit wraps, which with no
            // digits at all is immediately: the one empty tuple.
            done = true;
            for p in (0..counters.len()).rev() {
                counters[p] += 1;
                if counters[p] < projections[p].len() {
                    done = false;
                    break;
                }
                counters[p] = 0;
            }
            Some(tuple)
        })
    }
}

/// One grounding of the variables a head reads: `values[i]` is `vars[i]`'s.
#[derive(Clone, Copy, Debug)]
pub struct Binding<'a> {
    pub vars: &'a [String],
    pub values: &'a [Value],
}

impl Binding<'_> {
    pub fn get(&self, var: &str) -> Option<Value> {
        let i = self.vars.iter().position(|v| v == var)?;
        self.values.get(i).copied()
    }
}

/// Evaluates one rule against one program, folding what it finds into `solutions`.
///
/// Everything the body says is in `plan`; the rule itself is not consulted.
pub fn evaluate_rule(
    _rule: &Rule,
    plan: &Plan,
    facts: &ProgramFacts,
    solutions: &mut RuleSolutions,
) {
    for (ci, component) in plan.components.iter().enumerate() {
        let out = &mut solutions.components[ci];
        // A component no head reads is an existence check, and once one import has witnessed
        // it another cannot add anything.
        if component.vars.is_empty() && !out.is_empty() {
            continue;
        }
        let mut machine = Machine {
            facts,
            env: vec![None; component.slots],
            trail: Vec::new(),
        };
        machine.solve(
            &component.body,
            0,
            component.witness_from,
            &mut |m: &mut Machine<'_>| {
                let row: Vec<Value> = component
                    .outputs
                    .iter()
                    .map(|&s| m.env[s].expect("the planner binds every output"))
                    .collect();
                out.insert(row);
                true
            },
        );
    }
}

/// The evaluation state: one slot per rule variable, and the trail of slots bound since the
/// last choice point, which is what backtracking unwinds.
struct Machine<'a> {
    facts: &'a ProgramFacts,
    env: Vec<Option<Value>>,
    trail: Vec<Slot>,
}

/// What a step calls for each way it extends the bindings. Returning `false` stops the
/// enumeration, which is what makes an existence check stop at its first witness; every
/// enumerating function propagates that as its own `false` return.
type Cont<'c, 'a> = dyn FnMut(&mut Machine<'a>) -> bool + 'c;

/// One row of one relation, by position in [`ProgramFacts`].
#[derive(Copy, Clone)]
enum Row {
    Fun(usize),
    CallSig(usize),
    /// A function and one of its parameter indices.
    Param(usize, i64),
    CallSite(usize),
    /// `(sub, super)`.
    Class(Str, Str),
    UsesField(usize),
}

/// The values one row has for one attribute.
enum AttrValues<'a> {
    /// Every accepted spelling.
    Strs(&'a [Str]),
    One(Value),
    /// The row has no value: `arity` on a bodyless callee. Every constraint fails.
    Missing,
}

impl<'a> Machine<'a> {
    /// Enumerates every solution of `steps` from position `at`, calling `k` on each.
    ///
    /// From `witness_from` on, the remaining steps bind nothing `k` reads, so only whether they
    /// have a solution matters: `k` runs once if they do.
    fn solve(
        &mut self,
        steps: &[Step],
        at: usize,
        witness_from: usize,
        k: &mut Cont<'_, 'a>,
    ) -> bool {
        if at == steps.len() {
            return k(self);
        }
        if at == witness_from {
            return if self.exists(&steps[at..]) {
                k(self)
            } else {
                true
            };
        }
        match &steps[at] {
            Step::Filter(f) => !self.holds(f) || self.solve(steps, at + 1, witness_from, k),
            Step::Atom(atom) => self.match_atom(atom, &mut |m: &mut Machine<'a>| {
                m.solve(steps, at + 1, witness_from, k)
            }),
        }
    }

    /// Whether `steps` have a solution under the current bindings. Leaves them unchanged.
    fn exists(&mut self, steps: &[Step]) -> bool {
        let mut found = false;
        self.solve(steps, 0, usize::MAX, &mut |_: &mut Machine<'a>| {
            found = true;
            false
        });
        found
    }

    fn bind(&mut self, slot: Slot, value: Value) {
        self.env[slot] = Some(value);
        self.trail.push(slot);
    }

    fn undo(&mut self, mark: usize) {
        for slot in self.trail.drain(mark..) {
            self.env[slot] = None;
        }
    }

    /// An operand's value, or `None` for `_` (and for a variable nothing bound, which only a
    /// negated group's mis-ordered filter can see).
    fn value(&self, operand: &Operand) -> Option<Value> {
        match operand {
            Operand::Const(v) => Some(*v),
            Operand::Var(s) => self.env[*s],
            Operand::Any => None,
        }
    }

    // -----------------------------------------------------------------------
    // Generators
    // -----------------------------------------------------------------------

    /// Calls `k` once per way `atom` extends the bindings, with the extension bound.
    fn match_atom(&mut self, atom: &AtomPlan, k: &mut Cont<'_, 'a>) -> bool {
        let facts = self.facts;
        match atom.rel {
            Relation::Fun => match &atom.access {
                Access::Column(i) => {
                    match self.column_key(atom, *i).and_then(|fq| facts.fun_index(fq)) {
                        Some(r) => self.try_row(atom, Row::Fun(r), k),
                        None => true,
                    }
                }
                Access::Attr(attr, key) => {
                    let rows = self.keyed(key, |s| facts.funs_by(fun_key(*attr), s));
                    self.each(atom, rows.iter().map(|&r| Row::Fun(r)), k)
                }
                Access::Scan => self.each(atom, (0..facts.funs.len()).map(Row::Fun), k),
            },
            // A program collected without a dispatch universe has no rows, so such a rule
            // matches nothing rather than erroring — the gate that decides whether to collect
            // them lives in `cli`, and a rule that asks for one is what opens it.
            Relation::CallSig => match &atom.access {
                Access::Column(i) => match self
                    .column_key(atom, *i)
                    .and_then(|id| facts.callsig_index(id))
                {
                    Some(r) => self.try_row(atom, Row::CallSig(r), k),
                    None => true,
                },
                Access::Attr(attr, key) => {
                    let rows = self.keyed(key, |s| facts.callsigs_by(fun_key(*attr), s));
                    self.each(atom, rows.iter().map(|&r| Row::CallSig(r)), k)
                }
                Access::Scan => self.each(atom, (0..facts.callsigs.len()).map(Row::CallSig), k),
            },
            Relation::Param => self.generate_param(atom, k),
            Relation::CallSite => self.generate_callsite(atom, k),
            Relation::Subclass { reflexive, deep } => match &atom.access {
                Access::Column(0) => {
                    let Some(sub) = self.column_key(atom, 0) else {
                        return true;
                    };
                    let supers = facts.supers(sub, reflexive, deep);
                    self.each(atom, supers.into_iter().map(|sup| Row::Class(sub, sup)), k)
                }
                Access::Column(_) => {
                    let Some(sup) = self.column_key(atom, 1) else {
                        return true;
                    };
                    let subs = facts.subs(sup, reflexive, deep);
                    self.each(atom, subs.into_iter().map(|sub| Row::Class(sub, sup)), k)
                }
                // The direct relation is its edge list. A closure also has the reflexive pair of
                // every class, including one with no supertype, which the edges do not name.
                _ if !reflexive && !deep => self.each(
                    atom,
                    facts
                        .subclass
                        .iter()
                        .map(|&(sub, sup)| Row::Class(sub, sup)),
                    k,
                ),
                _ => {
                    for &sub in &facts.classes {
                        let supers = facts.supers(sub, reflexive, deep);
                        if !self.each(atom, supers.into_iter().map(|sup| Row::Class(sub, sup)), k) {
                            return false;
                        }
                    }
                    true
                }
            },
            Relation::UsesField => match &atom.access {
                Access::Column(i) => match self.column_key(atom, *i) {
                    Some(f) => self.each(
                        atom,
                        facts.uses_field_of(f).iter().map(|&r| Row::UsesField(r)),
                        k,
                    ),
                    None => true,
                },
                _ => self.each(atom, (0..facts.uses_field.len()).map(Row::UsesField), k),
            },
        }
    }

    fn generate_param(&mut self, atom: &AtomPlan, k: &mut Cont<'_, 'a>) -> bool {
        let facts = self.facts;
        let funs: Vec<usize> = match &atom.access {
            Access::Column(i) => self
                .column_key(atom, *i)
                .and_then(|fq| facts.fun_index(fq))
                .into_iter()
                .collect(),
            _ => (0..facts.funs.len()).collect(),
        };
        let want_index = match &atom.columns[1] {
            ColumnPat::Test(op) => self.value(op).and_then(|v| v.as_int()),
            ColumnPat::Bind(_) => None,
        };
        for f in funs {
            // "populated wherever arity is known": a bodyless callee has none, so it
            // contributes no parameter rows at all.
            let Some(arity) = facts.funs[f].arity else {
                continue;
            };
            let indices = match want_index {
                Some(w) if (0..arity).contains(&w) => w..w + 1,
                Some(_) => continue,
                None => 0..arity,
            };
            if !self.each(atom, indices.map(|i| Row::Param(f, i)), k) {
                return false;
            }
        }
        true
    }

    fn generate_callsite(&mut self, atom: &AtomPlan, k: &mut Cont<'_, 'a>) -> bool {
        let facts = self.facts;
        match &atom.access {
            Access::Column(1) => match self.site_key(atom) {
                Some(site) => self.each(
                    atom,
                    facts
                        .callsites_with_id(site)
                        .iter()
                        .map(|&r| Row::CallSite(r)),
                    k,
                ),
                None => true,
            },
            Access::Column(i) => match self.column_key(atom, *i) {
                Some(caller) => self.each(
                    atom,
                    facts
                        .callsites_of_caller(caller)
                        .iter()
                        .map(|&r| Row::CallSite(r)),
                    k,
                ),
                None => true,
            },
            Access::Attr(_, key) => {
                let rows = self.keyed(key, |s| facts.callsites_of_callee(s));
                self.each(atom, rows.iter().map(|&r| Row::CallSite(r)), k)
            }
            Access::Scan => self.each(atom, (0..facts.callsites.len()).map(Row::CallSite), k),
        }
    }

    /// The site a bound `callsite` site column names: a site value, or a literal spelling one.
    fn site_key(&self, atom: &AtomPlan) -> Option<SiteId> {
        let ColumnPat::Test(op) = &atom.columns[1] else {
            return None;
        };
        match self.value(op)? {
            Value::Site {
                caller,
                block,
                stmt,
                ..
            } => Some(SiteId {
                caller,
                block,
                stmt,
            }),
            Value::Str(s) => SiteId::parse(s.as_str()),
            _ => None,
        }
    }

    /// The key an [`Access::Column`] looks up by.
    fn column_key(&self, atom: &AtomPlan, column: usize) -> Option<Str> {
        match &atom.columns[column] {
            ColumnPat::Test(op) => self.value(op).and_then(|v| v.as_str()),
            ColumnPat::Bind(_) => None,
        }
    }

    /// The rows an attribute index reaches for `key`. A set is the union of its members'
    /// lookups, deduplicated so a row two members reach is visited once.
    fn keyed(&self, key: &Key, lookup: impl Fn(Str) -> &'a [usize]) -> Cow<'a, [usize]> {
        match key {
            Key::One(op) => match self.value(op).and_then(|v| v.as_str()) {
                Some(s) => Cow::Borrowed(lookup(s)),
                None => Cow::Borrowed(&[]),
            },
            Key::Set(members) => {
                let mut all: Vec<usize> = members
                    .iter()
                    .flat_map(|&s| lookup(s).iter().copied())
                    .collect();
                all.sort_unstable();
                all.dedup();
                Cow::Owned(all)
            }
        }
    }

    fn each(
        &mut self,
        atom: &AtomPlan,
        rows: impl IntoIterator<Item = Row>,
        k: &mut Cont<'_, 'a>,
    ) -> bool {
        rows.into_iter().all(|row| self.try_row(atom, row, k))
    }

    /// Matches one candidate row against the atom's columns and attributes, calling `k` for
    /// each way it does. The bindings are as they were on return.
    fn try_row(&mut self, atom: &AtomPlan, row: Row, k: &mut Cont<'_, 'a>) -> bool {
        let mark = self.trail.len();
        let mut ok = true;
        for (i, pat) in atom.columns.iter().enumerate() {
            match pat {
                ColumnPat::Test(Operand::Any) => {}
                ColumnPat::Bind(s) => {
                    let v = self.column_value(row, i);
                    self.bind(*s, v);
                }
                ColumnPat::Test(op) => {
                    if let Some(want) = self.value(op)
                        && !compare(&self.column_value(row, i), CmpOp::Eq, &want)
                    {
                        ok = false;
                        break;
                    }
                }
            }
        }
        let keep_going = !ok || self.match_attrs(atom, row, 0, k);
        self.undo(mark);
        keep_going
    }

    /// Matches attributes `at..` of `atom` against `row`.
    ///
    /// More than one way arises only when an attribute is an *output* on a function the frontend
    /// publishes under several spellings — `fun(F, name = N)` on a native symbol known both as
    /// `system` and as `<EXTERNAL>::system@00101008`. Binding one of them and dropping the other
    /// would silently narrow what a migrated `name` regex matches; see `relations.rs`.
    fn match_attrs(&mut self, atom: &AtomPlan, row: Row, at: usize, k: &mut Cont<'_, 'a>) -> bool {
        let Some((attr, pat)) = atom.attrs.get(at) else {
            return k(self);
        };
        let values = self.attr_values(row, *attr);
        let AttrPat::Bind(slot) = pat else {
            return !self.attr_holds(pat, &values) || self.match_attrs(atom, row, at + 1, k);
        };
        let mut alternative = |m: &mut Self, v: Value| {
            let mark = m.trail.len();
            m.bind(*slot, v);
            let keep_going = m.match_attrs(atom, row, at + 1, k);
            m.undo(mark);
            keep_going
        };
        match values {
            AttrValues::Strs(list) => list.iter().all(|&s| alternative(self, Value::Str(s))),
            AttrValues::One(v) => alternative(self, v),
            AttrValues::Missing => true,
        }
    }

    fn column_value(&self, row: Row, column: usize) -> Value {
        let facts = self.facts;
        match (row, column) {
            (Row::Fun(r), _) => Value::Str(facts.funs[r].fq),
            (Row::CallSig(r), _) => {
                let row = &facts.callsigs[r];
                Value::CallSig {
                    id: row.id,
                    cls: row.parent,
                    name: row.name,
                    desc: row.signature,
                }
            }
            (Row::Param(f, _), 0) => Value::Str(facts.funs[f].fq),
            (Row::Param(_, i), _) => Value::Int(i),
            (Row::CallSite(r), 0) => Value::Str(facts.callsites[r].caller),
            (Row::CallSite(r), _) => {
                let row = &facts.callsites[r];
                Value::Site {
                    caller: row.caller,
                    block: row.block,
                    stmt: row.stmt,
                    callee: row.callee,
                }
            }
            (Row::Class(sub, _), 0) => Value::Str(sub),
            (Row::Class(_, sup), _) => Value::Str(sup),
            (Row::UsesField(r), 0) => Value::Str(facts.uses_field[r].0),
            (Row::UsesField(r), _) => Value::Str(facts.uses_field[r].1),
        }
    }

    /// A call-signature key has exactly one spelling of each attribute — it is composed from the
    /// site's three components — so each is a one-element slice and the multi-spelling case
    /// [`Self::match_attrs`] describes cannot arise.
    fn attr_values(&self, row: Row, attr: Attr) -> AttrValues<'a> {
        let facts = self.facts;
        match row {
            Row::Fun(r) => {
                let row = &facts.funs[r];
                match attr {
                    Attr::Name => AttrValues::Strs(&row.names),
                    Attr::Parent => AttrValues::Strs(&row.parents),
                    Attr::Signature => AttrValues::Strs(&row.signatures),
                    Attr::QualifiedId => AttrValues::Strs(&row.qualified_ids),
                    // The per-import scope attributes go through the same path as the
                    // per-function ones, as a zero- or one-element slice.
                    Attr::Language => AttrValues::Strs(facts.language.as_slice()),
                    Attr::Import => AttrValues::Strs(facts.import.as_slice()),
                    Attr::Arity => row
                        .arity
                        .map_or(AttrValues::Missing, |a| AttrValues::One(Value::Int(a))),
                    Attr::HasCode => row
                        .has_code
                        .map_or(AttrValues::Missing, |b| AttrValues::One(Value::Bool(b))),
                    Attr::CalleeString => AttrValues::Missing,
                }
            }
            Row::CallSig(r) => {
                let row = &facts.callsigs[r];
                match attr {
                    Attr::Name => AttrValues::Strs(std::slice::from_ref(&row.name)),
                    Attr::Parent => AttrValues::Strs(std::slice::from_ref(&row.parent)),
                    Attr::Signature => AttrValues::Strs(std::slice::from_ref(&row.signature)),
                    Attr::QualifiedId => AttrValues::Strs(std::slice::from_ref(&row.id)),
                    _ => AttrValues::Missing,
                }
            }
            Row::CallSite(r) => match attr {
                Attr::CalleeString => {
                    AttrValues::Strs(std::slice::from_ref(&facts.callsites[r].callee))
                }
                _ => AttrValues::Missing,
            },
            Row::Param(..) | Row::Class(..) | Row::UsesField(_) => AttrValues::Missing,
        }
    }

    /// Whether a non-binding attribute constraint holds.
    fn attr_holds(&self, pat: &AttrPat, values: &AttrValues<'_>) -> bool {
        match (pat, values) {
            (_, AttrValues::Missing) | (AttrPat::Never, _) => false,
            (AttrPat::Bind(_), _) => unreachable!("a binding attribute enumerates"),
            (AttrPat::Present, AttrValues::Strs(list)) => !list.is_empty(),
            (AttrPat::Present, AttrValues::One(_)) => true,
            (AttrPat::In(set), AttrValues::Strs(list)) => {
                list.iter().any(|&s| set.contains(&Value::Str(s)))
            }
            (AttrPat::In(set), AttrValues::One(v)) => in_set(v, set),
            (AttrPat::Cmp(op, rhs), values) => {
                let Some(rhs) = self.value(rhs) else {
                    return false;
                };
                match values {
                    // `!=` over a multi-valued attribute is "no spelling equals it". Any other
                    // reading would make `name != "x"` true for a function that has `x` as one
                    // of its names.
                    AttrValues::Strs(list) if *op == CmpOp::Ne => list
                        .iter()
                        .all(|&s| !compare(&Value::Str(s), CmpOp::Eq, &rhs)),
                    AttrValues::Strs(list) => {
                        list.iter().any(|&s| compare(&Value::Str(s), *op, &rhs))
                    }
                    AttrValues::One(v) => compare(v, *op, &rhs),
                    AttrValues::Missing => false,
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Filters
    // -----------------------------------------------------------------------

    /// Whether a filter holds under the current bindings.
    ///
    /// This is where negation lives. `!x` is "no solution", and a group's local variables are
    /// existentially quantified by the subquery — see the module docs.
    fn holds(&mut self, filter: &Filter) -> bool {
        match filter {
            Filter::Exists(atom) => {
                let mut found = false;
                self.match_atom(atom, &mut |_: &mut Machine<'a>| {
                    found = true;
                    false
                });
                found
            }
            Filter::Group(steps) => self.exists(steps),
            Filter::Not(inner) => !self.holds(inner),
            Filter::Or(items) => items.iter().any(|f| self.holds(f)),
            Filter::Cmp(lhs, op, rhs) => match (self.value(lhs), self.value(rhs)) {
                // `_ = X` is vacuously true: a wildcard matches anything.
                (None, _) => true,
                (Some(lhs), Some(rhs)) => compare(&lhs, *op, &rhs),
                // `X = _` holds whenever `X` is bound at all.
                (Some(_), None) => *op == CmpOp::Eq,
            },
            Filter::In(lhs, set) => self.value(lhs).is_none_or(|v| in_set(&v, set)),
            Filter::Regex(subject, pattern) => self.holds_regex(subject, pattern),
            Filter::Never => false,
        }
    }

    fn holds_regex(&self, subject: &Operand, pattern: &Pattern) -> bool {
        let Some(subject) = self.value(subject).and_then(|v| text(&v)) else {
            return false;
        };
        let pattern = match pattern {
            Pattern::Compiled(rx) => return rx.is_match(&subject),
            Pattern::Var(slot) => match self.env[*slot].and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return false,
            },
        };
        // A pattern that arrived through a variable. An invalid one matches nothing rather than
        // failing the load: the load could not have seen it.
        match Regex::new(pattern.as_str()) {
            Ok(rx) => rx.is_match(&subject),
            Err(_) => {
                log::warn!("regex_match: invalid pattern {pattern:?}; matching nothing");
                false
            }
        }
    }
}

/// The [`FunKey`] an indexed string attribute looks up by.
fn fun_key(attr: Attr) -> FunKey {
    match attr {
        Attr::Name => FunKey::Name,
        Attr::Parent => FunKey::Parent,
        Attr::Signature => FunKey::Signature,
        Attr::QualifiedId => FunKey::QualifiedId,
        other => unreachable!("{other:?} is not an indexed attribute"),
    }
}

/// Membership in an `in {...}` set of literals. A site or a signature key is a member when its
/// id is, as it is equal to a string when its id is.
fn in_set(value: &Value, set: &HashSet<Value>) -> bool {
    match value {
        Value::Int(_) | Value::Bool(_) | Value::Str(_) => set.contains(value),
        // A site's spelling is never interned, so a member naming it is one already interned.
        Value::Site { .. } => {
            Str::try_get(&value.to_string()).is_some_and(|s| set.contains(&Value::Str(s)))
        }
        other => other.as_str().is_some_and(|s| set.contains(&Value::Str(s))),
    }
}

/// `value` as text, for a string comparison. A site has no interned spelling, so it is printed:
/// interning `caller#block:statement` per site would leak one string per call site of every
/// import.
fn text(value: &Value) -> Option<Cow<'static, str>> {
    match value {
        Value::Site { .. } => Some(Cow::Owned(value.to_string())),
        v => v.as_str().map(|s| Cow::Borrowed(s.as_str())),
    }
}

/// Comparison across the value domains. Strings compare lexicographically, numbers
/// numerically, booleans by equality only; a cross-domain comparison is false rather than an
/// error, which is what makes `fun(F, parent = P)` on a frontend with no classes match nothing
/// instead of failing the load.
fn compare(lhs: &Value, op: CmpOp, rhs: &Value) -> bool {
    let ord = match (lhs, rhs) {
        // Interned, so equality is the handle's. Only an ordering needs the text.
        (Value::Str(a), Value::Str(b)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
            return (a == b) == (op == CmpOp::Eq);
        }
        (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
        (a, b) => match (text(a), text(b)) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => None,
        },
    };
    let Some(ord) = ord else {
        return op == CmpOp::Ne;
    };
    use std::cmp::Ordering::*;
    match op {
        CmpOp::Eq => ord == Equal,
        CmpOp::Ne => ord != Equal,
        CmpOp::Lt => ord == Less,
        CmpOp::Le => ord != Greater,
        CmpOp::Gt => ord == Greater,
        CmpOp::Ge => ord != Less,
        CmpOp::In => false,
    }
}
