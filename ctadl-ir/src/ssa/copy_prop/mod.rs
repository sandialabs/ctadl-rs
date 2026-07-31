/*!
Copy propagation and trivial-phi elimination for post-SSA IR.

SSA conversion introduces administrative noise. A pre-SSA copy that survived
[`coalesce_copies`](super::coalesce_copies) because it had several uses becomes
a versioned copy `x_2 = x_1`; forwarding returns through a single exit block
([`complete`](super::transform)) mints `_$ret_2 = a_1` copies; and phi placement
inserts phis that, after the dust settles, merge a value with nothing but itself
or with copies of one value. The classic shape is

```text
x_2 = x_1
x_3 = phi(x_2, x_2)
y_1 = x_3
```

which should read as direct uses of `x_1`. This pass performs that
normalization by computing, for every SSA name, a *representative* and rewriting
all uses to it, then deleting the now-useless defs:

- **Copy propagation.** A pure copy `dest = src` (an [`StatementKind::Assign`]
  with a single [`Exp::Variable`] source, both versioned) makes `dest` an alias
  of `src`.
- **Trivial-phi elimination.** A [`StatementKind::Phi`] whose operands, once
  resolved to their own representatives and with self-references dropped, name a
  single value `u` makes its `dest` an alias of `u`. This subsumes both a
  single-input phi (`x = phi(y)` ⟹ `x` aliases `y`) and a phi whose operands are
  all identical (`x = phi(y, y)` ⟹ `x` aliases `y`), including the loop-header
  form `x = phi(y, x)` where the only non-self operand is `y`.

The two interact — resolving a copy can make a phi trivial, and eliminating a
phi can make a copy point further — so representatives are computed to a fixpoint
over both relations at once (see [`Aliases`]). A use is then rewritten to its
representative, which is by construction a name this pass does *not* delete, so
no reference is left dangling.

The pass is conservative and SSA-only:

- Only versioned refs are aliased, so it is a no-op on non-SSA IR. In particular
  the version-0 anchor assigns that [`transform`](super::transform) seeds for
  parameters and the global heap (`p_0 = p`, `$globals_0 = $globals`) read an
  *unversioned* source and are never treated as copies, so those anchors — and
  the [`StatementKind::ParamFlow`] round-trip that depends on them — are
  preserved.
- Only bare variable copies propagate. Multi-source assigns, address-arithmetic
  ([`Exp::AccessPath`]), constants, loads, and call results are never aliased:
  a phi operand and a copy target must both be plain variables for the rename to
  stay within SSA names.
- A phi that survives (two or more distinct resolved operands) is kept, and
  rewriting its operands to their representatives cannot make it trivial, so a
  single pass to fixpoint suffices.

Taint-flow semantics are preserved: an eliminated copy or trivial phi induced
only the identity edge `dest := src`, and every consumer now reads `src`
directly.
*/
use std::collections::HashMap;

use crate::mir::*;

#[cfg(test)]
mod tests;

/// Runs copy propagation and trivial-phi elimination on every function of
/// `program`. Intended to run *after* SSA conversion; see the module docs.
pub fn propagate_copies(program: &mut Program) {
    let mut removed = 0usize;
    for (_, f) in program.functions.iter_enumerated_mut() {
        removed += propagate_copies_function(f);
    }
    log::debug!("copy_prop: removed {removed} copy/trivial-phi statement(s)");
}

/// The alias relation being solved: `parent[v]` is the value `v` was found
/// equal to (a copy source or a trivial phi's sole operand). Resolving a chain
/// of `parent` links yields a name with no entry — the *representative*.
#[derive(Default)]
struct Aliases {
    parent: HashMap<VariableRef, VariableRef>,
}

impl Aliases {
    /// The representative of `v`: follow `parent` links to the end. The step cap
    /// makes this total even if the relation contains a cycle (an undefined
    /// value defined only in terms of itself), returning some member of it.
    fn resolve(&self, mut v: VariableRef) -> VariableRef {
        let mut steps = 0;
        while let Some(next) = self.parent.get(&v) {
            if steps > self.parent.len() {
                break;
            }
            v = next.clone();
            steps += 1;
        }
        v
    }
}

