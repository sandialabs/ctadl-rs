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

# What this interns

Every value is a [`Str`], whose interner never frees, so what a build interns stays for the
life of the process. The tables therefore hold only strings the program itself spells — names,
classes, signatures, fields, callees — each of which the interner keeps once however many
imports or rows repeat it. Nothing is composed per row: a call site is identified by its
position ([`SiteId`]) rather than by an interned `caller#block:statement`, and the tables no
loaded rule reads ([`Needs`]) are not built at all.
*/

use std::ops::{Deref, Range};

use hashbrown::hash_map::HashMap;

use ctadl_ir::mir::call::{CallEdges, CallStyle};
use ctadl_ir::mir::{FunctionData, StatementKind, Variable};

use crate::facts::Str;
use crate::models::dsl::ast::{BodyItem, Program};
use crate::models::match_index::{ProgramMatchIndex, for_each_spelling};
use ctadl_ir::mir::call::VirtualMethodTable;

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

impl FunRow {
    fn new(fq: Str) -> Self {
        Self {
            fq,
            names: Vec::new(),
            parents: Vec::new(),
            signatures: Vec::new(),
            qualified_ids: Vec::new(),
            arity: None,
            has_code: None,
        }
    }

    fn spellings_mut(&mut self, key: FunKey) -> &mut Vec<Str> {
        match key {
            FunKey::Name => &mut self.names,
            FunKey::Parent => &mut self.parents,
            FunKey::Signature => &mut self.signatures,
            FunKey::QualifiedId => &mut self.qualified_ids,
        }
    }
}

/// Which call site a row is: the function it sits in and the statement's position there.
/// Stable within one import, which is all a rule can join on.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SiteId {
    pub caller: Str,
    pub block: u32,
    pub stmt: u32,
}

impl SiteId {
    /// Reads back a [`Display`](std::fmt::Display)ed id. Interns nothing: a caller that was
    /// never interned is in no table, so its spelling names no site.
    pub fn parse(text: &str) -> Option<Self> {
        let (caller, pos) = text.rsplit_once('#')?;
        let (block, stmt) = pos.split_once(':')?;
        Some(Self {
            caller: Str::try_get(caller)?,
            block: block.parse().ok()?,
            stmt: stmt.parse().ok()?,
        })
    }
}

/// `caller#block:statement`, the spelling a site prints as and a literal names it by.
impl std::fmt::Display for SiteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}:{}", self.caller, self.block, self.stmt)
    }
}

/// One call site, or one of its edges: a direct call CHA resolved several ways is one row per
/// callee, all with the same [`SiteId`].
#[derive(Clone, Debug)]
pub struct CallSiteRow {
    /// The function the site sits in.
    pub caller: Str,
    /// The statement's block and its index in that block.
    pub block: u32,
    pub stmt: u32,
    /// The callee as the site names it: a fully qualified function name (joinable with `fun`)
    /// or, for an indirect call, the variable the program text calls through.
    pub callee: Str,
}

impl CallSiteRow {
    pub fn site(&self) -> SiteId {
        SiteId {
            caller: self.caller,
            block: self.block,
            stmt: self.stmt,
        }
    }
}

/// One distinct static call-site signature: what `callsig` ranges over and what a `dispatch`
/// head attaches a model to.
///
/// Deliberately *not* a [`FunRow`]. The two universes differ: `funs` is built from the VMT's
/// implementations, and about half of an app's interface call sites name a type that has no row
/// there -- `java.util.Iterator` is never an implementation -- so `fun` cannot name them. These
/// keys come from the call sites themselves, so by construction they can.
///
/// Every attribute is a single value rather than a list of accepted spellings: a key is
/// composed from the site's three components, so there is only ever one way to spell it.
#[derive(Clone, Debug)]
pub struct CallSigRow {
    /// `Lcls;->name(desc)` -- the canonical id, and the `callsig` relation's one column.
    pub id: Str,
    /// The simple method name the site names.
    pub name: Str,
    /// The static class the site names.
    pub parent: Str,
    /// The descriptor, e.g. `()Ljava/lang/String;`.
    pub signature: Str,
}

