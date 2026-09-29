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

# Which slots are promotable

A slot is reachable in two ways: directly, by a `.deref` load or store on `__stack_top.[k]`, and
through an *escaped* address. An address escapes when `__stack_top` (at some offset `j`) is used
any other way: copied, stored, passed to a call, returned, called through, or loaded with a field
other than `.deref`. The analysis adds offsets together, so an escaped `__stack_top.[j]` plus an
offset `d` reaches slot `j + d`. Promoting such a slot would lose that flow.

How far an escaped address reaches is not known from the IR, so the [`EscapePolicy`] decides.
Distinct offsets are distinct locations in the analysis (it matches offsets exactly), so treating
each offset as its own slot keeps how overlapping accesses of different widths behave today.

The pass leaves a function alone when `__stack_top` is ever assigned, or is used in a way this
pass does not classify (a phi, a param-flow, an update of the frame itself).
*/
use std::collections::{BTreeSet, HashMap};

use internment::ArcIntern;

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
}

impl std::ops::AddAssign for Mem2RegStats {
    fn add_assign(&mut self, o: Self) {
        self.promoted_accesses += o.promoted_accesses;
        self.kept_accesses += o.kept_accesses;
        self.promoted_slots += o.promoted_slots;
        self.skipped_functions += o.skipped_functions;
    }
}

/// Promotes stack slots in every function of `program`. Runs before SSA conversion.
pub fn promote_stack_slots(program: &mut Program, policy: EscapePolicy) -> Mem2RegStats {
    let mut total = Mem2RegStats::default();
    for (_, f) in program.functions.iter_enumerated_mut() {
        total += promote_stack_slots_function(f, policy);
    }
    log::info!("mem2reg({}): {total:?}", policy.tag());
    total
}

/// The sum of an address path's offsets: the slot it names.
fn slot(ap: &AccessPath) -> i64 {
    ap.accesses.offsets.iter().map(|a| a.offset().0).sum()
}

/// If `e` is an address on the frame base, its offset.
fn exp_slot(e: &Exp, st: &ArcIntern<Variable>) -> Option<i64> {
    match e {
        Exp::Variable(v) if v.variable == *st => Some(0),
        Exp::AccessPath(ap) if ap.base.variable == *st => Some(slot(ap)),
        _ => None,
    }
}

/// A use of the frame base.
enum Use {
    /// A `.deref` load or store of this slot.
    Direct(i64),
    /// The address of this slot escapes.
    Escape(i64),
}

/// Classifies every use of `st` in `kind`, pushing each onto `out`. The caller checks the number
/// pushed against the number of `st` reads the statement makes, so a use this misses is caught.
fn classify(kind: &StatementKind, st: &ArcIntern<Variable>, out: &mut Vec<Use>) {
    let is_st = |v: &VariableRef| v.variable == *st;
    let esc = |e: &Exp, out: &mut Vec<Use>| {
        if let Some(k) = exp_slot(e, st) {
            out.push(Use::Escape(k));
        }
    };
    match kind {
        StatementKind::Assign { sources, .. } => {
            for e in sources {
                esc(e, out);
            }
        }
        StatementKind::Load { source, field, .. } if is_st(&source.base) => {
            out.push(if &*field.field == DEREF {
                Use::Direct(slot(source))
            } else {
                Use::Escape(slot(source))
            });
        }
        StatementKind::Store { dest, field, value } => {
            if is_st(&dest.base) {
                out.push(if &*field.field == DEREF {
                    Use::Direct(slot(dest))
                } else {
                    Use::Escape(slot(dest))
                });
            }
            esc(value, out);
        }
        StatementKind::Update { value, .. } => esc(value, out),
        StatementKind::CallAssign { style, args, .. } => {
            for e in args {
                esc(e, out);
            }
            if let CallStyle::FuncPtrCall { callee, .. } = style
                && is_st(&callee.base)
            {
                out.push(Use::Escape(slot(callee)));
            }
        }
        _ => {}
    }
}

/// Promotes the stack slots of one function. See the module documentation.
pub fn promote_stack_slots_function(
    function: &mut FunctionData,
    policy: EscapePolicy,
) -> Mem2RegStats {
    let mut stats = Mem2RegStats::default();
    let Some((st_idx, _)) = function
        .locals
        .iter_enumerated()
        .find(|(_, d)| d.name == STACK_TOP)
    else {
        return stats;
    };
    let st = ArcIntern::new(Variable::Local(st_idx));

    // Pass 1: classify every use of the frame base. Bail on anything unclassified.
    let mut uses: Vec<Use> = Vec::new();
    let mut escapes: BTreeSet<i64> = BTreeSet::new();
    for (_, data) in function.blocks.iter_enumerated() {
        for s in data.iter() {
            if s.iter_dst_var().any(|v| v.variable == st) {
                stats.skipped_functions = 1;
                return stats;
            }
            let reads = s.iter_src_var().filter(|v| v.variable == st).count();
            let before = uses.len();
            classify(&s.kind, &st, &mut uses);
            if uses.len() - before != reads {
                stats.skipped_functions = 1;
                return stats;
            }
        }
        if let Some(t) = data.terminator_opt() {
            let reads = t.iter_src_var().filter(|v| v.variable == st).count();
            let mut n = 0;
            if let TerminatorKind::Return { args } = &t.kind {
                for e in args {
                    if let Some(k) = exp_slot(e, &st) {
                        escapes.insert(k);
                        n += 1;
                    }
                }
            }
            if n != reads {
                stats.skipped_functions = 1;
                return stats;
            }
        }
    }
    let mut direct: BTreeSet<i64> = BTreeSet::new();
    for u in &uses {
        match *u {
            Use::Direct(k) => {
                direct.insert(k);
            }
            Use::Escape(j) => {
                escapes.insert(j);
            }
        }
    }

    // Pass 2: choose promotable slots and give each a local.
    let promotable: HashMap<i64, VariableRef> = direct
        .into_iter()
        .filter(|&k| !escapes.iter().any(|&j| policy.reaches(j, k)))
        .map(|k| {
            let name = if k < 0 {
                format!("__stack_m{}", k.unsigned_abs())
            } else {
                format!("__stack_{k}")
            };
            (k, VariableRef::new_local_idx(function.intern_local(&name)))
        })
        .collect();
    stats.promoted_slots = promotable.len();
    if promotable.is_empty() {
        stats.kept_accesses = uses.iter().filter(|u| matches!(u, Use::Direct(_))).count();
        return stats;
    }

    // Pass 3: rewrite direct accesses of promoted slots into copies.
    let blocks = function.blocks.blocks_mut_preserves_cfg();
    for bb in blocks.indices() {
        for s in blocks[bb].iter_mut() {
            let replacement = match &s.kind {
                StatementKind::Load {
                    dest,
                    source,
                    field,
                } if source.base.variable == st && &*field.field == DEREF => {
                    match promotable.get(&slot(source)) {
                        Some(local) => Some(StatementKind::assign(
                            dest.clone(),
                            [Exp::Variable(local.clone())],
                        )),
                        None => {
                            stats.kept_accesses += 1;
                            None
                        }
                    }
                }
                StatementKind::Store { dest, field, value }
                    if dest.base.variable == st && &*field.field == DEREF =>
                {
                    match promotable.get(&slot(dest)) {
                        Some(local) => Some(StatementKind::assign(local.clone(), [value.clone()])),
                        None => {
                            stats.kept_accesses += 1;
                            None
                        }
                    }
                }
                _ => None,
            };
            if let Some(kind) = replacement {
                s.kind = kind;
                stats.promoted_accesses += 1;
            }
        }
    }
    stats
}
