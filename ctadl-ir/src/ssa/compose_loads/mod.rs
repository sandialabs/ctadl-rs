/*!
Load-chain composition: a read or a store through a load-chain temporary is rewritten onto the
root the temporary stands for.

```text
t1 = x.a          t1 = x.a
t2 = t1.b    ⟹    t2 = x.a.b
t2.c := v         x.a.b.c := v
```

A front end spells a nested access as a chain of one-step loads. This pass puts each read and
store of the chain back at the path it addresses, so a write through a loaded pointer lands on
the object it names rather than on the temporary. It is the last pass before codegen, which
runs it on every function it lowers.

Within one block, a copy `t = v·s` with a single access-path source makes `t` stand for `v·s`,
composed onto what `v` stands for. A store through `t`, and a read through memory from it (a path
with a field, `deref` or `#value`), is rewritten onto what `t` stands for; a read of `t` itself or
of an address `t.[k]` is not. The map is built over the whole block before anything is
rewritten, so a temporary assigned twice stands for its last assignment.

# Correctness (S18 of `docs/ir.md`)

A read is unchanged: `t = v·c` gives every location `t.s` the labels of `v·c·s` (S6), and the
stores through `t` that also put labels at `t.s` are rewritten onto `v·c·s` too. A store is not.
Its labels at `t.s` reach `v·c·s` backward only for a non-empty `s` other than `#value` (S8), so
landing a store directly on `v·c·s` can add flows: a store at `t.#value` reaches `v·c.#value`, and
stores through `t` and through `v·c` merge as S9's do. It also reaches further than the index's
backward rule, which follows whole-variable copies only (the backward row of the approximations
register, `docs/limitations.md`).

# The admissible path set depends on this pass

The admissible path set is built from the paths the program mentions ([`mentioned_paths`] of
each access path codegen lowers), so composition changes it: a composed chain mentions the same
paths as the single access path it spells, except for offsets at a junction of the chain
(`t = v.[1]; t.[2].deref` mentions `[2]` uncomposed and `[3]` composed).
*/
use std::collections::HashMap;

use crate::cir::call::CallStyle;
use crate::cir::*;

#[cfg(test)]
mod tests;

/// Runs load-chain composition on every function of `program`. See the module docs.
pub fn compose_loads(program: &mut Program) {
    let mut rewritten = 0usize;
    for (_, f) in program.functions.iter_enumerated_mut() {
        rewritten += compose_loads_function(f);
    }
    log::debug!("compose_loads: rewrote {rewritten} access path(s)");
}

/// Runs load-chain composition on `f`, returning how many access paths it rewrote.
pub fn compose_loads_function(f: &mut FunctionData) -> usize {
    let mut rewritten = 0;
    for block in f.blocks.iter_mut() {
        rewritten += compose_block(block);
    }
    rewritten
}

/// The root and path each load-chain temporary of a block stands for.
type Captures = HashMap<VariableRef, AccessPath>;

/// `ap` composed onto what its base stands for, or `None` when its base is not a load-chain
/// temporary.
fn composed(captures: &Captures, ap: &AccessPath) -> Option<AccessPath> {
    let cap = captures.get(&ap.base)?;
    Some(AccessPath {
        base: cap.base.clone(),
        segments: path::concat(&cap.segments, &ap.segments)
            .into_iter()
            .collect(),
    })
}

/// True if a read of `ap` goes through memory: its path has a field, `deref` or `#value`.
fn reads_memory(ap: &AccessPath) -> bool {
    !ap.segments.iter().all(PathSegment::is_offset)
}

fn compose_block(block: &mut BasicBlockData) -> usize {
    let mut captures = Captures::new();
    for statement in &block.statements {
        if let StatementKind::Assign { dest, sources } = &statement.kind
            && dest.is_pathless()
            && let [Exp::Path(src)] = sources.as_slice()
        {
            let cap = if src.is_pathless() {
                captures.get(&src.base).cloned()
            } else {
                Some(composed(&captures, src).unwrap_or_else(|| src.clone()))
            };
            if let Some(cap) = cap {
                captures.insert(dest.base.clone(), cap);
            }
        }
    }
    if captures.is_empty() {
        return 0;
    }
    // Each returns how many access paths it rewrote: one or none.
    let store = |ap: &mut AccessPath| match composed(&captures, ap) {
        Some(c) => {
            *ap = c;
            1
        }
        None => 0,
    };
    let read = |ap: &mut AccessPath| if reads_memory(ap) { store(ap) } else { 0 };
    let read_exp = |exp: &mut Exp| exp.access_path_mut().map_or(0, read);
    let mut rewritten = 0;
    for statement in block.statements.iter_mut() {
        match &mut statement.kind {
            StatementKind::Assign { dest, sources } => {
                if !dest.is_pathless() {
                    rewritten += store(dest);
                }
                rewritten += sources.iter_mut().map(read_exp).sum::<usize>();
            }
            StatementKind::CallAssign { args, style, .. } => {
                rewritten += args.iter_mut().map(read_exp).sum::<usize>();
                if let CallStyle::FuncPtrCall { callee, .. } = style {
                    rewritten += read(callee);
                }
            }
            _ => {}
        }
    }
    if let Some(TerminatorKind::Return { args }) = block.terminator.as_mut().map(|t| &mut t.kind) {
        rewritten += args.iter_mut().map(read_exp).sum::<usize>();
    }
    rewritten
}

/// The paths a read or a store of `ap` mentions, for the admissible path set: each offset run
/// between memory steps (fields, `deref` and `#value`), the address left over after the last one,
/// and each prefix of the path that ends at a memory step. See the module docs.
pub fn mentioned_paths(ap: &AccessPath) -> Vec<Vec<PathSegment>> {
    let segments = &ap.segments;
    let mut out = Vec::new();
    let mut run_start = 0;
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_offset() {
            continue;
        }
        out.push(segments[run_start..i].to_vec());
        out.push(segments[..=i].to_vec());
        run_start = i + 1;
    }
    out.push(segments[run_start..].to_vec());
    out
}