/// Every built-in relation, materialized for one program.
///
/// A table no loaded rule reads (see [`Needs`]) is left empty; `fun` and `param` are always
/// there.
pub struct ProgramFacts {
    pub funs: Vec<FunRow>,
    /// The distinct static call-site signatures, empty unless the import was indexed with a
    /// dispatch universe (see `ProgramMatchIndex::new_with_dispatch`). A rule using `callsig`
    /// against a program collected without one matches nothing rather than erroring: the same
    /// posture `fun`'s body-dependent attributes take on a bodyless callee.
    pub callsigs: Vec<CallSigRow>,
    /// Grouped by site: the edges of one site are adjacent.
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
    callsites_by_site: HashMap<SiteId, Range<usize>>,
    /// `0..callsites.len()`, so a site's rows can be handed out as a slice of indices like
    /// every other lookup's.
    callsite_indices: Vec<usize>,
    uses_field_by_fun: HashMap<Str, Vec<usize>>,
    supers_of: HashMap<Str, Vec<Str>>,
    subs_of: HashMap<Str, Vec<Str>>,
    callsigs_by_id: HashMap<Str, usize>,
    callsigs_by_name: HashMap<Str, Vec<usize>>,
    callsigs_by_parent: HashMap<Str, Vec<usize>>,
    callsigs_by_signature: HashMap<Str, Vec<usize>>,
}

/// Which input relations some loaded rule reads, so `ProgramFacts::build` can skip the tables
/// nobody asks for. `fun` and `param` read only the function table, which is always built.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Needs {
    /// `callsite`: a pass over every statement.
    pub callsite: bool,
    /// `uses_field`: the same pass.
    pub uses_field: bool,
    /// `subclass`, `subclass*`, `subclass+`.
    pub subclass: bool,
    /// `callsig`. The universe itself is collected only when someone asks for it (see
    /// `Program::uses_callsig`), but a JSON `find: "dispatch"` generator can be the one that
    /// did.
    pub callsig: bool,
}

impl Needs {
    /// Every table.
    pub fn all() -> Self {
        Self {
            callsite: true,
            uses_field: true,
            subclass: true,
            callsig: true,
        }
    }

    /// The tables the rules of `program` read, under negations and inside groups too: a
    /// filter reads its relation as surely as a generator does.
    pub fn of_program(program: &Program) -> Self {
        fn visit(item: &BodyItem, needs: &mut Needs) {
            match item {
                BodyItem::Atom(atom) => match atom.name.as_str() {
                    "callsite" => needs.callsite = true,
                    "uses_field" => needs.uses_field = true,
                    "subclass" | "subclass*" | "subclass+" => needs.subclass = true,
                    "callsig" => needs.callsig = true,
                    _ => {}
                },
                BodyItem::Not(inner) => visit(inner, needs),
                BodyItem::And(items) | BodyItem::Or(items) => {
                    items.iter().for_each(|i| visit(i, needs))
                }
            }
        }
        let mut needs = Self::default();
        for rule in &program.rules {
            rule.body.iter().for_each(|i| visit(i, &mut needs));
        }
        needs
    }

    pub fn union(&mut self, other: Self) {
        self.callsite |= other.callsite;
        self.uses_field |= other.uses_field;
        self.subclass |= other.subclass;
        self.callsig |= other.callsig;
    }
}

