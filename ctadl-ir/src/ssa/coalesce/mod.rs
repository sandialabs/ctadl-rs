/*!
Copy coalescing for pre-SSA IR.

# Correctness (S18 of `docs/ir-redesign.md`)

Correct under its side condition, which the pass checks: the temporary it merges away is
defined once and read once, in the same block, so its cell is reachable by no other path.
Fusing `t = e; use t` into `use e` then removes the one copy the cell took part in: the labels
`use` saw at `t` were exactly `e`'s, and nothing else could see `t`. The pass asserts the single
use before it deletes a def.

Frontends (the pcode frontend especially) linearize expressions through
single-use temporaries: a binop becomes `t = a, b; x = t`, and copy chains
like `u = t; x = u` are common. Every such temporary costs a statement, an
SSA variable, and downstream datalog facts. This pass fuses the definition
of a variable that is defined exactly once and used exactly once — with the
def and use in the same basic block — into its use, then deletes the def:

- `t = a, b; x = t`          becomes `x = a, b`
- `t = y; f(t, z)`           becomes `f(y, z)`
- `t = y; return t`          becomes `return y`

The pass is conservative:

- Only unversioned [`Variable::Local`]s are candidates, so running it on a
  program already in SSA form (every ref versioned) is a no-op.
- A pending def is invalidated by any intervening write to a variable its
  sources read. Calls are treated as potentially writing through their
  arguments (by-ref semantics), so `t = x; g(x); y = t` does not fuse.
- Defs with multiple sources only fuse into a whole-variable single-source
  `Assign` consumer; pure copies (single source) substitute anywhere an
  `Exp` is read.

Taint-flow semantics are preserved: the fused statement induces exactly the
dataflow edges the def/use pair induced, minus the intermediate hop.
*/
use std::collections::HashMap;

use smallvec::SmallVec;

use crate::index::idx::Idx;
use crate::mir::*;

/// Runs copy coalescing on every function of `program`. Intended to run
/// before SSA conversion; see the module docs.
pub fn coalesce_copies(program: &mut Program) {
    let mut removed = 0usize;
    for (_, f) in program.functions.iter_enumerated_mut() {
        removed += coalesce_function(f);
    }
    log::debug!("coalesce_copies: removed {removed} fused copy statement(s)");
}

/// A candidate def whose fusion into its (single) use is still possible.
struct Pending {
    /// Position of the def statement in the current block.
    stmt_pos: usize,
    /// The def's right-hand side.
    sources: SmallVec<[Exp; 2]>,
    /// Variables read by `sources`; a write to any of these kills the entry.
    src_vars: SmallVec<[Variable; 2]>,
}

