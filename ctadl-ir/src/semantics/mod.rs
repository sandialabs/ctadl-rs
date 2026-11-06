/*!
The reference implementation of the IR's semantics (`docs/ir.md`, S1–S17 of `docs/ir-redesign.md`).

This is a direct, unoptimized evaluation of the rules, written to be read beside them. It is the
oracle the IR passes are checked against (R12) and the engines are measured against (R36). It
is never run on a real artifact: it enumerates instances of every call, and every labeled cell
of every instance.

# What it computes

The least labeling closed under the rules, over the program's unfolding:

- **Paths** (S1–S3). Labels sit only at locations. An address holds none of its own; a read of
  one reads its base (`labels(p.[k]) = labels(p)`). A void path holds and propagates nothing.
- **Assignment** (S5–S9). `y.d = x.s` is a store when `d` is non-empty, else a copy. Forward,
  every suffix `t` of `x.s` carries its labels to `y.d.t`, with scope `All` for a store and
  `Fixed` for a copy. Backward, labels of scope `All` at `y.d.t` reach `x.s.t` for every
  non-empty `t` other than `#value`.
- **Values.** `y = value(p)` is the store `y.#value = p`.
- **Calls** (S10, S11). A call to a function with a body is an *instance* of it: the copies
  `p_i = a_i` into fresh formals, the body, and the copies `r_j = ret_j` back. A target label
  (`ptr<f>`) at an indirect call's callee path, or at its `#value`, makes it a call of `f`, so
  instances appear as labels do. A call to a function without a body, or with no target, is
  zero assignments and is reported in [`Outcome::unresolved`].
- **Globals** (S13). `$globals` is one variable shared by every instance.
- **Ports** (S17). A [`Ports::sources`] call labels its first result `r` and `r.#value` with the
  label its first argument names, scope `All`. A [`Ports::sinks`] call observes exactly its
  first argument and that argument's `#value`.

Every function is a root of the unfolding, and the observable is the union over instances.

# Bounds

Two explicit inputs keep the evaluation finite, and each is an approximation of the semantics
in the direction that loses flows: [`Config::path_bound`] drops labels at paths longer than it,
and [`Config::depth`] drops calls nested deeper than it. [`Outcome::truncated`] says whether
either bound was reached, so a test can tell "not derived" from "not explored".

# Input

The program must be in SSA form (S16): SSA is what makes a variable one cell. [`ssa_form`]
puts a program in it. Phi nodes are the copies of their operands.
*/
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::index::idx::Idx;
use crate::mir::path::{self, PathClass};
use crate::mir::{
    AccessPath, BasicBlockIdx, CallEdges, CallObject, CallStyle, Exp, FunctionData, FunctionIdx,
    PathSegment, Program, SourceInfo, StatementKind, TerminatorKind, Variable, VariableRef,
};

#[cfg(test)]
mod tests;

/// A label (S4, S11, S12).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Label {
    /// A taint label introduced by a source.
    Taint(String),
    /// A function reference: a call target.
    Target(String),
    /// A class reference from an allocation.
    Class(String),
    /// A constant string.
    Str(String),
}

/// Whether a label may move backward along assignments (S4, S7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// Arrived by a copy: forward only.
    Fixed,
    /// Placed by a store or a source: also backward.
    All,
}

/// The two bounds that keep the evaluation finite. See the module documentation.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The longest path, in segments, a label is kept at.
    pub path_bound: usize,
    /// The deepest call nesting instantiated below a root.
    pub depth: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            path_bound: 6,
            depth: 5,
        }
    }
}

/// The sources and sinks (S17): the initial labeling and the observation points.
#[derive(Clone, Debug, Default)]
pub struct Ports {
    /// A direct call to one of these labels its first result with the label its first
    /// argument names (a string constant), or with the callee's name.
    pub sources: BTreeSet<String>,
    /// A direct call to one of these observes its first argument.
    pub sinks: BTreeSet<String>,
    /// Labels placed at a path of a function, in every instance of it.
    pub seeds: Vec<Seed>,
    /// Paths of a function observed in every instance of it.
    pub observers: Vec<Observer>,
}

