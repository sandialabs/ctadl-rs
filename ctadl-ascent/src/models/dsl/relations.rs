/*! The built-in input relations, materialized over one program.

This is the DSL's view of a [`ProgramMatchIndex`] plus the IR it was built from. It is built
once per import and dropped with it, exactly as the match index is: the engine keeps only the
*bindings* a rule produced, never the tables it produced them from.

# Names

`fun`'s column is the IR name, the one identity a function has. Its `name`, `parent` and
`signature` are what the import's name table
([`VirtualMethodTable::names`](ctadl_ir::mir::call::VirtualMethodTable::names)) says, one value
each, so an attribute is a field of the row and never multiplies the solutions. The other names
a frontend knows a function by (pcode's namespace-qualified name, say) are rows of `name_of`,
together with the simple name: "some name of `F` is `x`" is a join a rule writes, not a
property of `fun`.

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
use crate::models::match_index::ProgramMatchIndex;
use ctadl_ir::mir::call::Dispatch;

/// One function, as the DSL sees it.
#[derive(Clone, Debug)]
pub struct FunRow {
    /// The fully qualified name — the `fun` relation's one column, and what every match
    /// structure keys on.
    pub fq: Str,
    /// The simple name the frontend gave it. `None` only for a function the name table does not
    /// describe, which an import cannot produce.
    pub name: Option<Str>,
    /// Owning class, where the frontend has a class hierarchy (Java, CIL, Lua).
    pub parent: Option<Str>,
    pub signature: Option<Str>,
    /// The other names the function is known by, none of them `fq` or `name`.
    pub other_names: Vec<Str>,
    /// Parameter count, where the IR recovered one. `None` for a bodyless callee — a Lua
    /// external — which is why `arity` and `param` do not match one.
    pub arity: Option<i64>,
    /// Whether the function has a body. `None`, again, for a function with no IR data at all.
    pub has_code: Option<bool>,
}

impl FunRow {
    fn new(fq: Str) -> Self {
        Self {
            fq,
            name: None,
            parent: None,
            signature: None,
            other_names: Vec::new(),
            arity: None,
            has_code: None,
        }
    }

    /// The `name_of` rows of this function: its simple name, then its other names.
    pub fn names(&self) -> impl Iterator<Item = Str> + '_ {
        self.name.iter().chain(&self.other_names).copied()
    }

    /// The value of `fun` attribute `key`.
    pub fn attr(&self, key: FunKey) -> Option<Str> {
        match key {
            FunKey::Name => self.name,
            FunKey::Parent => self.parent,
            FunKey::Signature => self.signature,
            // A `callsig` attribute only.
            FunKey::QualifiedId => None,
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
/// Every attribute is a single value, as on `fun`: a key is composed from the site's three
/// components, so there is only ever one way to spell it.
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

/// A row's position in one of [`ProgramFacts`]'s tables, as the indexes store it. The indexes
/// hold one per row they list, so a `u32` halves them; no one import has four billion rows.
pub type RowIdx = u32;

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
    /// In scan order: each caller's rows are contiguous, and within a caller they are sorted by
    /// `(block, stmt)`, so the edges of one site are adjacent. The site and caller lookups rely
    /// on that layout instead of indexing every row.
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

    by_fq: HashMap<Str, RowIdx>,
    by_name: HashMap<Str, Vec<RowIdx>>,
    by_parent: HashMap<Str, Vec<RowIdx>>,
    by_signature: HashMap<Str, Vec<RowIdx>>,
    /// The functions some `name_of` row of which is the key.
    by_name_of: HashMap<Str, Vec<RowIdx>>,
    /// Each caller's rows. One entry per caller, not per row or per site: a site is found by
    /// binary search within its caller's range (see [`Self::callsites_with_id`]).
    callsites_by_caller: HashMap<Str, Range<RowIdx>>,
    callsites_by_callee: HashMap<Str, Vec<RowIdx>>,
    uses_field_by_fun: HashMap<Str, Vec<RowIdx>>,
    supers_of: HashMap<Str, Vec<Str>>,
    subs_of: HashMap<Str, Vec<Str>>,
    callsigs_by_id: HashMap<Str, RowIdx>,
    callsigs_by_name: HashMap<Str, Vec<RowIdx>>,
    callsigs_by_parent: HashMap<Str, Vec<RowIdx>>,
    callsigs_by_signature: HashMap<Str, Vec<RowIdx>>,
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
        let mut by_fq: HashMap<Str, RowIdx> = HashMap::new();

        for n in &index.vmt().names {
            let i = row_for(&mut rows, &mut by_fq, Str::from(n.fq.as_ref()));
            let row = &mut rows[i];
            row.name = Some(Str::from(n.simple.as_ref()));
            row.parent = n.parent.as_deref().map(Str::from);
            row.signature = n.signature.as_deref().map(Str::from);
            row.other_names = n.other.iter().map(|o| Str::from(o.as_ref())).collect();
        }

        // Every lowered function. This is where arity / has_code come from.
        let mut lowered: Vec<(Str, &FunctionData)> = Vec::new();
        for (fq, data) in index.functions_with_data() {
            let i = row_for(&mut rows, &mut by_fq, Str::from(fq));
            let row = &mut rows[i];
            row.arity = Some(data.num_parameters() as i64);
            row.has_code = Some(!data.blocks.is_empty());
            lowered.push((row.fq, data));
        }

        let mut by_name: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        let mut by_parent: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        let mut by_signature: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        let mut by_name_of: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        for (i, row) in rows.iter().enumerate() {
            let i = row_idx(i);
            if let Some(n) = row.name {
                by_name.entry(n).or_default().push(i);
            }
            if let Some(p) = row.parent {
                by_parent.entry(p).or_default().push(i);
            }
            if let Some(s) = row.signature {
                by_signature.entry(s).or_default().push(i);
            }
            for n in row.names() {
                by_name_of.entry(n).or_default().push(i);
            }
        }

        let (callsites, uses_field) = if needs.callsite || needs.uses_field {
            scan_bodies(&lowered, needs)
        } else {
            Default::default()
        };
        let mut callsites_by_caller: HashMap<Str, Range<RowIdx>> = HashMap::new();
        let mut callsites_by_callee: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        for (i, cs) in callsites.iter().enumerate() {
            let i = row_idx(i);
            // A caller's rows are adjacent, so extending the range covers them all.
            callsites_by_caller
                .entry(cs.caller)
                .and_modify(|r| r.end = i + 1)
                .or_insert(i..i + 1);
            callsites_by_callee.entry(cs.callee).or_default().push(i);
        }
        let mut uses_field_by_fun: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        for (i, (f, _)) in uses_field.iter().enumerate() {
            let i = row_idx(i);
            uses_field_by_fun.entry(*f).or_default().push(i);
        }

        let mut subclass: Vec<(Str, Str)> = Vec::new();
        let mut classes: Vec<Str> = Vec::new();
        let mut supers_of: HashMap<Str, Vec<Str>> = HashMap::new();
        let mut subs_of: HashMap<Str, Vec<Str>> = HashMap::new();
        if needs.subclass {
            match &index.vmt().dispatch {
                Dispatch::Java { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
                Dispatch::Cil { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
                Dispatch::Lua { hierarchy, .. } => push_edges(hierarchy, &mut subclass),
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
                .chain(rows.iter().filter_map(|r| r.parent))
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
        let mut callsigs_by_id: HashMap<Str, RowIdx> = HashMap::new();
        let mut callsigs_by_name: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        let mut callsigs_by_parent: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        let mut callsigs_by_signature: HashMap<Str, Vec<RowIdx>> = HashMap::new();
        for (i, row) in callsigs.iter().enumerate() {
            let i = row_idx(i);
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
            by_name_of,
            callsites_by_caller,
            callsites_by_callee,
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
        self.by_fq.get(&fq).map(|&i| &self.funs[i as usize])
    }

    #[inline]
    pub fn fun_index(&self, fq: Str) -> Option<usize> {
        self.by_fq.get(&fq).map(|&i| i as usize)
    }

    /// Row indices whose attribute `key` is `value`, ascending.
    pub fn funs_by(&self, key: FunKey, value: Str) -> &[RowIdx] {
        let map = match key {
            FunKey::Name => &self.by_name,
            FunKey::Parent => &self.by_parent,
            FunKey::Signature => &self.by_signature,
            FunKey::QualifiedId => return &[],
        };
        map.get(&value).map_or(&[], |v| v.as_slice())
    }

    /// Row indices of the functions one of whose `name_of` rows is `name`, ascending.
    pub fn funs_named(&self, name: Str) -> &[RowIdx] {
        self.by_name_of.get(&name).map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn callsig_index(&self, id: Str) -> Option<usize> {
        self.callsigs_by_id.get(&id).map(|&i| i as usize)
    }

    /// Row indices a `callsig` attribute lookup reaches. `qualified-id` is the id itself, which
    /// is why it needs no map of its own.
    #[inline]
    pub fn callsigs_by(&self, key: FunKey, value: Str) -> &[RowIdx] {
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

    /// The rows of one caller, as a range of row indices: they are contiguous.
    #[inline]
    pub fn callsites_of_caller(&self, caller: Str) -> Range<usize> {
        self.callsites_by_caller
            .get(&caller)
            .map_or(0..0, |r| r.start as usize..r.end as usize)
    }

    #[inline]
    pub fn callsites_of_callee(&self, callee: Str) -> &[RowIdx] {
        self.callsites_by_callee
            .get(&callee)
            .map_or(&[], |v| v.as_slice())
    }

    /// The rows of one site: one per edge, so more than one for a call CHA resolved several
    /// ways. Found by binary search on `(block, stmt)` within the caller's rows, which are
    /// sorted by it, rather than through a map with an entry per site.
    pub fn callsites_with_id(&self, site: SiteId) -> Range<usize> {
        let rows = self.callsites_of_caller(site.caller);
        let in_caller = &self.callsites[rows.clone()];
        let key = (site.block, site.stmt);
        let start = in_caller.partition_point(|r| (r.block, r.stmt) < key);
        let end = start + in_caller[start..].partition_point(|r| (r.block, r.stmt) == key);
        rows.start + start..rows.start + end
    }

    #[inline]
    pub fn uses_field_of(&self, f: Str) -> &[RowIdx] {
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

fn row_for(rows: &mut Vec<FunRow>, by_fq: &mut HashMap<Str, RowIdx>, fq: Str) -> usize {
    *by_fq.entry(fq).or_insert_with(|| {
        rows.push(FunRow::new(fq));
        row_idx(rows.len() - 1)
    }) as usize
}

fn row_idx(i: usize) -> RowIdx {
    RowIdx::try_from(i).expect("a table has more than u32::MAX rows")
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
///
/// Call sites come out in the layout [`ProgramFacts::callsites`] documents: `lowered` names each
/// function once, and a body is walked block by block, statement by statement.
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
                    StatementKind::Load { field, .. }
                    | StatementKind::Store { field, .. }
                    | StatementKind::Update { field, .. }
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
