/*! The built-in input relations, materialized over one program.

This is the DSL's view of a [`ProgramMatchIndex`] plus the IR it was built from. It is built
once per import and dropped with it, exactly as the match index is: the engine keeps only the
*bindings* a rule produced, never the tables it produced them from.

# Two spellings of one name

`fun`'s `name`, `signature` and `qualified-id` attributes are **matched** against every spelling
a frontend publishes for a function, and **bound** to the canonical one. The asymmetry is not an
accident: Ghidra names an imported `system` as `<EXTERNAL>::system@00101008` while the frontend
also publishes the simple `system`, and Lua publishes both `os.execute` and the bare `execute`
for one callee. A model that says `name = "system"` must match; a model that says
`fun(F, name = N)` must bind `N` to one name, because the design makes every attribute
functionally dependent on the columns. Matching generously and binding canonically is what
satisfies both.
*/

use hashbrown::hash_map::HashMap;

use ctadl_ir::mir::call::{CallEdges, CallStyle, VirtualMethodTable};
use ctadl_ir::mir::{FunctionData, StatementKind, Variable};

use crate::facts::Str;
use crate::models::match_index::ProgramMatchIndex;

/// One function, as the DSL sees it.
#[derive(Clone, Debug)]
pub struct FunRow {
    /// The fully qualified name — the `fun` relation's one column, and what every match
    /// structure keys on.
    pub fq: Str,
    /// Accepted spellings of the simple name; `names[0]` is the canonical one.
    pub names: Vec<Str>,
    /// Owning class. Populated only where the frontend has a class hierarchy (Java, Lua).
    pub parents: Vec<Str>,
    pub signatures: Vec<Str>,
    pub qualified_ids: Vec<Str>,
    /// Parameter count, where the IR recovered one. `None` for a bodyless callee — an external,
    /// a dex/jvm `ext` stub — which is why `arity` and `param` do not match one.
    pub arity: Option<i64>,
    /// Whether the function has a body. `None`, again, for a function with no IR data at all.
    pub has_code: Option<bool>,
}

/// One call site.
#[derive(Clone, Debug)]
pub struct CallSiteRow {
    /// The function the site sits in.
    pub caller: Str,
    /// A stable per-program identity: `caller#block:statement`. Stable within one import, which
    /// is all a rule can join on.
    pub id: Str,
    /// The callee as the site names it: a fully qualified function name (joinable with `fun`)
    /// or, for an indirect call, the variable the program text calls through.
    pub callee: Str,
}

/// Every built-in relation, materialized for one program.
pub struct ProgramFacts {
    pub funs: Vec<FunRow>,
    pub callsites: Vec<CallSiteRow>,
    /// `(function, field)` pairs from every `Load`/`Store`.
    pub uses_field: Vec<(Str, Str)>,
    /// Direct `(subclass, superclass)` edges.
    pub subclass: Vec<(Str, Str)>,
    /// Every class name the program mentions, sorted. This is what `subclass*` enumerates over
    /// when neither column is bound: the reflexive pair `(C, C)` exists for every class,
    /// including one with no supertype at all, which the edge list alone does not name.
    pub classes: Vec<Str>,
    /// The import's language name (`dex`, `pcode`, `lua`, …), if known.
    pub language: Option<Str>,
    /// The import's name, if known.
    pub import: Option<Str>,

    by_fq: HashMap<Str, usize>,
    by_name: HashMap<Str, Vec<usize>>,
    by_parent: HashMap<Str, Vec<usize>>,
    by_signature: HashMap<Str, Vec<usize>>,
    by_qualified_id: HashMap<Str, Vec<usize>>,
    callsites_by_caller: HashMap<Str, Vec<usize>>,
    callsites_by_callee: HashMap<Str, Vec<usize>>,
    callsite_by_id: HashMap<Str, usize>,
    uses_field_by_fun: HashMap<Str, Vec<usize>>,
    supers_of: HashMap<Str, Vec<Str>>,
}

