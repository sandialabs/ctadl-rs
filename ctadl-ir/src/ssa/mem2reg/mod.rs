/*!
Promotes stack slots to locals before SSA conversion, as LLVM's mem2reg does.

The pcode frontend addresses every stack slot through one variable, the frame base
[`STACK_TOP`]: a slot at frame offset `k` is read by `d = load __stack_top.[k].deref` and written
by `store __stack_top.[k].deref := v`. The analysis treats memory flow-insensitively, so every
load of slot `k` sees every value ever stored to it anywhere in the function. On a large native
function the stack becomes one pool that mixes every value with every other.

This pass gives each promotable slot its own local and rewrites its loads and stores into copies:

```text
d = load __stack_top.[-16].deref         d = __stack_m16
store __stack_top.[-16].deref := v   =>  __stack_m16 = v
```

SSA conversion then versions the new locals like any other, so a load sees only the stores that
reach it.

A frame write may also be a functional update of the frame (see [`super::FrameWrites`]),
`__stack_top = update (__stack_top.[-16].deref := v)`. It is a direct store like any other:
promoting the slot rewrites it into `__stack_m16 = v`, and a slot that stays in memory
keeps its updates, so SSA still versions the frame for the slots left in it.

# Which slots are promotable

A slot is reachable in two ways: directly, by a `.deref` load or store on `__stack_top.[k]`, and
through an *escaped* address. An address escapes when `__stack_top` (at some offset `j`) is used
any other way: copied, stored, passed to a call, returned, called through, or loaded with a field
other than `.deref`. The analysis adds offsets together, so an escaped `__stack_top.[j]` plus an
offset `d` reaches slot `j + d`. Promoting such a slot would lose that flow.

How far an escaped address reaches is not known from the IR, so the [`EscapePolicy`] decides.
Distinct offsets are distinct locations in the analysis (it matches offsets exactly), so treating
each offset as its own slot keeps how overlapping accesses of different widths behave today.

# Full promotion

The slots an escape reaches are the busiest ones in a large function, so they stay mixing pools.
[`Mem2Reg::full_above`] promotes every directly accessed slot of a function with more direct
accesses than that, escapes or not. A direct store then writes only the local, so memory holds
only what callees and copies of the address write. After a call passed an address that reaches
a slot (under the escape policy), the local is reloaded from the slot:

```text
f(__stack_top.[-16])                     f(__stack_top.[-16])
                                     =>  __stack_m16 = load __stack_top.[-16].deref
```

The reload keeps the out-parameter case: a callee writing through the address it was given. What
is lost is whatever reads or writes the slot through the address other than that: a callee
reading what the function stored before the call, a stored or copied address. A function where
such an escape reaches a promoted slot gets an [`EscapeNote`], which names it.

A slot a returned address reaches stays in memory even so. The pcode frontend returns a value
held in a stack slot as that slot's address (`return __stack_top.[4]`), so promoting the slot
would drop the function's return flow. Only small functions do this; the large ones that full
promotion is for do not.
There is no write-back before calls: memory is flow-insensitive, so a value written back before
one call would reach every read of the slot, which mixes the slot again.

[`FullMode`] forces or forbids full promotion for one function, whatever its size.

The pass leaves a function alone when `__stack_top` is assigned other than by a frame update, or
is used in a way this pass does not classify (a phi, a param-flow, an update of something else
that copies the frame).
*/
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::index::idx::Idx;
use crate::mir::call::CallStyle;
use crate::mir::*;

#[cfg(test)]
mod tests;

/// Name of the frame-base local the pcode frontend addresses stack slots through.
pub const STACK_TOP: &str = "__stack_top";

/// The field a stack slot's memory is read and written through.
const DEREF: &str = "deref";

/// How far an escaped frame address `__stack_top.[j]` is assumed to reach. Slots it reaches stay
/// in memory; the rest are promoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapePolicy {
    /// Only slot `j` itself. Loses any flow where a callee reaches past the escaped slot, such
    /// as a field of a stack struct passed by pointer.
    Exact,
    /// Slots `j .. j + n`.
    Window(i64),
    /// Every slot at or above `j`. The only policy that loses no flow the analysis sees today
    /// (short of an escaped address reaching below itself).
    AllAbove,
}

