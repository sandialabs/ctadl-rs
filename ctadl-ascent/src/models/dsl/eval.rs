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
exist, cheapest generator otherwise). This walks it with ordinary backtracking. Correctness does
not depend on the order — only on never running a filter before its variables are bound, which
the planner guarantees and the load-time mode check reports on.

# Negation

A negated atom is an existence check under the current bindings, which is why the mode check
requires its variables to be bound. A negated *group* — `!(fun(F, name = N) && regex_match(N,
"^get"))` — is an existence check over a whole subquery, and the variables local to it (`N`
here) are existentially quantified. That distinction is what lets "no name of F matches this
pattern" be expressed at all; without it the group would decompose into two independent tests
and mean something else entirely.
*/

use std::collections::{BTreeMap, BTreeSet};

use hashbrown::hash_map::HashMap;
use regex::Regex;

use crate::facts::Str;

use super::ast::*;
use super::check::{Plan, collect_vars, is_generator, required_vars};
use super::relations::{FunKey, ProgramFacts};

/// A value a variable can take.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Value {
    Str(Str),
    Int(i64),
    Bool(bool),
    /// A call site. Carries its caller and callee because a port anchored at a site is emitted
    /// as "the callee, at the sites of it inside this caller" — see `emit.rs`.
    Site {
        id: Str,
        caller: Str,
        callee: Str,
    },
}

impl Value {
    pub fn as_str(&self) -> Option<Str> {
        match self {
            Value::Str(s) => Some(*s),
            // A site's identity is its id; joining a site against a string compares that.
            Value::Site { id, .. } => Some(*id),
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
            Value::Site { id, .. } => write!(f, "{id}"),
        }
    }
}

/// One rule's accumulated solutions: per component, the set of variable tuples that satisfied
/// it, in that component's `vars` order.
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

    /// True when some component matched nothing anywhere, so the join is empty.
    pub fn is_dead(&self) -> bool {
        self.components.iter().any(|c| c.is_empty())
    }

    /// How many groundings the join produces.
    pub fn grounding_count(&self) -> usize {
        self.components
            .iter()
            .map(|c| c.len())
            .try_fold(1usize, |a, b| a.checked_mul(b))
            .unwrap_or(usize::MAX)
    }

    /// The join of every component: each grounding is a full variable binding for the rule.
    ///
    /// Components share no variables by construction, so this is a cross product and needs no
    /// equality check.
    pub fn groundings<'a>(&'a self, plan: &'a Plan) -> impl Iterator<Item = Binding> + 'a {
        let lists: Vec<Vec<&Vec<Value>>> =
            self.components.iter().map(|c| c.iter().collect()).collect();
        let sizes: Vec<usize> = lists.iter().map(|l| l.len()).collect();
        let total = if sizes.contains(&0) {
            0
        } else {
            sizes.iter().copied().product::<usize>()
        };
        let mut counters = vec![0usize; lists.len()];
        let mut emitted = 0usize;
        std::iter::from_fn(move || {
            if emitted >= total {
                return None;
            }
            let mut binding = Binding::default();
            for (ci, comp) in plan.components.iter().enumerate() {
                let row = lists[ci][counters[ci]];
                for (vi, var) in comp.vars.iter().enumerate() {
                    binding.0.insert(var.clone(), row[vi]);
                }
            }
            // Odometer.
            for ci in (0..counters.len()).rev() {
                counters[ci] += 1;
                if counters[ci] < sizes[ci] {
                    break;
                }
                counters[ci] = 0;
            }
            emitted += 1;
            Some(binding)
        })
    }
}

/// One complete grounding of a rule's variables.
#[derive(Clone, Debug, Default)]
pub struct Binding(pub BTreeMap<String, Value>);

impl Binding {
    pub fn get(&self, var: &str) -> Option<Value> {
        self.0.get(var).copied()
    }
}