/// Runs copy coalescing on one function, returning the number of fused copy
/// statements deleted.
pub fn coalesce_function(function: &mut FunctionData) -> usize {
    if function.blocks.is_empty() {
        return 0;
    }

    // Def/use counts over the whole function. Only variables with exactly one
    // def and one use are candidates.
    let mut defs: HashMap<Variable, u32> = HashMap::new();
    let mut uses: HashMap<Variable, u32> = HashMap::new();
    for (_, block) in function.blocks.iter_enumerated() {
        for stmt in block.statements.iter() {
            for v in stmt.iter_src_var() {
                *uses.entry(v.variable).or_default() += 1;
            }
            for v in stmt.iter_dst_var() {
                *defs.entry(v.variable).or_default() += 1;
            }
        }
        if let Some(term) = block.terminator_opt() {
            for v in term.iter_src_var() {
                *uses.entry(v.variable).or_default() += 1;
            }
        }
    }

    let is_candidate_var = |var: &Variable| {
        matches!(var, Variable::Local(_)) && defs.get(var) == Some(&1) && uses.get(var) == Some(&1)
    };

    let mut removed = 0usize;
    for block in function.blocks.blocks_mut_preserves_cfg().iter_mut() {
        let mut pending: HashMap<Variable, Pending> = HashMap::new();
        // Def statement positions that were fused and must be deleted.
        let mut dead: Vec<usize> = Vec::new();

        let num_stmts = block.statements.len();
        for pos in 0..num_stmts {
            let stmt = block
                .statements
                .get_mut(crate::mir::StatementIdx::new(pos))
                .expect("in range");

            // (a) Try to fuse pending defs into this statement's reads.
            match &mut stmt.kind {
                // A store `y.f = t`: only pure copies substitute, and a value def does not.
                StatementKind::Assign { dest, sources } if !dest.is_pathless() => {
                    for src in sources.iter_mut() {
                        try_subst_exp(src, false, &mut pending, &mut dead);
                    }
                }
                StatementKind::Assign { dest: _, sources } => {
                    // Whole-statement fuse of a whole-variable copy `x = t`:
                    // accepts multi-source defs.
                    let whole_var_copy_of = if sources.len() == 1
                        && let Some(v) = sources[0].variable_ref()
                        && v.version.is_none()
                        && pending.contains_key(&v.variable)
                    {
                        Some(v.variable)
                    } else {
                        None
                    };
                    if let Some(var) = whole_var_copy_of {
                        let p = pending.remove(&var).expect("checked");
                        *sources = p.sources;
                        dead.push(p.stmt_pos);
                    } else {
                        for src in sources.iter_mut() {
                            try_subst_exp(src, true, &mut pending, &mut dead);
                        }
                    }
                }
                StatementKind::CallAssign { args, style, .. } => {
                    for a in args.iter_mut() {
                        try_subst_exp(a, false, &mut pending, &mut dead);
                    }
                    if let CallStyle::FuncPtrCall { callee, .. } = style {
                        try_subst_address(callee, &mut pending, &mut dead);
                    }
                }
                StatementKind::Phi { .. } | StatementKind::Nop => {}
            }

            // (b) Invalidate pending defs whose sources are (possibly)
            // written by this statement. Calls may write through their
            // arguments (by-ref), so treat their reads as writes too.
            if !pending.is_empty() {
                let mut written: SmallVec<[Variable; 4]> =
                    stmt.iter_dst_var().map(|v| v.variable).collect();
                if matches!(stmt.kind, StatementKind::CallAssign { .. }) {
                    written.extend(stmt.iter_src_var().map(|v| v.variable));
                }
                if !written.is_empty() {
                    pending.retain(|_, p| !p.src_vars.iter().any(|sv| written.contains(sv)));
                }
            }

            // (c) Register this statement as a pending def if it qualifies.
            if let StatementKind::Assign { dest, sources } = &stmt.kind
                && dest.is_pathless()
                && let dest = &dest.base
                && dest.version.is_none()
                && is_candidate_var(&dest.variable)
                && !sources.is_empty()
                // A load (a source with a field or `deref`) is left for codegen's load-chain
                // composition, which needs the temporary it defines.
                && sources.iter().all(|e| {
                    e.access_path()
                        .is_none_or(|ap| ap.segments.iter().all(PathSegment::is_offset))
                })
            {
                let mut src_vars: SmallVec<[Variable; 2]> = SmallVec::new();
                let mut ok = true;
                for src in sources.iter() {
                    // Any source that reads a variable (a bare copy or an address expression)
                    // must be tracked, so a later write to it invalidates this pending def.
                    if let Some(v) = src.base_variable() {
                        // Versioned reads shouldn't exist pre-SSA; bail if seen.
                        if v.version.is_some() || v.variable == dest.variable {
                            ok = false;
                            break;
                        }
                        src_vars.push(v.variable);
                    }
                }
                if ok {
                    pending.insert(
                        dest.variable,
                        Pending {
                            stmt_pos: pos,
                            sources: sources.clone(),
                            src_vars,
                        },
                    );
                }
            }
        }

        // Terminator reads (return arguments).
        if let Some(term) = block.terminator.as_mut()
            && let TerminatorKind::Return { args } = &mut term.kind
        {
            for a in args.iter_mut() {
                try_subst_exp(a, false, &mut pending, &mut dead);
            }
        }

        // Delete fused defs. Each defines a temporary with exactly one use, the one it was
        // fused into: the side condition that makes the fusion correct.
        for &pos in &dead {
            let def = &block.statements[crate::mir::StatementIdx::new(pos)];
            for v in def.iter_dst_var() {
                assert_eq!(
                    uses.get(&v.variable),
                    Some(&1),
                    "coalesce fused a def of {v} that has other uses"
                );
            }
        }
        if !dead.is_empty() {
            removed += dead.len();
            dead.sort_unstable();
            let mut dead = dead.into_iter().peekable();
            let old = std::mem::take(&mut block.statements);
            for (i, s) in old.into_iter_inner().enumerate() {
                if dead.peek() == Some(&i) {
                    dead.next();
                    continue;
                }
                block.statements.push_back(s);
            }
        }
    }
    removed
}

/// If `exp` is the (single) read of a pending pure-copy def, substitute the
/// def's source into it and mark the def dead. Only a whole-variable read
/// substitutes (a read of `t.f` keeps `t`), so substitution is a plain
/// variable/const copy: `t = y; use t` becomes `use y`.
///
/// [`Exp::Value`] is valid only as an `Assign` source (`in_assign`), so a `t = value(y)` def
/// substitutes only there, and a `value(t)` read takes only a variable: `value(t)` with `t = y`
/// or `t = value(y)` is `value(y)`, while `value` of an address `y.[8]` has no spelling.
fn try_subst_exp(
    exp: &mut Exp,
    in_assign: bool,
    pending: &mut HashMap<Variable, Pending>,
    dead: &mut Vec<usize>,
) {
    let (Exp::Path(ap) | Exp::Value(ap)) = &*exp else {
        return;
    };
    if !ap.is_pathless() {
        return;
    }
    let vref = &ap.base;
    let is_value = matches!(exp, Exp::Value(_));
    if vref.version.is_some() {
        return;
    }
    let Some(p) = pending.get(&vref.variable) else {
        return;
    };
    // Only pure copies substitute at expression granularity.
    if p.sources.len() != 1 {
        return;
    }
    let new_exp = match (is_value, &p.sources[0]) {
        (true, Exp::Value(y)) => Exp::Value(y.clone()),
        (true, Exp::Path(y)) if y.is_pathless() => Exp::Value(y.clone()),
        (true, _) => return,
        (false, Exp::Value(_)) if !in_assign => return,
        (false, src) => src.clone(),
    };
    let p = pending.remove(&vref.variable).expect("checked");
    dead.push(p.stmt_pos);
    *exp = new_exp;
}

/// [`try_subst_exp`] for an indirect-call callee. Only a variable-copy source
/// substitutes here; the callee's own path is preserved, with its base variable
/// rewritten to the def's source.
fn try_subst_address(
    addr: &mut AccessPath,
    pending: &mut HashMap<Variable, Pending>,
    dead: &mut Vec<usize>,
) {
    if addr.base.version.is_some() {
        return;
    }
    let Some(p) = pending.get(&addr.base.variable) else {
        return;
    };
    if p.sources.len() != 1 {
        return;
    }
    let Some(def_var) = p.sources[0].variable_ref() else {
        return;
    };
    let def_var = def_var.clone();
    let p = pending.remove(&addr.base.variable).expect("checked");
    dead.push(p.stmt_pos);
    addr.base = def_var;
}

#[cfg(test)]
mod tests;