/// A source at an access path: `label` at `path` and `path.#value`, scope `All`.
#[derive(Clone, Debug)]
pub struct Seed {
    pub function: String,
    pub path: AccessPath,
    pub label: String,
}

/// A sink at an access path: it observes `path` and `path.#value`.
#[derive(Clone, Debug)]
pub struct Observer {
    pub function: String,
    pub path: AccessPath,
    /// What the observation calls the sink.
    pub name: String,
}

/// One observation: a taint label at a sink's port.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Observation {
    /// The function the sink is in.
    pub function: String,
    /// The sink: an [`Observer::name`], or `callee(label)@span` for a [`Ports::sinks`] call,
    /// `label` its second argument when that is a string and `span` the call's source span,
    /// which, unlike its position, survives the IR passes.
    pub site: String,
    /// The taint label observed.
    pub label: String,
}

impl fmt::Display for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} <- {}", self.function, self.site, self.label)
    }
}

/// What [`evaluate`] found.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// The observable (S17): every taint label at every sink port, over all instances.
    pub observed: BTreeSet<Observation>,
    /// Calls that are zero assignments: no body, or no target (S10). Each is
    /// `function@block:statement: why`.
    pub unresolved: BTreeSet<String>,
    /// Whether a bound of [`Config`] cut the evaluation short.
    pub truncated: bool,
}

/// Puts every function of `program` in SSA form, pruning unreachable blocks first. A function
/// already in SSA form is left as it is.
pub fn ssa_form(program: &Program) -> Program {
    let mut out = program.clone();
    for (_, f) in out.functions.iter_enumerated_mut() {
        if !f.blocks.is_empty() && !is_ssa(f) {
            crate::ssa::transform(f, true);
        }
    }
    out
}

/// Whether `f` has been through SSA conversion: its parameters and globals are bound at version
/// 0 by the entry block.
fn is_ssa(f: &FunctionData) -> bool {
    f.blocks.iter_enumerated().any(|(_, b)| {
        b.statements.iter().any(|s| {
            s.iter_dst_var().any(|v| v.version.is_some())
                || s.iter_src_var().any(|v| v.version.is_some())
        })
    })
}

/// Evaluates `program`, which must be in SSA form, to its least labeling. See the module
/// documentation.
pub fn evaluate(program: &Program, ports: &Ports, config: &Config) -> Outcome {
    let mut eval = Eval::new(program, ports, config);
    eval.run();
    eval.outcome
}

/// Who a variable belongs to: an instance, or every instance (`$globals`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Owner {
    Instance(usize),
    Shared,
}

/// A variable of one instance.
type Var = (Owner, VariableRef);

/// The labels at one location.
type Labels = BTreeMap<Label, Scope>;

/// One instance of a function in the unfolding.
struct Instance {
    function: FunctionIdx,
    depth: usize,
    /// The instance each call site called, by site and callee.
    children: HashMap<(BasicBlockIdx, usize, FunctionIdx), usize>,
}

struct Eval<'a> {
    program: &'a Program,
    ports: &'a Ports,
    config: &'a Config,
    by_name: HashMap<&'a str, FunctionIdx>,
    instances: Vec<Instance>,
    cells: HashMap<Var, BTreeMap<Vec<PathSegment>, Labels>>,
    outcome: Outcome,
    changed: bool,
}

impl<'a> Eval<'a> {
    fn new(program: &'a Program, ports: &'a Ports, config: &'a Config) -> Self {
        let by_name = program
            .functions
            .iter_enumerated()
            .filter(|(_, f)| !f.blocks.is_empty())
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();
        let instances = program
            .functions
            .iter_enumerated()
            .filter(|(_, f)| !f.blocks.is_empty())
            .map(|(function, _)| Instance {
                function,
                depth: 0,
                children: HashMap::new(),
            })
            .collect();
        Eval {
            program,
            ports,
            config,
            by_name,
            instances,
            cells: HashMap::new(),
            outcome: Outcome::default(),
            changed: false,
        }
    }