impl ProgramFacts {
    /// Materializes the relations `needs` asks for, over the program `index` was built from.
    pub fn build(index: &ProgramMatchIndex<'_>, needs: &Needs) -> Self {
        let mut rows: Vec<FunRow> = Vec::new();
        let mut by_fq: HashMap<Str, usize> = HashMap::new();

        // One row per function even when a frontend publishes several table entries for it (a
        // Java method under two classes, a native symbol under its simple and decorated names).
        // A function's spellings arrive together, so its handle is interned once rather than
        // once per spelling.
        let mut current: (&str, usize) = ("", usize::MAX);
        for_each_spelling(index.vmt(), |fq, key, spelling| {
            if !std::ptr::eq(fq, current.0) {
                current = (fq, row_for(&mut rows, &mut by_fq, Str::from(fq)));
            }
            let row = &mut rows[current.1];
            let value = if spelling == fq {
                row.fq
            } else {
                Str::from(spelling)
            };
            push_unique(row.spellings_mut(key), value);
        });

        // Every lowered function, whether or not the method table names it. This is what gives
        // flowy and pcode a `fun` relation at all, and it is where arity / has_code come from.
        let mut lowered: Vec<(Str, &FunctionData)> = Vec::new();
        for (fq, data) in index.functions_with_data() {
            let i = row_for(&mut rows, &mut by_fq, Str::from(fq));
            let row = &mut rows[i];
            let fq = row.fq;
            push_unique(&mut row.names, fq);
            push_unique(&mut row.signatures, fq);
            push_unique(&mut row.qualified_ids, fq);
            row.arity = Some(data.num_parameters() as i64);
            row.has_code = Some(!data.blocks.is_empty());
            lowered.push((fq, data));
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

        let (callsites, uses_field) = if needs.callsite || needs.uses_field {
            scan_bodies(&lowered, needs)
        } else {
            Default::default()
        };
        let mut callsites_by_caller: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsites_by_callee: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsites_by_site: HashMap<SiteId, Range<usize>> = HashMap::new();
        for (i, cs) in callsites.iter().enumerate() {
            callsites_by_caller.entry(cs.caller).or_default().push(i);
            callsites_by_callee.entry(cs.callee).or_default().push(i);
            // A site's edges are adjacent, so extending the range covers them all.
            callsites_by_site
                .entry(cs.site())
                .and_modify(|r| r.end = i + 1)
                .or_insert(i..i + 1);
        }
        let mut uses_field_by_fun: HashMap<Str, Vec<usize>> = HashMap::new();
        for (i, (f, _)) in uses_field.iter().enumerate() {
            uses_field_by_fun.entry(*f).or_default().push(i);
        }

        let mut subclass: Vec<(Str, Str)> = Vec::new();
        let mut classes: Vec<Str> = Vec::new();
        let mut supers_of: HashMap<Str, Vec<Str>> = HashMap::new();
        let mut subs_of: HashMap<Str, Vec<Str>> = HashMap::new();
        if needs.subclass {
            match index.vmt() {
                VirtualMethodTable::Java { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
                VirtualMethodTable::Cil { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
                VirtualMethodTable::Lua { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
                _ => {}
            }
            subclass.sort_unstable();
            subclass.dedup();
            for &(sub, sup) in &subclass {
                supers_of.entry(sub).or_default().push(sup);
                subs_of.entry(sup).or_default().push(sub);
            }
            classes = subclass
                .iter()
                .flat_map(|(a, b)| [*a, *b])
                .chain(rows.iter().flat_map(|r| r.parents.iter().copied()))
                .collect();
            classes.sort_unstable();
            classes.dedup();
        }

        // The call-site signature universe, when the caller collected one and a rule reads it.
        // `parts` is already the distinct set, so there is nothing to deduplicate here.
        let mut callsigs: Vec<CallSigRow> = Vec::new();
        if let Some(universe) = index.dispatch.as_ref().filter(|_| needs.callsig) {
            callsigs.extend(
                universe
                    .parts
                    .iter()
                    .map(|(id, (cls, name, desc))| CallSigRow {
                        id: Str::from(*id),
                        name: Str::from(*name),
                        parent: Str::from(*cls),
                        signature: Str::from(*desc),
                    }),
            );
            // `parts` is a hash map, so its iteration order is not the program's. Sort by the
            // canonical id: a rule that scans the universe must enumerate the same way twice.
            callsigs.sort_unstable_by_key(|r| r.id);
        }
        let mut callsigs_by_id: HashMap<Str, usize> = HashMap::new();
        let mut callsigs_by_name: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsigs_by_parent: HashMap<Str, Vec<usize>> = HashMap::new();
        let mut callsigs_by_signature: HashMap<Str, Vec<usize>> = HashMap::new();
        for (i, row) in callsigs.iter().enumerate() {
            callsigs_by_id.insert(row.id, i);
            callsigs_by_name.entry(row.name).or_default().push(i);
            callsigs_by_parent.entry(row.parent).or_default().push(i);
            callsigs_by_signature
                .entry(row.signature)
                .or_default()
                .push(i);
        }

        Self {
            funs: rows,
            callsigs,
            callsite_indices: (0..callsites.len()).collect(),
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
            callsites_by_site,
            uses_field_by_fun,
            supers_of,
            subs_of,
            callsigs_by_id,
            callsigs_by_name,
            callsigs_by_parent,
            callsigs_by_signature,
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
    pub fn callsig_index(&self, id: Str) -> Option<usize> {
        self.callsigs_by_id.get(&id).copied()
    }

    /// Row indices a `callsig` attribute lookup reaches. `qualified-id` is the id itself, which
    /// is why it needs no map of its own.
    #[inline]
    pub fn callsigs_by(&self, key: FunKey, value: Str) -> &[usize] {
        match key {
            FunKey::Name => self.callsigs_by_name.get(&value),
            FunKey::Parent => self.callsigs_by_parent.get(&value),
            FunKey::Signature => self.callsigs_by_signature.get(&value),
            FunKey::QualifiedId => {
                return match self.callsigs_by_id.get(&value) {
                    Some(i) => std::slice::from_ref(i),
                    None => &[],
                };
            }
        }
        .map_or(&[], |v| v.as_slice())
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

    /// The rows of one site: one per edge, so more than one for a call CHA resolved several
    /// ways.
    #[inline]
    pub fn callsites_with_id(&self, site: SiteId) -> &[usize] {
        self.callsites_by_site
            .get(&site)
            .map_or(&[], |r| &self.callsite_indices[r.clone()])
    }

    #[inline]
    pub fn uses_field_of(&self, f: Str) -> &[usize] {
        self.uses_field_by_fun.get(&f).map_or(&[], |v| v.as_slice())
    }

    /// Superclasses of `cls` at the requested closure. `reflexive` adds `cls` itself, `deep`
    /// walks the chain; `(false, false)` is the direct relation.
    pub fn supers(&self, cls: Str, reflexive: bool, deep: bool) -> Vec<Str> {
        closure(&self.supers_of, cls, reflexive, deep)
    }

    /// Subclasses of `cls` at the requested closure; the converse of [`Self::supers`].
    pub fn subs(&self, cls: Str, reflexive: bool, deep: bool) -> Vec<Str> {
        closure(&self.subs_of, cls, reflexive, deep)
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

fn row_for(rows: &mut Vec<FunRow>, by_fq: &mut HashMap<Str, usize>, fq: Str) -> usize {
    *by_fq.entry(fq).or_insert_with(|| {
        rows.push(FunRow::new(fq));
        rows.len() - 1
    })
}

fn push_unique(v: &mut Vec<Str>, s: Str) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// A frontend's class hierarchy as `(subclass, superclass)` edges.
fn push_edges<'a, C, S>(
    hierarchy: impl IntoIterator<Item = (&'a C, &'a S)>,
    out: &mut Vec<(Str, Str)>,
) where
    C: Deref<Target: AsRef<str>> + 'a,
    S: Deref<Target = [C]> + 'a,
{
    for (cls, supers) in hierarchy {
        let sub = Str::from((**cls).as_ref());
        out.extend(supers.iter().map(|sup| (sub, Str::from((**sup).as_ref()))));
    }
}

/// `cls`'s neighbors in `edges` at the requested closure, sorted.
fn closure(edges: &HashMap<Str, Vec<Str>>, cls: Str, reflexive: bool, deep: bool) -> Vec<Str> {
    let mut out: Vec<Str> = Vec::new();
    if reflexive {
        out.push(cls);
    }
    if !deep {
        out.extend(edges.get(&cls).into_iter().flatten().copied());
    } else {
        // Depth-first with a seen set: a frontend's `__index` chain can be cyclic, and a cycle
        // must not become a hang.
        let mut seen: hashbrown::HashSet<Str> = hashbrown::HashSet::new();
        let mut stack = vec![cls];
        while let Some(c) = stack.pop() {
            for &s in edges.get(&c).into_iter().flatten() {
                if seen.insert(s) {
                    out.push(s);
                    stack.push(s);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// One pass over every function body, collecting whichever of the two relations that need one
/// `needs` asks for.
fn scan_bodies(
    lowered: &[(Str, &FunctionData)],
    needs: &Needs,
) -> (Vec<CallSiteRow>, Vec<(Str, Str)>) {
    let mut callsites = Vec::new();
    let mut fields: Vec<(Str, Str)> = Vec::new();
    for &(caller, data) in lowered {
        for (b, block) in data.blocks.iter().enumerate() {
            for (s, stmt) in block.statements.iter().enumerate() {
                match &stmt.kind {
                    StatementKind::Load { field, .. } | StatementKind::Store { field, .. }
                        if needs.uses_field =>
                    {
                        fields.push((caller, Str::from(field.field.as_ref())));
                    }
                    StatementKind::CallAssign { style, .. } if needs.callsite => {
                        callsites.extend(callee_strings(style, data).into_iter().map(|callee| {
                            CallSiteRow {
                                caller,
                                block: b as u32,
                                stmt: s as u32,
                                callee,
                            }
                        }));
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
///
/// The Java and CIL spellings are composed, and are the one place a build may intern a string
/// the program does not already hold whole: the id of a method the import calls but does not
/// implement. It is per distinct callee, not per site.
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
        CallStyle::CilCall {
            cls,
            simple_name,
            descriptor,
            ..
        } => vec![Str::from(format!("{cls}::{simple_name}{descriptor}"))],
        CallStyle::LuaCall { method, .. } => vec![Str::from(method.as_ref())],
        CallStyle::FuncPtrCall { callee, .. } => {
            // "the variable name from the program text": a local resolves through the
            // function's own declaration table, which is the only place the source name lives.
            let name = match &callee.base.variable {
                Variable::Local(idx) => match data.locals.get(*idx) {
                    Some(d) => return vec![Str::from(d.name.as_str())],
                    None => callee.base.to_string(),
                },
                other => other.to_string(),
            };
            vec![Str::from(name)]
        }
        CallStyle::Unknown => Vec::new(),
    }
}