impl EscapePolicy {
    /// Parses `exact`, `window:N` or `above`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(EscapePolicy::Exact),
            "above" => Some(EscapePolicy::AllAbove),
            _ => s
                .strip_prefix("window:")
                .and_then(|n| n.parse().ok())
                .filter(|n: &i64| *n > 0)
                .map(EscapePolicy::Window),
        }
    }

    /// A short name for [`super::Pipeline::tag`].
    pub fn tag(&self) -> String {
        match self {
            EscapePolicy::Exact => "exact".to_string(),
            EscapePolicy::Window(n) => format!("w{n}"),
            EscapePolicy::AllAbove => "above".to_string(),
        }
    }

    /// Whether an address escaped at `j` reaches slot `k`.
    #[inline]
    fn reaches(&self, j: i64, k: i64) -> bool {
        match *self {
            EscapePolicy::Exact => k == j,
            EscapePolicy::Window(n) => j <= k && k < j.saturating_add(n),
            EscapePolicy::AllAbove => j <= k,
        }
    }
}

/// The function-size threshold `full` takes when none is given: above any function of ordinary
/// size, and well below the few that hold most of a native program's `locals` (thousands of
/// direct accesses).
pub const DEFAULT_FULL_THRESHOLD: usize = 1000;

/// The configuration of [`promote_stack_slots`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem2Reg {
    /// How far an escaped address reaches. Under full promotion, which calls reload a slot.
    pub escape: EscapePolicy,
    /// Promote every directly accessed slot of a function with more than this many direct
    /// accesses, escapes or not. See the module documentation. `None` never does.
    pub full_above: Option<usize>,
}

impl From<EscapePolicy> for Mem2Reg {
    fn from(escape: EscapePolicy) -> Self {
        Mem2Reg {
            escape,
            full_above: None,
        }
    }
}

impl Mem2Reg {
    /// Parses an [`EscapePolicy`], or `full` (`window:8`, and full promotion above
    /// [`DEFAULT_FULL_THRESHOLD`]), or `full:N`.
    pub fn parse(s: &str) -> Option<Self> {
        let full = |n| Mem2Reg {
            escape: EscapePolicy::Window(8),
            full_above: Some(n),
        };
        match s {
            "full" => Some(full(DEFAULT_FULL_THRESHOLD)),
            _ => match s.strip_prefix("full:") {
                Some(n) => n.parse().ok().map(full),
                None => EscapePolicy::parse(s).map(Mem2Reg::from),
            },
        }
    }

    /// A short name for [`super::Pipeline::tag`].
    pub fn tag(&self) -> String {
        match self.full_above {
            None => self.escape.tag(),
            Some(n) => format!("{},full>{n}", self.escape.tag()),
        }
    }
}

/// A per-function override of [`Mem2Reg::full_above`], from a model's `modes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullMode {
    /// `mem2reg-full`: promote fully, whatever the function's size.
    Force,
    /// `mem2reg-window`: never promote fully.
    Forbid,
}

/// How an escaped frame address leaves the function's direct accesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EscapeKind {
    /// Passed to a call, or called through.
    CallInput,
    /// Copied into a variable.
    Copied,
    /// Read or written with a field other than `.deref`.
    FieldAccess,
    /// Returned. Full promotion leaves the slots this reaches in memory, so it is never lost.
    Returned,
    /// Stored to memory.
    Stored,
}

impl std::fmt::Display for EscapeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EscapeKind::CallInput => "call input",
            EscapeKind::Copied => "copied",
            EscapeKind::FieldAccess => "field access",
            EscapeKind::Returned => "returned",
            EscapeKind::Stored => "stored",
        })
    }
}

/// A function promoted fully where an escape reaches a promoted slot in a way the reloads do not
/// cover, so flows through those slots may be missed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeNote {
    pub function: String,
    /// How many escapes of each kind reach a promoted slot.
    pub kinds: BTreeMap<EscapeKind, usize>,
    /// A few of those escapes, as `bbN[i]: statement`, for the debug log.
    pub samples: Vec<String>,
}

impl EscapeNote {
    /// The message that names this function in the index's `notifications.sarif`. It holds no
    /// counts or sites, so the same fact reads the same way every time.
    pub fn message(&self) -> String {
        let kinds: Vec<String> = self.kinds.keys().map(ToString::to_string).collect();
        format!(
            "mem2reg full: {}: frame address escapes ({}); flows through those slots may be \
             missed",
            self.function,
            kinds.join(", ")
        )
    }
}

/// What [`promote_stack_slots_function`] did to one function.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mem2RegStats {
    /// Direct loads and stores rewritten into copies.
    pub promoted_accesses: usize,
    /// Direct loads and stores left in memory because an escaped address reaches their slot.
    pub kept_accesses: usize,
    /// Distinct slots given their own local.
    pub promoted_slots: usize,
    /// 1 when the function was left alone because of a use this pass does not classify.
    pub skipped_functions: usize,
    /// 1 when the function was promoted fully.
    pub full_functions: usize,
    /// Of the promoted slots, those an escaped address reaches, which only full promotion
    /// promotes.
    pub escaped_slots: usize,
    /// Reloads inserted after calls.
    pub reloads: usize,
}