/// Runs the pass on one function, returning the number of copy / trivial-phi
/// statements deleted.
pub fn propagate_copies_function(function: &mut FunctionData) -> usize {
    if function.blocks.is_empty() {
        return 0;
    }

    // Gather alias candidates. Copies are unconditional aliases and go straight
    // in; phis are conditional (triviality depends on where their operands
    // resolve) and are settled by fixpoint below.
    let mut aliases = Aliases::default();
    let mut phis: Vec<(VariableRef, Vec<VariableRef>)> = Vec::new();
    for (_, block) in function.blocks.iter_enumerated() {
        for stmt in block.statements.iter() {
            match &stmt.kind {
                StatementKind::Assign { dest, sources } if dest.version.is_some() => {
                    if let [Exp::Variable(src)] = sources.as_slice()
                        && src.version.is_some()
                        && *src != *dest
                    {
                        aliases.parent.insert(dest.clone(), src.clone());
                    }
                }
                StatementKind::Phi { dest, operands } if dest.version.is_some() => {
                    phis.push((
                        dest.clone(),
                        operands.iter().map(|(_, v)| v.clone()).collect(),
                    ));
                }
                _ => {}
            }
        }
    }

    // Fixpoint over phis: a phi is trivial when its operands, resolved through
    // the current alias relation and with self-references dropped, name exactly
    // one value. Eliminating one phi (or resolving a copy) can expose another,
    // so repeat until a sweep settles nothing new.
    let mut changed = true;
    while changed {
        changed = false;
        for (dest, operands) in &phis {
            if aliases.parent.contains_key(dest) {
                continue;
            }
            let mut unique: Option<VariableRef> = None;
            let mut trivial = true;
            for op in operands {
                let r = aliases.resolve(op.clone());
                if r == *dest {
                    // Self-reference (directly, or a copy that folds back to
                    // this phi): contributes no distinct value.
                    continue;
                }
                match &unique {
                    None => unique = Some(r),
                    Some(u) if *u == r => {}
                    Some(_) => {
                        trivial = false;
                        break;
                    }
                }
            }
            // `unique == None` means every operand was a self-reference: an
            // undefined value (dead/unreachable phi). Leave it be.
            if trivial && let Some(u) = unique {
                aliases.parent.insert(dest.clone(), u);
                changed = true;
            }
        }
    }

    if aliases.parent.is_empty() {
        return 0;
    }

    // Rewrite every use to its representative. Representatives have no `parent`
    // entry, so their defs are retained and no reference dangles.
    let rewrite = |v: &mut VariableRef| {
        let r = aliases.resolve(v.clone());
        if r != *v {
            *v = r;
        }
    };
    let blocks = function.blocks.blocks_mut_preserves_cfg();
    for block in blocks.iter_mut() {
        for stmt in block.statements.iter_mut() {
            for v in stmt.iter_src_var_mut() {
                rewrite(v);
            }
        }
        if let Some(term) = block.terminator.as_mut() {
            for v in term.iter_src_var_mut() {
                rewrite(v);
            }
        }
    }

    // Delete the aliased defs (the copies and trivial phis). Each defines
    // exactly the aliased variable, which now has no readers.
    let mut removed = 0usize;
    for block in blocks.iter_mut() {
        let has_dead = block.statements.iter().any(|s| is_aliased_def(s, &aliases));
        if !has_dead {
            continue;
        }
        let old = std::mem::take(&mut block.statements);
        for s in old.into_iter_inner() {
            if is_aliased_def(&s, &aliases) {
                removed += 1;
            } else {
                block.statements.push_back(s);
            }
        }
    }
    removed
}

/// Whether `stmt` is a copy or phi whose destination was aliased away, i.e. a
/// def this pass has made dead.
fn is_aliased_def(stmt: &Statement, aliases: &Aliases) -> bool {
    match &stmt.kind {
        StatementKind::Assign { dest, .. } | StatementKind::Phi { dest, .. } => {
            aliases.parent.contains_key(dest)
        }
        _ => false,
    }
}