impl ProgramFacts {
    /// Materializes the relations for the program `index` was built from.
    pub fn build(index: &ProgramMatchIndex<'_>) -> Self {
        let mut rows: Vec<FunRow> = Vec::new();
        let mut by_fq: HashMap<Str, usize> = HashMap::new();

        // `row_for` is what gives one row per function even when a frontend publishes several
        // table entries for it (a Java method under two classes, a native symbol under its
        // simple and decorated names).
        macro_rules! row_for {
            ($fq:expr) => {{
                let fq: Str = $fq;
                match by_fq.get(&fq) {
                    Some(&i) => i,
                    None => {
                        let i = rows.len();
                        rows.push(FunRow {
                            fq,
                            names: Vec::new(),
                            parents: Vec::new(),
                            signatures: Vec::new(),
                            qualified_ids: Vec::new(),
                            arity: None,
                            has_code: None,
                        });
                        by_fq.insert(fq, i);
                        i
                    }
                }
            }};
        }

        match index.vmt() {
            VirtualMethodTable::Java { methods, .. } => {
                for (cls, name, sig, fid) in methods {
                    let i = row_for!(Str::from(fid.as_ref()));
                    push_unique(&mut rows[i].names, Str::from(name.as_ref()));
                    push_unique(&mut rows[i].parents, Str::from(cls.as_ref()));
                    push_unique(&mut rows[i].signatures, Str::from(sig.as_ref()));
                    push_unique(&mut rows[i].qualified_ids, Str::from(fid.as_ref()));
                }
            }
            VirtualMethodTable::Native { methods } => {
                for (simple, sig, fq, qualified) in methods {
                    let i = row_for!(Str::from(fq.as_ref()));
                    // The simple name first: it is the canonical one, and it is what a model
                    // written against a stripped or a decorated binary can share.
                    push_unique(&mut rows[i].names, Str::from(simple.as_ref()));
                    push_unique(&mut rows[i].names, Str::from(fq.as_ref()));
                    push_unique(&mut rows[i].signatures, Str::from(sig.as_ref()));
                    push_unique(&mut rows[i].signatures, Str::from(fq.as_ref()));
                    push_unique(&mut rows[i].qualified_ids, Str::from(qualified.as_ref()));
                    push_unique(&mut rows[i].qualified_ids, Str::from(fq.as_ref()));
                }
            }
            VirtualMethodTable::Lua {
                methods,
                functions,
                externals,
                ..
            } => {
                for (simple, fq) in functions.iter().chain(externals.iter()) {
                    let i = row_for!(Str::from(fq.as_ref()));
                    push_unique(&mut rows[i].names, Str::from(simple.as_ref()));
                    push_unique(&mut rows[i].names, Str::from(fq.as_ref()));
                    push_unique(&mut rows[i].signatures, Str::from(simple.as_ref()));
                    push_unique(&mut rows[i].signatures, Str::from(fq.as_ref()));
                    // Only the fq name is a qualified id: keying the bare name here would hand
                    // `qualified-id` exactly the collisions it exists to remove.
                    push_unique(&mut rows[i].qualified_ids, Str::from(fq.as_ref()));
                }
                // The metatable-recovered class methods give Lua its `parent`.
                for (cls, _name, fq) in methods {
                    let i = row_for!(Str::from(fq.as_ref()));
                    push_unique(&mut rows[i].parents, Str::from(cls.as_ref()));
                }
            }
            VirtualMethodTable::Unknown => {}
        }

        // Every lowered function, whether or not the method table names it. This is what gives
        // flowy and pcode a `fun` relation at all, and it is where arity / has_code come from.
        for (fq, data) in index.functions_with_data() {
            let i = row_for!(Str::from(fq));
            let row = &mut rows[i];
            push_unique(&mut row.names, Str::from(fq));
            push_unique(&mut row.signatures, Str::from(fq));
            push_unique(&mut row.qualified_ids, Str::from(fq));
            row.arity = Some(data.num_parameters() as i64);
            row.has_code = Some(!data.blocks.is_empty());
        }

        let mut by_name: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut by_parent: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut by_signature: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut by_qualified_id: HashMap<Str, Vec<usize>> = HashMap::new();
        for (i, row) in rows.iter().enumerate() {
            for n in &row.names {
                by_name.entry(*n).or_default().push(i);
            }
            for p in &row.parents {
                by_parent.entry(*p).or_default().push(i);
            }
            for s in &row.signatures {
                by_signature.entry(*s).or_default().push(i);
            }
            for q in &row.qualified_ids {
                by_qualified_id.entry(*q).or_default().push(i);
            }
        }

        let (callsites, uses_field) = scan_bodies(index);
        let mut callsites_by_caller: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsites_by_callee: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsite_by_id: HashMap<Str, usize> = HashMap::new();
        for (i, cs) in callsites.iter().enumerate() {
            callsites_by_caller.entry(cs.caller).or_default().push(i);
            callsites_by_callee.entry(cs.callee).or_default().push(i);
            callsite_by_id.insert(cs.id, i);
        }
        let mut uses_field_by_fun: HashMap<Str, Vec<usize>> = HashMap::new();
        for (i, (f, _)) in uses_field.iter().enumerate() {
            uses_field_by_fun.entry(*f).or_default().push(i);
        }

        let mut subclass: Vec<(Str, Str)> = Vec::new();
        let mut supers_of: HashMap<Str, Vec<Str>> = HashMap::new();
        match index.vmt() {
            VirtualMethodTable::Java { hierarchy, .. } => {
                for (cls, supers) in hierarchy {
                    let sub = Str::from(cls.as_ref());
                    for sup in supers {
                        let sup = Str::from(sup.as_ref());
                        subclass.push((sub, sup));
                        supers_of.entry(sub).or_default().push(sup);
                    }
                }
            }
            VirtualMethodTable::Lua { hierarchy, .. } => {
                for (cls, supers) in hierarchy {
                    let sub = Str::from(cls.as_ref());
                    for sup in supers {
                        let sup = Str::from(sup.as_ref());
                        subclass.push((sub, sup));
                        supers_of.entry(sub).or_default().push(sup);
                    }
                }
            }
            _ => {}
        }
        subclass.sort_unstable();
        subclass.dedup();

        let mut classes: Vec<Str> = subclass
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .chain(rows.iter().flat_map(|r| r.parents.iter().copied()))
            .collect();
        classes.sort_unstable();
        classes.dedup();

        Self {
            funs: rows,
            callsites,
            uses_field,
            subclass,
            classes,
            language: index.scope.language.map(|l| Str::from(l.name())),
            import: index.scope.import.as_deref().map(Str::from),
            by_fq,
            by_name,
            by_parent,
            by_signature,
            by_qualified_id,
            callsites_by_caller,
            callsites_by_callee,
            callsite_by_id,
            uses_field_by_fun,
            supers_of,
        }
    }