/// Evaluates one rule against one program, folding what it finds into `solutions`.
pub fn evaluate_rule(
    rule: &Rule,
    plan: &Plan,
    facts: &ProgramFacts,
    solutions: &mut RuleSolutions,
) {
    let ev = Evaluator {
        facts,
        regexes: compile_regexes(rule),
    };
    for (ci, component) in plan.components.iter().enumerate() {
        let items: Vec<&BodyItem> = component.steps.iter().map(|&i| &rule.body[i]).collect();
        let mut env: Env = BTreeMap::new();
        let out = &mut solutions.components[ci];
        ev.solve(&items, 0, &mut env, &mut |env| {
            let row: Vec<Value> = component
                .vars
                .iter()
                .map(|v| env.get(v).copied().unwrap_or(Value::Int(0)))
                .collect();
            out.insert(row);
            true
        });
    }
}

/// Every literal `regex_match` pattern in a rule, compiled once.
///
/// A pattern given as a *variable* is compiled at use instead; that is the rare shape, and
/// caching it would need a mutable evaluator for no measurable gain.
fn compile_regexes(rule: &Rule) -> HashMap<String, Regex> {
    let mut out = HashMap::new();
    fn walk(item: &BodyItem, out: &mut HashMap<String, Regex>) {
        match item {
            BodyItem::Atom(atom) if atom.name == "regex_match" => {
                if let Some(Term::Lit(Literal::Str(p))) = atom.columns.get(1)
                    && let Ok(rx) = Regex::new(p)
                {
                    out.insert(p.clone(), rx);
                }
            }
            BodyItem::Atom(_) => {}
            BodyItem::Not(inner) => walk(inner, out),
            BodyItem::And(items) | BodyItem::Or(items) => {
                for i in items {
                    walk(i, out);
                }
            }
        }
    }
    for item in &rule.body {
        walk(item, &mut out);
    }
    out
}

struct Evaluator<'a> {
    facts: &'a ProgramFacts,
    regexes: HashMap<String, Regex>,
}

type Env = BTreeMap<String, Value>;
/// One way an atom can extend the current bindings.
type Extension = Vec<(String, Value)>;

impl<'a> Evaluator<'a> {
    /// Enumerates every solution of `items` from position `at`, calling `out` on each.
    ///
    /// `out` returns `false` to stop early, which is what makes an existence check cheap.
    /// `solve` propagates that: a `false` return means "the caller asked to stop".
    fn solve(
        &self,
        items: &[&BodyItem],
        at: usize,
        env: &mut Env,
        out: &mut dyn FnMut(&Env) -> bool,
    ) -> bool {
        if at == items.len() {
            return out(env);
        }
        let item = items[at];
        if !is_generator(item) {
            if self.satisfiable(item, env) {
                return self.solve(items, at + 1, env, out);
            }
            return true;
        }
        let BodyItem::Atom(atom) = item else {
            unreachable!("is_generator implies an atom")
        };
        // Materialized before recursing: the recursion mutates `env`, which the generator reads.
        let mut extensions: Vec<Extension> = Vec::new();
        self.generate(atom, env, &mut extensions);
        for ext in extensions {
            for (k, v) in &ext {
                env.insert(k.clone(), *v);
            }
            let keep_going = self.solve(items, at + 1, env, out);
            for (k, _) in &ext {
                env.remove(k);
            }
            if !keep_going {
                return false;
            }
        }
        true
    }

    // -----------------------------------------------------------------------
    // Generators
    // -----------------------------------------------------------------------

    fn generate(&self, atom: &Atom, env: &Env, out: &mut Vec<Extension>) {
        match atom.name.as_str() {
            "fun" => self.generate_fun(atom, env, out),
            "param" => self.generate_param(atom, env, out),
            "callsite" => self.generate_callsite(atom, env, out),
            "subclass" => self.generate_subclass(atom, env, false, false, out),
            "subclass*" => self.generate_subclass(atom, env, true, true, out),
            "subclass+" => self.generate_subclass(atom, env, false, true, out),
            "uses_field" => self.generate_uses_field(atom, env, out),
            other => unreachable!("'{other}' is not a generator"),
        }
    }