impl std::ops::AddAssign for Mem2RegStats {
    fn add_assign(&mut self, o: Self) {
        self.promoted_accesses += o.promoted_accesses;
        self.kept_accesses += o.kept_accesses;
        self.promoted_slots += o.promoted_slots;
        self.skipped_functions += o.skipped_functions;
        self.full_functions += o.full_functions;
        self.escaped_slots += o.escaped_slots;
        self.reloads += o.reloads;
    }
}

/// What [`promote_stack_slots_with`] did to a program.
#[derive(Debug, Default, Clone)]
pub struct Mem2RegReport {
    pub stats: Mem2RegStats,
    /// One per function promoted fully where flows may be missed, in program order.
    pub notes: Vec<EscapeNote>,
}

/// Promotes stack slots in every function of `program`. Runs before SSA conversion.
pub fn promote_stack_slots(program: &mut Program, config: impl Into<Mem2Reg>) -> Mem2RegStats {
    promote_stack_slots_with(program, config.into(), &HashMap::new()).stats
}

/// [`promote_stack_slots`], with full promotion forced or forbidden for the functions `modes`
/// names.
pub fn promote_stack_slots_with(
    program: &mut Program,
    config: Mem2Reg,
    modes: &HashMap<String, FullMode>,
) -> Mem2RegReport {
    let mut report = Mem2RegReport::default();
    for (_, f) in program.functions.iter_enumerated_mut() {
        let mode = modes.get(&f.name).copied();
        let (stats, note) = promote_function(f, config, mode);
        report.stats += stats;
        report.notes.extend(note.inspect(log_escape_note));
    }
    log::info!("mem2reg({}): {:?}", config.tag(), report.stats);
    report
}

/// Logs a note with what its message leaves out: the count of each kind and sample sites.
pub(super) fn log_escape_note(note: &EscapeNote) {
    log::debug!(
        "{}: {:?}; e.g. {}",
        note.message(),
        note.kinds,
        note.samples.join("; ")
    );
}

/// The sum of an address path's offsets: the slot it names.
fn slot(ap: &AccessPath) -> i64 {
    ap.accesses.offsets.iter().map(|a| a.offset().0).sum()
}

/// If `e` is an address on the frame base, its offset.
fn exp_slot(e: &Exp, st: Variable) -> Option<i64> {
    match e {
        Exp::Variable(v) if v.variable == st => Some(0),
        Exp::AccessPath(ap) if ap.base.variable == st => Some(slot(ap)),
        _ => None,
    }
}

/// If `kind` is a frame update, `st = update (st.[k].deref := v)`, its slot `k`.
fn frame_update_slot(kind: &StatementKind, st: Variable) -> Option<i64> {
    match kind {
        StatementKind::Update {
            dest,
            source,
            field,
            ..
        } if dest.base.variable == st && source.variable == st && &*field.field == DEREF => {
            Some(slot(dest))
        }
        _ => None,
    }
}

/// A use of the frame base.
enum Use {
    /// A `.deref` load (`false`) or store (`true`) of this slot.
    Direct(i64, bool),
    /// The address of this slot escapes.
    Escape(i64, EscapeKind),
}

/// Classifies every use of `st` in `kind`, pushing each onto `out`. The caller checks the number
/// pushed against the number of `st` reads the statement makes, so a use this misses is caught.
fn classify(kind: &StatementKind, st: Variable, out: &mut Vec<Use>) {
    let is_st = |v: &VariableRef| v.variable == st;
    let esc = |e: &Exp, how: EscapeKind, out: &mut Vec<Use>| {
        if let Some(k) = exp_slot(e, st) {
            out.push(Use::Escape(k, how));
        }
    };
    match kind {
        StatementKind::Assign { sources, .. } => {
            for e in sources {
                esc(e, EscapeKind::Copied, out);
            }
        }
        StatementKind::Load { source, field, .. } if is_st(&source.base) => {
            out.push(if &*field.field == DEREF {
                Use::Direct(slot(source), false)
            } else {
                Use::Escape(slot(source), EscapeKind::FieldAccess)
            });
        }
        StatementKind::Store { dest, field, value } => {
            if is_st(&dest.base) {
                out.push(if &*field.field == DEREF {
                    Use::Direct(slot(dest), true)
                } else {
                    Use::Escape(slot(dest), EscapeKind::FieldAccess)
                });
            }
            esc(value, EscapeKind::Stored, out);
        }
        StatementKind::Update { value, .. } => {
            if let Some(k) = frame_update_slot(kind, st) {
                out.push(Use::Direct(k, true));
            }
            esc(value, EscapeKind::Stored, out);
        }
        StatementKind::CallAssign { .. } => call_escapes(kind, st, |k| {
            out.push(Use::Escape(k, EscapeKind::CallInput))
        }),
        _ => {}
    }
}