    fn run(&mut self) {
        loop {
            self.changed = false;
            let mut i = 0;
            while i < self.instances.len() {
                self.step(i);
                i += 1;
            }
            if !self.changed {
                break;
            }
        }
    }

    /// The variable `v` names in instance `inst`. Unversioned `$globals` is the one shared
    /// variable; SSA's `$globals_0 = $globals` copies it into each instance.
    fn var(inst: usize, v: &VariableRef) -> Var {
        if v.variable == Variable::GlobalHeap && v.version.is_none() {
            (Owner::Shared, v.clone())
        } else {
            (Owner::Instance(inst), v.clone())
        }
    }

    /// Adds `labels` at `var.p`, which must be a location within the path bound, each with at
    /// least `scope`.
    fn add<'l>(
        &mut self,
        var: &Var,
        p: &[PathSegment],
        labels: impl IntoIterator<Item = (&'l Label, Scope)>,
    ) {
        if path::classify(p) != PathClass::Location {
            return;
        }
        if p.len() > self.config.path_bound {
            self.outcome.truncated = true;
            return;
        }
        let cell = self
            .cells
            .entry(var.clone())
            .or_default()
            .entry(p.to_vec())
            .or_default();
        for (label, scope) in labels {
            match cell.get_mut(label) {
                Some(s) if *s >= scope => {}
                Some(s) => {
                    *s = scope;
                    self.changed = true;
                }
                None => {
                    cell.insert(label.clone(), scope);
                    self.changed = true;
                }
            }
        }
    }

    /// The labels at `var.p` for a read: an address reads its base (S3).
    fn read(&self, var: &Var, p: &[PathSegment]) -> Labels {
        let p = path::location_of(p);
        self.cells
            .get(var)
            .and_then(|cells| cells.get(p))
            .cloned()
            .unwrap_or_default()
    }