    fn generate_fun(&self, atom: &Atom, env: &Env, out: &mut Vec<Extension>) {
        let subject = &atom.columns[0];
        let candidates: Vec<usize> = match self.resolve(subject, env) {
            Some(Value::Str(fq)) => match self.facts.fun_index(fq) {
                Some(i) => vec![i],
                None => return,
            },
            Some(_) => return,
            // Unbound (or `_`): narrow by an indexed attribute if one is usable, else scan.
            None => match self.keyed_candidates(atom, env) {
                Some(list) => list,
                None => (0..self.facts.funs.len()).collect(),
            },
        };
        for i in candidates {
            let row = &self.facts.funs[i];
            let Some(mut alternatives) = self.fun_attr_options(atom, row, env) else {
                continue;
            };
            if let Term::Var(v) = subject
                && !env.contains_key(v)
            {
                for ext in &mut alternatives {
                    ext.push((v.clone(), Value::Str(row.fq)));
                }
            }
            out.append(&mut alternatives);
        }
    }

    /// Row indices reachable through an indexed attribute (`name`, `parent`, `signature`,
    /// `qualified-id`) with a value already known. `None` means "no usable index; scan".
    fn keyed_candidates(&self, atom: &Atom, env: &Env) -> Option<Vec<usize>> {
        for attr in &atom.attrs {
            let key = match attr.name.as_str() {
                "name" => FunKey::Name,
                "parent" => FunKey::Parent,
                "signature" => FunKey::Signature,
                "qualified-id" => FunKey::QualifiedId,
                _ => continue,
            };
            match (attr.op, &attr.rhs) {
                (CmpOp::Eq, Rhs::Term(t)) => {
                    if let Some(Value::Str(s)) = self.resolve(t, env) {
                        return Some(self.facts.funs_by(key, s).to_vec());
                    }
                }
                (CmpOp::In, Rhs::Set(items)) => {
                    let mut all: Vec<usize> = Vec::new();
                    for lit in items {
                        if let Literal::Str(s) = lit {
                            all.extend_from_slice(self.facts.funs_by(key, Str::from(s.as_str())));
                        }
                    }
                    all.sort_unstable();
                    all.dedup();
                    return Some(all);
                }
                _ => {}
            }
        }
        None
    }

    /// Every way this row can satisfy the atom's attributes, or `None` if it cannot.
    ///
    /// More than one way arises only when an attribute is an *output* on a function the frontend
    /// publishes under several spellings — `fun(F, name = N)` on a native symbol known both as
    /// `system` and as `<EXTERNAL>::system@00101008`. Binding one of them and dropping the other
    /// would silently narrow what a migrated `name` regex matches; see `relations.rs`.
    fn fun_attr_options(
        &self,
        atom: &Atom,
        row: &super::relations::FunRow,
        env: &Env,
    ) -> Option<Vec<Extension>> {
        let mut alternatives: Vec<Extension> = vec![Vec::new()];
        for attr in &atom.attrs {
            let options = match attr.name.as_str() {
                "name" => self.multi_attr(attr, &row.names, env),
                "parent" => self.multi_attr(attr, &row.parents, env),
                "signature" => self.multi_attr(attr, &row.signatures, env),
                "qualified-id" => self.multi_attr(attr, &row.qualified_ids, env),
                "language" => self.multi_attr(attr, opt_slice(&self.facts.language), env),
                "import" => self.multi_attr(attr, opt_slice(&self.facts.import), env),
                "arity" => match row.arity {
                    Some(a) => self.scalar_attr(attr, Value::Int(a), env),
                    None => None,
                },
                "has_code" => match row.has_code {
                    Some(b) => self.scalar_attr(attr, Value::Bool(b), env),
                    None => None,
                },
                _ => None,
            }?;
            alternatives = cross(alternatives, options)?;
        }
        Some(alternatives)
    }