    #[inline]
    pub fn fun_row(&self, fq: Str) -> Option<&FunRow> {
        self.by_fq.get(&fq).map(|&i| &self.funs[i])
    }

    #[inline]
    pub fn fun_index(&self, fq: Str) -> Option<usize> {
        self.by_fq.get(&fq).copied()
    }

    #[inline]
    pub fn funs_by(&self, key: FunKey, value: Str) -> &[usize] {
        let map = match key {
            FunKey::Name => &self.by_name,
            FunKey::Parent => &self.by_parent,
            FunKey::Signature => &self.by_signature,
            FunKey::QualifiedId => &self.by_qualified_id,
        };
        map.get(&value).map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn callsites_of_caller(&self, caller: Str) -> &[usize] {
        self.callsites_by_caller
            .get(&caller)
            .map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn callsites_of_callee(&self, callee: Str) -> &[usize] {
        self.callsites_by_callee
            .get(&callee)
            .map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn callsite_index(&self, id: Str) -> Option<usize> {
        self.callsite_by_id.get(&id).copied()
    }

    #[inline]
    pub fn uses_field_of(&self, f: Str) -> &[usize] {
        self.uses_field_by_fun.get(&f).map_or(&[], |v| v.as_slice())
    }

    /// Superclasses of `cls` at the requested closure. `reflexive` adds `cls` itself, `deep`
    /// walks the chain; `(false, false)` is the direct relation.
    pub fn supers(&self, cls: Str, reflexive: bool, deep: bool) -> Vec<Str> {
        let mut out: Vec<Str> = Vec::new();
        if reflexive {
            out.push(cls);
        }
        if !deep {
            if let Some(direct) = self.supers_of.get(&cls) {
                out.extend(direct.iter().copied());
            }
            out.sort_unstable();
            out.dedup();
            return out;
        }
        // Depth-first with a seen set: a frontend's `__index` chain can be cyclic, and a cycle
        // must not become a hang.
        let mut seen: std::collections::BTreeSet<Str> = std::collections::BTreeSet::new();
        let mut stack = vec![cls];
        while let Some(c) = stack.pop() {
            let Some(direct) = self.supers_of.get(&c) else {
                continue;
            };
            for s in direct {
                if seen.insert(*s) {
                    out.push(*s);
                    stack.push(*s);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Which of `fun`'s indexed attributes a lookup keys on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FunKey {
    Name,
    Parent,
    Signature,
    QualifiedId,
}

fn push_unique(v: &mut Vec<Str>, s: Str) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// One pass over every function body, collecting the two relations that need one.
fn scan_bodies(index: &ProgramMatchIndex<'_>) -> (Vec<CallSiteRow>, Vec<(Str, Str)>) {
    let mut callsites = Vec::new();
    let mut fields: Vec<(Str, Str)> = Vec::new();
    for (fq, data) in index.functions_with_data() {
        let caller = Str::from(fq);
        for (b, block) in data.blocks.iter().enumerate() {
            for (s, stmt) in block.statements.iter().enumerate() {
                match &stmt.kind {
                    StatementKind::Load { field, .. } | StatementKind::Store { field, .. } => {
                        fields.push((caller, Str::from(field.field.as_ref())));
                    }
                    StatementKind::CallAssign { style, .. } => {
                        for callee in callee_strings(style, data) {
                            callsites.push(CallSiteRow {
                                caller,
                                id: Str::from(format!("{fq}#{b}:{s}")),
                                callee,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    fields.sort_unstable();
    fields.dedup();
    (callsites, fields)
}

/// How each call style names its callee.
///
/// A direct call can list several edges — that is CHA's answer, not an ambiguity — and each is
/// one row, so `callsite(F, S, callee_string = G)` joins on any of them.
fn callee_strings(style: &CallStyle, data: &FunctionData) -> Vec<Str> {
    match style {
        CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(edges),
        } => edges.iter().map(|e| Str::from(e.as_str())).collect(),
        CallStyle::JavaCall {
            cls,
            simple_name,
            descriptor,
            ..
        } => vec![Str::from(format!("{cls}->{simple_name}{descriptor}"))],
        CallStyle::LuaCall { method, .. } => vec![Str::from(method.as_ref())],
        CallStyle::FuncPtrCall { callee, .. } => {
            // "the variable name from the program text": a local resolves through the
            // function's own declaration table, which is the only place the source name lives.
            let name = match &*callee.variable_ref.variable {
                Variable::Local(idx) => data
                    .locals
                    .get(*idx)
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| callee.variable_ref.to_string()),
                other => other.to_string(),
            };
            vec![Str::from(name)]
        }
        CallStyle::Unknown => Vec::new(),
    }
}
