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

# What is a load-chain temporary

Within one block, a whole-variable copy `t = v·s` with a single access-path source makes `t`
stand for `v·s`, composed onto what `v` stands for: a load `t = v.f`, an address copy
`t = v.[k]`, and a plain copy `t = v` (which carries `v`'s capture forward, if it has one) all
qualify. A copy with several sources, a `value(..)` source or a constant does not. The map from
temporaries is built over the whole block before anything is rewritten, and a temporary assigned
twice in a block (which SSA rules out) stands for its last assignment. Nothing crosses a block
boundary.

# What is rewritten

An access path `t·s` whose base is a load-chain temporary becomes `root·c·s`, where `t` stands for
`root·c`, when it is:

- an assignment's destination with a non-empty path (a store);
- an assignment's source, a call argument, a return value or an indirect call's callee, as a
  path or under `value(..)`, whose path has a field, `deref` or `#value` in it: a read through
  memory. A read of an address (`t.[k]`) or of the whole temporary is left alone.

# Correctness (S18 of `docs/ir.md`)

A read is unchanged: `t = v·c` gives every location `t.s` the labels of `v·c·s` (S6), and the
stores through `t` that also put labels at `t.s` are rewritten onto `v·c·s` too. A store is not.
Its labels at `t.s` reach `v·c·s` backward only for a non-empty `s` other than `#value` (S8), so
landing a store directly on `v·c·s` can add flows: a store at `t.#value` reaches `v·c.#value`, and
stores through `t` and through `v·c` merge as S9's do. It also reaches further than the index's
backward rule, which follows whole-variable copies only (the backward row of the approximations
register, `docs/limitations.md`).

# The admissible path set depends on this pass

The analysis keeps only the paths the program mentions, and their one-level concatenations (the
"admissible path set" row of the approximations register). Codegen seeds that set from
[`mentioned_paths`] of each access path it lowers, so composing a chain changes which paths are
mentioned: `x.a.b.c` rather than `t2.c`. That is what makes a path admit the same set whether a
front end writes it as one access path or as a chain of loads, since a composed chain mentions
the same memory paths as the single access path (tested in this module). The offsets differ:
`t = v.[1]; t.[2].deref` mentions the offset `[2]` as a chain and `[3]` composed, and the chain's
own offsets are not mentioned when the chain is written as one path.
*/
use std::collections::HashMap;

use crate::mir::call::CallStyle;
use crate::mir::*;

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
    let mut rewritten = 0;
    let mut rewrite = |ap: &mut AccessPath| {
        if let Some(c) = composed(&captures, ap) {
            *ap = c;
            rewritten += 1;
        }
    };
    let read = |exp: &mut Exp, rewrite: &mut dyn FnMut(&mut AccessPath)| {
        if let Some(ap) = exp.access_path_mut()
            && reads_memory(ap)
        {
            rewrite(ap);
        }
    };
    for statement in block.statements.iter_mut() {
        match &mut statement.kind {
            StatementKind::Assign { dest, sources } => {
                if !dest.is_pathless() {
                    rewrite(dest);
                }
                for src in sources.iter_mut() {
                    read(src, &mut rewrite);
                }
            }
            StatementKind::CallAssign { args, style, .. } => {
                for arg in args.iter_mut() {
                    read(arg, &mut rewrite);
                }
                if let CallStyle::FuncPtrCall { callee, .. } = style
                    && reads_memory(callee)
                {
                    rewrite(callee);
                }
            }
            _ => {}
        }
    }
    if let Some(TerminatorKind::Return { args }) = block.terminator.as_mut().map(|t| &mut t.kind) {
        for arg in args.iter_mut() {
            read(arg, &mut rewrite);
        }
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
