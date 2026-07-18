/*!
Dead-temporary elimination for pre-SSA IR.

Frontends emit temporaries that are assigned but never read: a computed value
that a later rewrite orphaned, a call result nobody consumes, or the tail of a
copy chain whose only reader was itself dead. Each such temporary costs a
statement, an SSA variable, and downstream datalog facts, and — unlike a
single-use copy — copy coalescing never touches it, because coalescing fuses a
def into its (one) use and a dead temp has no use to fuse into.

This pass deletes the defining statement of any [`Variable::Local`] that has
zero uses across its function, provided the statement's only effect is that
definition:

- `t = a, b;`   with `t` unread   is deleted
- `t = y.f;`    (a [`StatementKind::Load`]) with `t` unread is deleted

Deletion is a fixpoint: removing `x = a, b` drops the reads of `a` and `b`, so
a temp whose sole reader was `x` becomes dead in turn and is removed on the same
run. A copy chain feeding a dead root collapses entirely.

The pass is conservative:

- Only unversioned [`Variable::Local`]s are candidates, so running it on a
  program already in SSA form (every ref versioned) is a no-op. Intended to run
  before SSA conversion, ahead of [`coalesce_copies`](super::coalesce_copies).
- Only side-effect-free defs are removed. A [`StatementKind::Assign`] or
  [`StatementKind::Load`] merely names a value, so dropping an unread one is
  safe. A [`StatementKind::CallAssign`] with unread `rets` is *kept*: the call
  itself may have effects. [`StatementKind::Store`] defines no variable and is
  never a candidate.

Taint-flow semantics are preserved: a variable read by nothing induces no
dataflow edge, so deleting its def removes only edges that had no sink.
*/
use std::collections::{HashMap, HashSet};

use internment::ArcIntern;
use smallvec::SmallVec;

use crate::index::idx::Idx;
use crate::mir::*;

/// Runs dead-temporary elimination on every function of `program`. Intended to
/// run before SSA conversion; see the module docs.
pub fn eliminate_dead_temps(program: &mut Program) {
    for (_, f) in program.functions.iter_enumerated_mut() {
        eliminate_dead_temps_function(f);
    }
}

/// If `kind` is a side-effect-free def of an unversioned local — the only kind
/// of statement this pass may delete — returns that local; otherwise `None`.
fn removable_dest(kind: &StatementKind) -> Option<&ArcIntern<Variable>> {
    let dest = match kind {
        StatementKind::Assign { dest, .. } => dest,
        StatementKind::Load { dest, .. } => dest,
        _ => return None,
    };
    (dest.version.is_none() && matches!(dest.variable.as_ref(), Variable::Local(_)))
        .then(|| &dest.variable)
}

pub fn eliminate_dead_temps_function(function: &mut FunctionData) {
    if function.blocks.is_empty() {
        return;
    }

    // Use counts over the whole function, plus the location of every removable
    // def keyed by the variable it defines. Terminator reads (return arguments)
    // count as uses so a returned temp is never considered dead.
    let mut uses: HashMap<ArcIntern<Variable>, u32> = HashMap::new();
    let mut def_sites: HashMap<ArcIntern<Variable>, SmallVec<[(BasicBlockIdx, usize); 2]>> =
        HashMap::new();
    for (bb, block) in function.blocks.iter_enumerated() {
        for (pos, stmt) in block.statements.iter().enumerate() {
            for v in stmt.iter_src_var() {
                *uses.entry(v.variable.clone()).or_default() += 1;
            }
            if let Some(var) = removable_dest(&stmt.kind) {
                def_sites.entry(var.clone()).or_default().push((bb, pos));
            }
        }
        if let Some(term) = block.terminator_opt() {
            for v in term.iter_src_var() {
                *uses.entry(v.variable.clone()).or_default() += 1;
            }
        }
    }

    // Worklist of def statements to delete. Seed it with every removable def
    // whose variable is read nowhere.
    let mut dead: HashSet<(BasicBlockIdx, usize)> = HashSet::new();
    let mut worklist: Vec<(BasicBlockIdx, usize)> = Vec::new();
    for (var, sites) in &def_sites {
        if uses.get(var).copied().unwrap_or(0) == 0 {
            for &site in sites {
                if dead.insert(site) {
                    worklist.push(site);
                }
            }
        }
    }

    // Propagate: deleting a def drops the uses it made, so a variable that
    // thereby reaches zero uses makes its own removable def(s) dead in turn.
    while let Some((bb, pos)) = worklist.pop() {
        let read: SmallVec<[ArcIntern<Variable>; 4]> = function.blocks[bb].statements
            [StatementIdx::new(pos)]
        .iter_src_var()
        .map(|v| v.variable.clone())
        .collect();
        for var in read {
            let count = uses.get_mut(&var).expect("use was counted");
            *count -= 1;
            if *count == 0 {
                for &site in def_sites.get(&var).into_iter().flatten() {
                    if dead.insert(site) {
                        worklist.push(site);
                    }
                }
            }
        }
    }

    if dead.is_empty() {
        return;
    }

    // Rebuild each block that lost a statement, dropping the dead positions.
    // Deletion doesn't change the CFG, so the cache is preserved.
    let mut by_block: HashMap<BasicBlockIdx, Vec<usize>> = HashMap::new();
    for (bb, pos) in dead {
        by_block.entry(bb).or_default().push(pos);
    }
    let blocks = function.blocks.blocks_mut_preserves_cfg();
    for (bb, mut positions) in by_block {
        positions.sort_unstable();
        let mut positions = positions.into_iter().peekable();
        let block = &mut blocks[bb];
        let old = std::mem::take(&mut block.statements);
        for (i, s) in old.into_iter_inner().enumerate() {
            if positions.peek() == Some(&i) {
                positions.next();
                continue;
            }
            block.statements.push_back(s);
        }
    }
}
