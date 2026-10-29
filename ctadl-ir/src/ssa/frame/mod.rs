/*!
Coarsens the frame's functional updates into stores, before SSA.

The pcode frontend writes a stack slot as a functional update of the frame base [`STACK_TOP`]:

```text
__stack_top = update (__stack_top.[k].deref := v)
```

An `Update` defines `__stack_top`, so SSA versions the frame: each write makes a new version, and
each load and call reads the version that reaches it. Coarsening drops the definition:

```text
__stack_top = update (__stack_top.[k].deref := v)   =>   store __stack_top.[k].deref := v
```

which leaves one frame for the whole function, where every load of a slot sees every store to it.
It must run before SSA, after which the versions are separate variables, and before mem2reg, so
that mem2reg sees stores.

Only updates of the frame by itself are coarsened. A write through a copy of the frame address
(`%stack = %__stack_top`, then `store %stack.[k].deref`) is a `Store` to begin with.
*/
use crate::mir::*;

use super::STACK_TOP;

#[cfg(test)]
mod tests;

/// How the frame's writes reach SSA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameWrites {
    /// Coarsen every frame update into a store (see the module documentation).
    Store,
    /// Keep frame updates, so SSA versions the frame.
    #[default]
    Update,
}

impl FrameWrites {
    /// Parses `store` or `update`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "store" => Some(FrameWrites::Store),
            "update" => Some(FrameWrites::Update),
            _ => None,
        }
    }

    /// A short name for [`super::Pipeline::tag`].
    pub fn tag(&self) -> &'static str {
        match self {
            FrameWrites::Store => "store",
            FrameWrites::Update => "update",
        }
    }
}

/// Rewrites every frame update of `function` into a store, returning how many it rewrote.
pub fn coarsen_frame_updates_function(function: &mut FunctionData) -> usize {
    let Some((st_idx, _)) = function
        .locals
        .iter_enumerated()
        .find(|(_, d)| d.name == STACK_TOP)
    else {
        return 0;
    };
    let st = Variable::Local(st_idx);
    let mut n = 0;
    for data in function.blocks.blocks_mut_preserves_cfg().iter_mut() {
        for s in data.statements.iter_mut() {
            let StatementKind::Update { dest, source, .. } = &s.kind else {
                continue;
            };
            if dest.base.variable != st || source.variable != st {
                continue;
            }
            let StatementKind::Update {
                dest, field, value, ..
            } = std::mem::replace(&mut s.kind, StatementKind::Nop)
            else {
                unreachable!()
            };
            s.kind = StatementKind::Store { dest, field, value };
            n += 1;
        }
    }
    n
}

/// [`coarsen_frame_updates_function`] on every function of `program`.
pub fn coarsen_frame_updates(program: &mut Program) -> usize {
    program
        .functions
        .iter_enumerated_mut()
        .map(|(_, f)| coarsen_frame_updates_function(f))
        .sum()
}