    /// The assignment `dst.d = src.s` (S5–S8). `store` gives forward labels scope `All`.
    fn assign(&mut self, dst: &Var, d: &[PathSegment], src: &Var, s: &[PathSegment], store: bool) {
        if path::classify(d) == PathClass::Void || path::classify(s) == PathClass::Void {
            return;
        }
        let fwd_scope = if store { Scope::All } else { Scope::Fixed };
        // Forward (S6): every suffix `t` with `src.s.t` a labeled location.
        let src_cells: Vec<(Vec<PathSegment>, Labels)> = self
            .cells
            .get(src)
            .map(|c| c.iter().map(|(p, l)| (p.clone(), l.clone())).collect())
            .unwrap_or_default();
        for (p, labels) in &src_cells {
            let mut targets = Vec::new();
            if let Some(t) = path::strip_prefix(p, s) {
                targets.push(path::concat(d, &t));
                // `s` ends in `#value`, so it absorbs `t = #value` too.
                if t.is_empty() && s.last() == Some(&PathSegment::Value) {
                    targets.push(path::concat(d, &[PathSegment::Value]));
                }
            }
            // The empty suffix of an address reads its base (S3).
            if path::classify(s) == PathClass::Address && path::location_of(s) == p.as_slice() {
                targets.push(d.to_vec());
            }
            for target in targets {
                self.add(dst, &target, labels.keys().map(|l| (l, fwd_scope)));
            }
        }
        // Backward (S8): labels of scope `All` at `dst.d.t`, `t` non-empty and not `#value`.
        let dst_cells: Vec<(Vec<PathSegment>, Vec<Label>)> = self
            .cells
            .get(dst)
            .map(|c| {
                c.iter()
                    .map(|(p, l)| {
                        let all = l
                            .iter()
                            .filter(|(_, s)| **s == Scope::All)
                            .map(|(l, _)| l.clone())
                            .collect();
                        (p.clone(), all)
                    })
                    .filter(|(_, l): &(_, Vec<Label>)| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        for (q, labels) in &dst_cells {
            let Some(t) = path::strip_prefix(q, d) else {
                continue;
            };
            if t.is_empty() || t == [PathSegment::Value] {
                continue;
            }
            let target = path::concat(s, &t);
            self.add(src, &target, labels.iter().map(|l| (l, Scope::All)));
        }
    }

    /// `dst.d = e`, with `e` read in instance `src_inst`.
    fn assign_exp(&mut self, dst: &Var, d: &[PathSegment], src_inst: usize, e: &Exp) {
        let store = !d.is_empty();
        let scope = if store { Scope::All } else { Scope::Fixed };
        match e {
            Exp::Path(ap) => {
                self.assign(dst, d, &Self::var(src_inst, &ap.base), &ap.segments, store)
            }
            // `y = value(p)` is the store `y.#value = p`.
            Exp::Value(ap) => {
                let d = path::concat(d, &[PathSegment::Value]);
                self.assign(dst, &d, &Self::var(src_inst, &ap.base), &ap.segments, true)
            }
            Exp::ObjectRef(obj) => {
                let label = match obj {
                    CallObject::FunctionPtr(f) => Label::Target(f.to_string()),
                    other => Label::Class(other.to_string()),
                };
                self.add(dst, d, [(&label, scope)]);
            }
            Exp::Str(s) => {
                let label = Label::Str(s.to_string());
                self.add(dst, d, [(&label, scope)]);
            }
            Exp::Int(_) | Exp::Bytes(_) => {}
        }
    }

    fn step(&mut self, inst: usize) {
        let function = self.instances[inst].function;
        let program = self.program;
        let f = &program[function];
        let ports = self.ports;
        for seed in ports.seeds.iter().filter(|s| s.function == f.name) {
            let var = Self::var(inst, &seed.path.base);
            let label = Label::Taint(seed.label.clone());
            let value = path::concat(&seed.path.segments, &[PathSegment::Value]);
            self.add(&var, &seed.path.segments, [(&label, Scope::All)]);
            self.add(&var, &value, [(&label, Scope::All)]);
        }
        for obs in ports.observers.iter().filter(|o| o.function == f.name) {
            self.observe(inst, &obs.path, obs.name.clone());
        }
        for (bb, block) in f.blocks.iter_enumerated() {
            for (i, stmt) in block.statements.iter().enumerate() {
                match &stmt.kind {
                    StatementKind::Assign { dest, sources } => {
                        let dst = Self::var(inst, &dest.base);
                        for e in sources {
                            self.assign_exp(&dst, &dest.segments, inst, e);
                        }
                    }
                    StatementKind::Phi { dest, operands } => {
                        let dst = Self::var(inst, dest);
                        for (_, op) in operands {
                            self.assign(&dst, &[], &Self::var(inst, op), &[], false);
                        }
                    }
                    StatementKind::CallAssign { style, rets, args } => {
                        self.call(inst, bb, i, stmt.source_info, style, rets, args);
                    }
                    StatementKind::Nop => {}
                }
            }
        }
    }

    /// Where an unresolved call is, for [`Outcome::unresolved`].
    fn site(&self, inst: usize, bb: BasicBlockIdx, i: usize) -> String {
        let f = &self.program[self.instances[inst].function];
        format!("{}@{}:{}", f.name, bb.index(), i)
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        inst: usize,
        bb: BasicBlockIdx,
        i: usize,
        span: SourceInfo,
        style: &CallStyle,
        rets: &[VariableRef],
        args: &[Exp],
    ) {
        let targets: Vec<String> = match style {
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(names),
            } => names.iter().cloned().collect(),
            CallStyle::FuncPtrCall { callee, .. } => {
                // The target labels at the callee path and at its value (S11).
                let var = Self::var(inst, &callee.base);
                let value = path::concat(&callee.segments, &[PathSegment::Value]);
                let mut names: BTreeSet<String> = BTreeSet::new();
                for p in [&callee.segments[..], &value[..]] {
                    for label in self.read(&var, p).into_keys() {
                        if let Label::Target(f) = label {
                            names.insert(f);
                        }
                    }
                }
                if names.is_empty() {
                    let site = self.site(inst, bb, i);
                    self.outcome
                        .unresolved
                        .insert(format!("{site}: no target for {callee}"));
                }
                names.into_iter().collect()
            }
            other => {
                // Virtual calls resolve through a class hierarchy this evaluator does not
                // take; they are pinned by the engines' own tests (R42).
                let site = self.site(inst, bb, i);
                self.outcome
                    .unresolved
                    .insert(format!("{site}: unsupported call style {other}"));
                Vec::new()
            }
        };
        for name in targets {
            if self.ports.sources.contains(&name) {
                self.source(inst, &name, rets, args);
            } else if self.ports.sinks.contains(&name) {
                self.sink(inst, span, &name, args);
            } else if let Some(&callee) = self.by_name.get(name.as_str()) {
                self.instantiate(inst, bb, i, callee, rets, args);
            } else {
                let site = self.site(inst, bb, i);
                self.outcome
                    .unresolved
                    .insert(format!("{site}: no body for {name}"));
            }
        }
    }

    fn source(&mut self, inst: usize, name: &str, rets: &[VariableRef], args: &[Exp]) {
        let Some(r) = rets.first() else {
            return;
        };
        let label = match args.first() {
            Some(Exp::Str(s)) => Label::Taint(s.to_string()),
            _ => Label::Taint(name.to_string()),
        };
        let var = Self::var(inst, r);
        self.add(&var, &[], [(&label, Scope::All)]);
        self.add(&var, &[PathSegment::Value], [(&label, Scope::All)]);
    }

    fn sink(&mut self, inst: usize, span: SourceInfo, name: &str, args: &[Exp]) {
        let Some(ap) = args.first().and_then(Exp::access_path) else {
            return;
        };
        let port_label = match args.get(1) {
            Some(Exp::Str(s)) => s.to_string(),
            _ => "_".to_string(),
        };
        let site = format!("{name}({port_label})@{span}");
        self.observe(inst, ap, site);
    }

    /// Records every taint label at `ap` and `ap.#value` in `inst` as observed at `site`.
    fn observe(&mut self, inst: usize, ap: &AccessPath, site: String) {
        let var = Self::var(inst, &ap.base);
        let value = path::concat(&ap.segments, &[PathSegment::Value]);
        let mut labels = self.read(&var, &ap.segments);
        labels.extend(self.read(&var, &value));
        let function = self.program[self.instances[inst].function].name.clone();
        for label in labels.into_keys() {
            if let Label::Taint(label) = label {
                let obs = Observation {
                    function: function.clone(),
                    site: site.clone(),
                    label,
                };
                if self.outcome.observed.insert(obs) {
                    self.changed = true;
                }
            }
        }
    }

    /// The call of `callee` at `(bb, i)` in `inst`: the copies into its formals, its body as a
    /// child instance, and the copies of its returns (S10).
    fn instantiate(
        &mut self,
        inst: usize,
        bb: BasicBlockIdx,
        i: usize,
        callee: FunctionIdx,
        rets: &[VariableRef],
        args: &[Exp],
    ) {
        let key = (bb, i, callee);
        let child = match self.instances[inst].children.get(&key) {
            Some(&c) => c,
            None => {
                let depth = self.instances[inst].depth + 1;
                if depth > self.config.depth {
                    self.outcome.truncated = true;
                    return;
                }
                self.instances.push(Instance {
                    function: callee,
                    depth,
                    children: HashMap::new(),
                });
                let c = self.instances.len() - 1;
                self.instances[inst].children.insert(key, c);
                self.changed = true;
                c
            }
        };
        let program = self.program;
        let f = &program[callee];
        // `p_i = a_i`.
        for (idx, arg) in args.iter().enumerate().take(f.num_parameters()) {
            let formal = VariableRef::new_parameter(crate::mir::ParameterIdx::new(idx));
            self.assign_exp(&Self::var(child, &formal), &[], inst, arg);
        }
        // `r_j = ret_j`, for every return.
        let returns: Vec<Vec<Exp>> = f
            .blocks
            .iter_enumerated()
            .filter_map(|(_, b)| match &b.terminator_opt()?.kind {
                TerminatorKind::Return { args } => Some(args.iter().cloned().collect()),
                _ => None,
            })
            .collect();
        for ret in returns {
            for (r, e) in rets.iter().zip(ret.iter()) {
                self.assign_exp(&Self::var(inst, r), &[], child, e);
            }
        }
    }
}