    fn generate_param(&self, atom: &Atom, env: &Env, out: &mut Vec<Extension>) {
        let func = &atom.columns[0];
        let idx = &atom.columns[1];
        let rows: Vec<&super::relations::FunRow> = match self.resolve(func, env) {
            Some(Value::Str(fq)) => self.facts.fun_row(fq).into_iter().collect(),
            Some(_) => return,
            None => self.facts.funs.iter().collect(),
        };
        let want_index = self.resolve(idx, env).and_then(|v| v.as_int());
        for row in rows {
            // "populated wherever arity is known": a bodyless callee has none, so it
            // contributes no parameter rows at all.
            let Some(arity) = row.arity else { continue };
            for i in 0..arity {
                if let Some(w) = want_index
                    && w != i
                {
                    continue;
                }
                let mut ext = Vec::new();
                if let Term::Var(v) = func
                    && !env.contains_key(v)
                {
                    ext.push((v.clone(), Value::Str(row.fq)));
                }
                if let Term::Var(v) = idx
                    && !env.contains_key(v)
                {
                    ext.push((v.clone(), Value::Int(i)));
                }
                out.push(ext);
            }
        }
    }

    fn generate_callsite(&self, atom: &Atom, env: &Env, out: &mut Vec<Extension>) {
        let caller = &atom.columns[0];
        let site = &atom.columns[1];
        let bound_caller = match self.resolve(caller, env) {
            Some(Value::Str(f)) => Some(f),
            Some(_) => return,
            None => None,
        };
        let candidates: Vec<usize> = match self.resolve(site, env) {
            Some(v) => match v.as_str().and_then(|id| self.facts.callsite_index(id)) {
                Some(i) => vec![i],
                None => return,
            },
            None => match bound_caller {
                Some(f) => self.facts.callsites_of_caller(f).to_vec(),
                None => match self.callee_key(atom, env) {
                    Some(list) => list,
                    None => (0..self.facts.callsites.len()).collect(),
                },
            },
        };
        for i in candidates {
            let row = &self.facts.callsites[i];
            if let Some(f) = bound_caller
                && f != row.caller
            {
                continue;
            }
            let mut alternatives: Vec<Extension> = vec![Vec::new()];
            let mut ok = true;
            for attr in &atom.attrs {
                let options = if attr.name == "callee_string" {
                    self.scalar_attr(attr, Value::Str(row.callee), env)
                } else {
                    None
                };
                match options.and_then(|o| cross(alternatives.clone(), o)) {
                    Some(next) => alternatives = next,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            for ext in &mut alternatives {
                if let Term::Var(v) = caller
                    && !env.contains_key(v)
                {
                    ext.push((v.clone(), Value::Str(row.caller)));
                }
                if let Term::Var(v) = site
                    && !env.contains_key(v)
                {
                    ext.push((
                        v.clone(),
                        Value::Site {
                            id: row.id,
                            caller: row.caller,
                            callee: row.callee,
                        },
                    ));
                }
            }
            out.append(&mut alternatives);
        }
    }

    /// Call-site rows reachable through a known `callee_string`.
    fn callee_key(&self, atom: &Atom, env: &Env) -> Option<Vec<usize>> {
        for attr in &atom.attrs {
            if attr.name != "callee_string" {
                continue;
            }
            match (attr.op, &attr.rhs) {
                (CmpOp::Eq, Rhs::Term(t)) => {
                    if let Some(Value::Str(s)) = self.resolve(t, env) {
                        return Some(self.facts.callsites_of_callee(s).to_vec());
                    }
                }
                (CmpOp::In, Rhs::Set(items)) => {
                    let mut all = Vec::new();
                    for lit in items {
                        if let Literal::Str(s) = lit {
                            all.extend_from_slice(
                                self.facts.callsites_of_callee(Str::from(s.as_str())),
                            );
                        }
                    }
                    all.sort_unstable();
                    all.dedup();
                    return Some(all);
                }
                _ => {}
            }
        }
        None
    }

    fn generate_subclass(
        &self,
        atom: &Atom,
        env: &Env,
        reflexive: bool,
        deep: bool,
        out: &mut Vec<Extension>,
    ) {
        let sub = &atom.columns[0];
        let sup = &atom.columns[1];
        let subs: Vec<Str> = match self.resolve(sub, env) {
            Some(Value::Str(s)) => vec![s],
            Some(_) => return,
            None => self.facts.classes.clone(),
        };
        let want_super = match self.resolve(sup, env) {
            Some(Value::Str(s)) => Some(s),
            Some(_) => return,
            None => None,
        };
        for s in subs {
            for parent in self.facts.supers(s, reflexive, deep) {
                if let Some(w) = want_super
                    && w != parent
                {
                    continue;
                }
                let mut ext = Vec::new();
                if let Term::Var(v) = sub
                    && !env.contains_key(v)
                {
                    ext.push((v.clone(), Value::Str(s)));
                }
                if let Term::Var(v) = sup
                    && !env.contains_key(v)
                {
                    ext.push((v.clone(), Value::Str(parent)));
                }
                out.push(ext);
            }
        }
    }

    fn generate_uses_field(&self, atom: &Atom, env: &Env, out: &mut Vec<Extension>) {
        let func = &atom.columns[0];
        let field = &atom.columns[1];
        let candidates: Vec<usize> = match self.resolve(func, env) {
            Some(Value::Str(f)) => self.facts.uses_field_of(f).to_vec(),
            Some(_) => return,
            None => (0..self.facts.uses_field.len()).collect(),
        };
        let want_field = match self.resolve(field, env) {
            Some(Value::Str(s)) => Some(s),
            Some(_) => return,
            None => None,
        };
        for i in candidates {
            let (f, fld) = self.facts.uses_field[i];
            if let Some(w) = want_field
                && w != fld
            {
                continue;
            }
            let mut ext = Vec::new();
            if let Term::Var(v) = func
                && !env.contains_key(v)
            {
                ext.push((v.clone(), Value::Str(f)));
            }
            if let Term::Var(v) = field
                && !env.contains_key(v)
            {
                ext.push((v.clone(), Value::Str(fld)));
            }
            out.push(ext);
        }
    }

    // -----------------------------------------------------------------------
    // Filters
    // -----------------------------------------------------------------------

    /// Whether an item has at least one solution under `env`.
    ///
    /// This is where negation lives. `!x` is "no solution", and a group's local variables are
    /// existentially quantified by the subquery — see the module docs.
    fn satisfiable(&self, item: &BodyItem, env: &Env) -> bool {
        match item {
            BodyItem::Not(inner) => !self.satisfiable(inner, env),
            BodyItem::Or(items) => items.iter().any(|i| self.satisfiable(i, env)),
            BodyItem::And(items) => {
                let refs: Vec<&BodyItem> = items.iter().collect();
                let ordered = self.order_runtime(&refs, env);
                let mut e = env.clone();
                let mut found = false;
                self.solve(&ordered, 0, &mut e, &mut |_| {
                    found = true;
                    false
                });
                found
            }
            BodyItem::Atom(atom) if atom.name.starts_with('$') => self.holds_test(atom, env),
            BodyItem::Atom(atom) if atom.name == "regex_match" => self.holds_regex(atom, env),
            BodyItem::Atom(atom) => {
                let mut out = Vec::new();
                self.generate(atom, env, &mut out);
                !out.is_empty()
            }
        }
    }

    /// A binding-consistent order for a subquery, chosen at run time because the bindings a
    /// negated group sees depend on where it sits in the enclosing rule.
    fn order_runtime<'b>(&self, items: &[&'b BodyItem], env: &Env) -> Vec<&'b BodyItem> {
        let mut remaining: Vec<&'b BodyItem> = items.to_vec();
        let mut bound: BTreeSet<String> = env.keys().cloned().collect();
        let mut out = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let mut pick = None;
            for (pos, item) in remaining.iter().enumerate() {
                if !is_generator(item) {
                    let mut elsewhere = BTreeSet::new();
                    for (other, o) in remaining.iter().enumerate() {
                        if other != pos {
                            collect_vars(o, &mut elsewhere);
                        }
                    }
                    elsewhere.extend(bound.iter().cloned());
                    if !required_vars(item, &elsewhere)
                        .iter()
                        .all(|v| bound.contains(v))
                    {
                        continue;
                    }
                    pick = Some(pos);
                    break;
                }
                if pick.is_none() {
                    pick = Some(pos);
                }
            }
            let pos = pick.unwrap_or(0);
            let item = remaining.remove(pos);
            if is_generator(item) {
                collect_vars(item, &mut bound);
            }
            out.push(item);
        }
        out
    }

    fn holds_test(&self, atom: &Atom, env: &Env) -> bool {
        let Some(lhs) = self.resolve(&atom.columns[0], env) else {
            // `_ = X` is vacuously true: a wildcard matches anything.
            return true;
        };
        let attr = &atom.attrs[0];
        match (&attr.rhs, attr.op) {
            (Rhs::Set(items), CmpOp::In) => items.iter().any(|lit| lit_eq(&lhs, lit)),
            (Rhs::Set(_), _) => false,
            (Rhs::Term(t), op) => match self.resolve(t, env) {
                Some(rhs) => compare(&lhs, op, &rhs),
                // `X = _` holds whenever `X` is bound at all.
                None => op == CmpOp::Eq,
            },
        }
    }

    fn holds_regex(&self, atom: &Atom, env: &Env) -> bool {
        let Some(subject) = self.resolve(&atom.columns[0], env).and_then(|v| v.as_str()) else {
            return false;
        };
        let Some(pattern) = self.resolve(&atom.columns[1], env).and_then(|v| v.as_str()) else {
            return false;
        };
        if let Some(rx) = self.regexes.get(pattern.as_str()) {
            return rx.is_match(subject.as_str());
        }
        // A pattern that arrived through a variable. An invalid one matches nothing rather than
        // failing the load: the load could not have seen it.
        match Regex::new(pattern.as_str()) {
            Ok(rx) => rx.is_match(subject.as_str()),
            Err(_) => {
                log::warn!("regex_match: invalid pattern {pattern:?}; matching nothing");
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Shared attribute machinery
    // -----------------------------------------------------------------------

    /// An attribute with several accepted spellings. `None` means the row fails the constraint.
    fn multi_attr(
        &self,
        attr: &AttrConstraint,
        values: &[Str],
        env: &Env,
    ) -> Option<Vec<Extension>> {
        let pass = || Some(vec![Vec::new()]);
        match (&attr.rhs, attr.op) {
            (Rhs::Set(items), CmpOp::In) => {
                if values
                    .iter()
                    .any(|v| items.iter().any(|lit| lit_eq(&Value::Str(*v), lit)))
                {
                    pass()
                } else {
                    None
                }
            }
            (Rhs::Set(_), _) => None,
            (Rhs::Term(Term::Wildcard), CmpOp::Eq) => {
                if values.is_empty() {
                    None
                } else {
                    pass()
                }
            }
            (Rhs::Term(Term::Wildcard), _) => None,
            (Rhs::Term(t), op) => match self.resolve(t, env) {
                Some(rhs) => {
                    let ok = match op {
                        // `!=` over a multi-valued attribute is "no spelling equals it". Any
                        // other reading would make `name != "x"` true for a function that has
                        // `x` as one of its names.
                        CmpOp::Ne => values
                            .iter()
                            .all(|v| !compare(&Value::Str(*v), CmpOp::Eq, &rhs)),
                        _ => values.iter().any(|v| compare(&Value::Str(*v), op, &rhs)),
                    };
                    if ok { pass() } else { None }
                }
                None => {
                    // An unbound variable on the right of `=` is an output: one alternative per
                    // spelling.
                    if op != CmpOp::Eq {
                        return None;
                    }
                    let Term::Var(v) = t else { return None };
                    if values.is_empty() {
                        return None;
                    }
                    Some(
                        values
                            .iter()
                            .map(|value| vec![(v.clone(), Value::Str(*value))])
                            .collect(),
                    )
                }
            },
        }
    }

    /// A single-valued attribute (`arity`, `has_code`, `callee_string`).
    fn scalar_attr(
        &self,
        attr: &AttrConstraint,
        value: Value,
        env: &Env,
    ) -> Option<Vec<Extension>> {
        let pass = || Some(vec![Vec::new()]);
        match (&attr.rhs, attr.op) {
            (Rhs::Set(items), CmpOp::In) => {
                if items.iter().any(|lit| lit_eq(&value, lit)) {
                    pass()
                } else {
                    None
                }
            }
            (Rhs::Set(_), _) => None,
            (Rhs::Term(Term::Wildcard), CmpOp::Eq) => pass(),
            (Rhs::Term(Term::Wildcard), _) => None,
            (Rhs::Term(t), op) => match self.resolve(t, env) {
                Some(rhs) => {
                    if compare(&value, op, &rhs) {
                        pass()
                    } else {
                        None
                    }
                }
                None => {
                    if op != CmpOp::Eq {
                        return None;
                    }
                    let Term::Var(v) = t else { return None };
                    Some(vec![vec![(v.clone(), value)]])
                }
            },
        }
    }

    /// The value of a term under `env`, or `None` for an unbound variable or `_`.
    fn resolve(&self, term: &Term, env: &Env) -> Option<Value> {
        match term {
            Term::Wildcard => None,
            Term::Var(v) => env.get(v).copied(),
            Term::Lit(Literal::Str(s)) => Some(Value::Str(Str::from(s.as_str()))),
            Term::Lit(Literal::Int(i)) => Some(Value::Int(*i)),
            Term::Lit(Literal::Bool(b)) => Some(Value::Bool(*b)),
        }
    }
}

/// Combines two independent sets of alternative extensions, dropping any pair that binds one
/// variable two different ways. `None` when nothing survives.
fn cross(left: Vec<Extension>, right: Vec<Extension>) -> Option<Vec<Extension>> {
    let mut out = Vec::with_capacity(left.len() * right.len());
    for l in &left {
        'next: for r in &right {
            let mut merged = l.clone();
            for (k, v) in r {
                match merged.iter().find(|(mk, _)| mk == k) {
                    Some((_, existing)) if existing != v => continue 'next,
                    Some(_) => {}
                    None => merged.push((k.clone(), *v)),
                }
            }
            out.push(merged);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `Option<Str>` as a zero-or-one-element slice, so the per-import scope attributes go through
/// the same path as the per-function ones.
fn opt_slice(value: &Option<Str>) -> &[Str] {
    match value {
        Some(s) => std::slice::from_ref(s),
        None => &[],
    }
}

fn lit_eq(value: &Value, lit: &Literal) -> bool {
    match (value, lit) {
        (Value::Str(s), Literal::Str(l)) => s.as_str() == l,
        (Value::Site { id, .. }, Literal::Str(l)) => id.as_str() == l,
        (Value::Int(i), Literal::Int(l)) => i == l,
        (Value::Bool(b), Literal::Bool(l)) => b == l,
        _ => false,
    }
}

/// Comparison across the value domains. Strings compare lexicographically, numbers
/// numerically, booleans by equality only; a cross-domain comparison is false rather than an
/// error, which is what makes `fun(F, parent = P)` on a frontend with no classes match nothing
/// instead of failing the load.
fn compare(lhs: &Value, op: CmpOp, rhs: &Value) -> bool {
    let ord = match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
        (a, b) => match (a.as_str(), b.as_str()) {
            (Some(a), Some(b)) => a.as_str().partial_cmp(b.as_str()),
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