/// Calls `f` with each frame address a call is passed or calls through.
fn call_escapes(kind: &StatementKind, st: Variable, mut f: impl FnMut(i64)) {
    if let StatementKind::CallAssign { style, args, .. } = kind {
        for e in args {
            if let Some(k) = exp_slot(e, st) {
                f(k);
            }
        }
        if let CallStyle::FuncPtrCall { callee, .. } = style
            && callee.base.variable == st
        {
            f(slot(callee));
        }
    }
}

/// The local a promoted slot is given.
fn slot_local_name(k: i64) -> String {
    if k < 0 {
        format!("__stack_m{}", k.unsigned_abs())
    } else {
        format!("__stack_{k}")
    }
}

/// Promotes the stack slots of one function. See the module documentation.
pub fn promote_stack_slots_function(
    function: &mut FunctionData,
    config: impl Into<Mem2Reg>,
) -> Mem2RegStats {
    promote_function(function, config.into(), None).0
}

/// [`promote_stack_slots_function`], with `mode` overriding [`Mem2Reg::full_above`], and the
/// function's [`EscapeNote`] when it has one.
pub fn promote_function(
    function: &mut FunctionData,
    config: Mem2Reg,
    mode: Option<FullMode>,
) -> (Mem2RegStats, Option<EscapeNote>) {
    let policy = config.escape;
    let mut stats = Mem2RegStats::default();
    let Some((st_idx, _)) = function
        .locals
        .iter_enumerated()
        .find(|(_, d)| d.name == STACK_TOP)
    else {
        return (stats, None);
    };
    let st = Variable::Local(st_idx);

    // Pass 1: classify every use of the frame base. Bail on anything unclassified.
    let mut uses: Vec<Use> = Vec::new();
    // Each escape, with where it is, for the note.
    let mut escapes: Vec<(i64, EscapeKind, BasicBlockIdx, usize)> = Vec::new();
    for (bb, data) in function.blocks.iter_enumerated() {
        for (i, s) in data.iter().enumerate() {
            if s.iter_dst_var().any(|v| v.variable == st)
                && frame_update_slot(&s.kind, st).is_none()
            {
                stats.skipped_functions = 1;
                return (stats, None);
            }
            let reads = s.iter_src_var().filter(|v| v.variable == st).count();
            let before = uses.len();
            classify(&s.kind, st, &mut uses);
            if uses.len() - before != reads {
                stats.skipped_functions = 1;
                return (stats, None);
            }
            for u in &uses[before..] {
                if let Use::Escape(j, how) = *u {
                    escapes.push((j, how, bb, i));
                }
            }
        }
        if let Some(t) = data.terminator_opt() {
            let reads = t.iter_src_var().filter(|v| v.variable == st).count();
            let mut n = 0;
            if let TerminatorKind::Return { args } = &t.kind {
                for e in args {
                    if let Some(k) = exp_slot(e, st) {
                        escapes.push((k, EscapeKind::Returned, bb, data.statements.len()));
                        n += 1;
                    }
                }
            }
            if n != reads {
                stats.skipped_functions = 1;
                return (stats, None);
            }
        }
    }
    let mut direct: BTreeSet<i64> = BTreeSet::new();
    let mut stored: HashSet<i64> = HashSet::new();
    for u in &uses {
        if let Use::Direct(k, is_store) = *u {
            direct.insert(k);
            if is_store {
                stored.insert(k);
            }
        }
    }
    let direct_accesses = uses.iter().filter(|u| matches!(u, Use::Direct(..))).count();
    let full = match mode {
        Some(FullMode::Force) => true,
        Some(FullMode::Forbid) => false,
        None => config.full_above.is_some_and(|n| direct_accesses > n),
    };

    // Pass 2: choose promotable slots and give each a local. Under full promotion a slot an
    // escape reaches is promoted too, unless a returned address reaches it (see the module
    // documentation), or its local exists already: then this pass has run on the function
    // before, and the slot's remaining direct accesses are its reloads.
    let existing: HashSet<String> = if full {
        function
            .locals
            .iter_enumerated()
            .filter(|(_, d)| d.name.starts_with("__stack_"))
            .map(|(_, d)| d.name.clone())
            .collect()
    } else {
        HashSet::new()
    };
    let mut promotable: HashMap<i64, VariableRef> = HashMap::new();
    // The promoted slots an escape reaches, which calls reload.
    let mut escaped: BTreeMap<i64, VariableRef> = BTreeMap::new();
    for k in direct {
        let name = slot_local_name(k);
        let reached = escapes.iter().any(|&(j, ..)| policy.reaches(j, k));
        let returned = escapes
            .iter()
            .any(|&(j, how, ..)| how == EscapeKind::Returned && policy.reaches(j, k));
        if reached && (!full || returned || existing.contains(&name)) {
            continue;
        }
        let local = VariableRef::new_local_idx(function.intern_local(&name));
        if reached {
            escaped.insert(k, local.clone());
        }
        promotable.insert(k, local);
    }
    stats.promoted_slots = promotable.len();
    stats.escaped_slots = escaped.len();
    stats.full_functions = usize::from(full);
    if promotable.is_empty() {
        stats.kept_accesses = direct_accesses;
        return (stats, None);
    }

    // The escapes that reach a promoted slot in a way the reloads do not cover: a call reads a
    // slot the function stores to directly, and anything else that holds the address may read
    // or write it.
    let mut note: Option<EscapeNote> = None;
    for &(j, how, bb, i) in &escapes {
        let lost = escaped.keys().any(|&k| {
            policy.reaches(j, k) && (how != EscapeKind::CallInput || stored.contains(&k))
        });
        if !lost {
            continue;
        }
        let note = note.get_or_insert_with(|| EscapeNote {
            function: function.name.clone(),
            kinds: BTreeMap::new(),
            samples: Vec::new(),
        });
        *note.kinds.entry(how).or_default() += 1;
        if note.samples.len() < 5 {
            let data = &function.blocks[bb];
            let site = match data.statements.get(StatementIdx::new(i)) {
                Some(s) => s.kind.to_string(),
                None => data
                    .terminator_opt()
                    .map(|t| t.kind.to_string())
                    .unwrap_or_default(),
            };
            note.samples.push(format!("{bb:?}[{i}]: {site}"));
        }
    }

    // Pass 3: rewrite direct accesses of promoted slots into copies, and reload the escaped
    // ones after each call that reaches them.
    let st_ref = VariableRef::new_local_idx(st_idx);
    let deref = FieldRef::symbol(DEREF);
    let blocks = function.blocks.blocks_mut_preserves_cfg();
    for bb in blocks.indices() {
        let old = std::mem::take(&mut blocks[bb].statements);
        for mut s in old.into_iter_inner() {
            let mut reload: BTreeSet<i64> = BTreeSet::new();
            match &s.kind {
                StatementKind::Load {
                    dest,
                    source,
                    field,
                } if source.base.variable == st && &*field.field == DEREF => {
                    match promotable.get(&slot(source)) {
                        Some(local) => {
                            s.kind =
                                StatementKind::assign(dest.clone(), [Exp::Variable(local.clone())]);
                            stats.promoted_accesses += 1;
                        }
                        None => stats.kept_accesses += 1,
                    }
                }
                StatementKind::Store { dest, field, value }
                    if dest.base.variable == st && &*field.field == DEREF =>
                {
                    match promotable.get(&slot(dest)) {
                        Some(local) => {
                            s.kind = StatementKind::assign(local.clone(), [value.clone()]);
                            stats.promoted_accesses += 1;
                        }
                        None => stats.kept_accesses += 1,
                    }
                }
                StatementKind::Update { value, .. } => {
                    if let Some(k) = frame_update_slot(&s.kind, st) {
                        match promotable.get(&k) {
                            Some(local) => {
                                s.kind = StatementKind::assign(local.clone(), [value.clone()]);
                                stats.promoted_accesses += 1;
                            }
                            None => stats.kept_accesses += 1,
                        }
                    }
                }
                StatementKind::CallAssign { .. } if !escaped.is_empty() => {
                    call_escapes(&s.kind, st, |j| {
                        reload.extend(escaped.keys().filter(|&&k| policy.reaches(j, k)));
                    });
                }
                _ => {}
            }
            let source_info = s.source_info;
            blocks[bb].statements.push_back(s);
            for k in reload {
                let path = AccessPath::new(
                    st_ref.clone(),
                    (k != 0).then_some(OffsetAccess::Offset(Offset(k))),
                );
                blocks[bb].statements.push_back(Statement {
                    source_info,
                    kind: StatementKind::load(escaped[&k].clone(), path, deref.clone()),
                });
                stats.reloads += 1;
            }
        }
    }
    (stats, note)
}
