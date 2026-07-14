/*!
Copy coalescing for pre-SSA IR.

Frontends (the pcode frontend especially) linearize expressions through
single-use temporaries: a binop becomes `t = a, b; x = t`, and copy chains
like `u = t; x = u` are common. Every such temporary costs a statement, an
SSA variable, and downstream datalog facts. This pass fuses the definition
of a variable that is defined exactly once and used exactly once — with the
def and use in the same basic block — into its use, then deletes the def:

- `t = a, b; x = t`          becomes `x = a, b`
- `t = y.f; x = t.g`         becomes `x = y.f.g`
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

use internment::ArcIntern;
use smallvec::SmallVec;

use crate::index::idx::Idx;
use crate::mir::*;

/// Runs copy coalescing on every function of `program`. Intended to run
/// before SSA conversion; see the module docs.
pub fn coalesce_copies(program: &mut Program) {
    for (_, f) in program.functions.iter_enumerated_mut() {
        coalesce_function(f);
    }
}

/// A candidate def whose fusion into its (single) use is still possible.
struct Pending {
    /// Position of the def statement in the current block.
    stmt_pos: usize,
    /// The def's right-hand side.
    sources: SmallVec<[Exp; 2]>,
    /// Variables read by `sources`; a write to any of these kills the entry.
    src_vars: SmallVec<[ArcIntern<Variable>; 2]>,
}

pub fn coalesce_function(function: &mut FunctionData) {
    if function.blocks.is_empty() {
        return;
    }

    // Def/use counts over the whole function. Only variables with exactly one
    // def and one use are candidates.
    let mut defs: HashMap<ArcIntern<Variable>, u32> = HashMap::new();
    let mut uses: HashMap<ArcIntern<Variable>, u32> = HashMap::new();
    for (_, block) in function.blocks.iter_enumerated() {
        for stmt in block.statements.iter() {
            for v in stmt.iter_src_var() {
                *uses.entry(v.variable.clone()).or_default() += 1;
            }
            for v in stmt.iter_dst_var() {
                *defs.entry(v.variable.clone()).or_default() += 1;
            }
        }
        if let Some(term) = block.terminator_opt() {
            for v in term.iter_src_var() {
                *uses.entry(v.variable.clone()).or_default() += 1;
            }
        }
    }

    let is_candidate_var = |var: &ArcIntern<Variable>| {
        matches!(var.as_ref(), Variable::Local(_))
            && defs.get(var) == Some(&1)
            && uses.get(var) == Some(&1)
    };

    for block in function.blocks.blocks_mut_preserves_cfg().iter_mut() {
        let mut pending: HashMap<ArcIntern<Variable>, Pending> = HashMap::new();
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
                StatementKind::Assign { dest: _, sources } => {
                    // Whole-statement fuse of a whole-variable copy `x = t`:
                    // accepts multi-source defs.
                    let whole_var_copy_of = if sources.len() == 1
                        && let Exp::Variable(v) = &sources[0]
                        && v.version.is_none()
                        && pending.contains_key(&v.variable)
                    {
                        Some(v.variable.clone())
                    } else {
                        None
                    };
                    if let Some(var) = whole_var_copy_of {
                        let p = pending.remove(&var).expect("checked");
                        *sources = p.sources;
                        dead.push(p.stmt_pos);
                    } else {
                        for src in sources.iter_mut() {
                            try_subst_exp(src, &mut pending, &mut dead);
                        }
                    }
                }
                StatementKind::Store { value, .. } => {
                    try_subst_exp(value, &mut pending, &mut dead);
                }
                StatementKind::Load { .. } => {}
                StatementKind::CallAssign { args, style, .. } => {
                    for a in args.iter_mut() {
                        try_subst_exp(a, &mut pending, &mut dead);
                    }
                    if let CallStyle::FuncPtrCall { callee, .. } = style {
                        try_subst_access_path(callee, &mut pending, &mut dead);
                    }
                }
                StatementKind::Phi { .. }
                | StatementKind::ParamFlow { .. }
                | StatementKind::Nop => {}
            }

            // (b) Invalidate pending defs whose sources are (possibly)
            // written by this statement. Calls may write through their
            // arguments (by-ref), so treat their reads as writes too.
            if !pending.is_empty() {
                let mut written: SmallVec<[ArcIntern<Variable>; 4]> =
                    stmt.iter_dst_var().map(|v| v.variable.clone()).collect();
                if matches!(stmt.kind, StatementKind::CallAssign { .. }) {
                    written.extend(stmt.iter_src_var().map(|v| v.variable.clone()));
                }
                if !written.is_empty() {
                    pending.retain(|_, p| !p.src_vars.iter().any(|sv| written.contains(sv)));
                }
            }

            // (c) Register this statement as a pending def if it qualifies.
            if let StatementKind::Assign { dest, sources } = &stmt.kind
                && dest.version.is_none()
                && is_candidate_var(&dest.variable)
                && !sources.is_empty()
            {
                let mut src_vars: SmallVec<[ArcIntern<Variable>; 2]> = SmallVec::new();
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
                        src_vars.push(v.variable.clone());
                    }
                }
                if ok {
                    pending.insert(
                        dest.variable.clone(),
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
                try_subst_exp(a, &mut pending, &mut dead);
            }
        }

        // Delete fused defs.
        if !dead.is_empty() {
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
}

/// If `exp` is the (single) read of a pending pure-copy def, substitute the
/// def's source into it and mark the def dead. Since a field read is no longer
/// expressible as an [`Exp`] (field reads are [`StatementKind::Load`]s, which
/// this pass leaves untouched), substitution is a plain variable/const copy:
/// `t = y; use t` becomes `use y`.
fn try_subst_exp(
    exp: &mut Exp,
    pending: &mut HashMap<ArcIntern<Variable>, Pending>,
    dead: &mut Vec<usize>,
) {
    let Exp::Variable(vref) = exp else {
        return;
    };
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
    let new_exp = p.sources[0].clone();
    let p = pending.remove(&vref.variable).expect("checked");
    dead.push(p.stmt_pos);
    *exp = new_exp;
}

/// [`try_subst_exp`] for a bare [`AccessPath`] read position (indirect-call
/// callee). Only a variable-copy source substitutes here; the callee's own
/// field path is preserved, with its base variable rewritten to the def's source.
fn try_subst_access_path(
    ap: &mut AccessPath,
    pending: &mut HashMap<ArcIntern<Variable>, Pending>,
    dead: &mut Vec<usize>,
) {
    if ap.variable_ref.version.is_some() {
        return;
    }
    let Some(p) = pending.get(&ap.variable_ref.variable) else {
        return;
    };
    if p.sources.len() != 1 {
        return;
    }
    let Exp::Variable(def_var) = &p.sources[0] else {
        return;
    };
    let def_var = def_var.clone();
    let p = pending.remove(&ap.variable_ref.variable).expect("checked");
    dead.push(p.stmt_pos);
    ap.variable_ref = def_var;
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn local(name: &str) -> VariableRef {
        VariableRef::new_local(name.to_string())
    }

    fn read(name: &str) -> Exp {
        Exp::Variable(local(name))
    }

    fn assign(dest: &str, sources: Vec<Exp>) -> Statement {
        Statement::new_kind(StatementKind::assign(local(dest), sources))
    }

    /// Builds a single-block function with the given statements and a
    /// no-arg return terminator.
    fn one_block_function(statements: Vec<Statement>) -> FunctionData {
        let mut f = FunctionData::default();
        let mut block = BasicBlockData::new(Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![],
        })));
        for s in statements {
            block.statements.push_back(s);
        }
        f.blocks.push(block);
        f
    }

    fn block_kinds(f: &FunctionData) -> Vec<&StatementKind> {
        f.blocks[BasicBlockIdx::ZERO]
            .statements
            .iter()
            .map(|s| &s.kind)
            .collect()
    }

    #[test]
    fn fuses_binop_temp_into_whole_var_copy() {
        // t = a, b; x = t  =>  x = a, b
        let mut f = one_block_function(vec![
            assign("t", vec![read("a"), read("b")]),
            assign("x", vec![read("t")]),
        ]);
        coalesce_function(&mut f);
        let kinds = block_kinds(&f);
        assert_eq!(kinds.len(), 1);
        let StatementKind::Assign { dest, sources } = kinds[0] else {
            panic!("expected assign");
        };
        assert_eq!(dest, &local("x"));
        assert_eq!(sources.as_slice(), &[read("a"), read("b")]);
    }

    #[test]
    fn collapses_copy_chains() {
        // t0 = a, b; t1 = t0; x = t1  =>  x = a, b
        let mut f = one_block_function(vec![
            assign("t0", vec![read("a"), read("b")]),
            assign("t1", vec![read("t0")]),
            assign("x", vec![read("t1")]),
        ]);
        coalesce_function(&mut f);
        let kinds = block_kinds(&f);
        assert_eq!(kinds.len(), 1);
        let StatementKind::Assign { dest, sources } = kinds[0] else {
            panic!("expected assign");
        };
        assert_eq!(dest, &local("x"));
        assert_eq!(sources.as_slice(), &[read("a"), read("b")]);
    }

    #[test]
    fn no_fusion_when_temp_used_twice() {
        // t = a; x = t; y = t  =>  unchanged
        let mut f = one_block_function(vec![
            assign("t", vec![read("a")]),
            assign("x", vec![read("t")]),
            assign("y", vec![read("t")]),
        ]);
        coalesce_function(&mut f);
        assert_eq!(block_kinds(&f).len(), 3);
    }

    #[test]
    fn no_fusion_across_write_to_source() {
        // t = a; a = b; x = t  =>  unchanged (a changed between def and use)
        let mut f = one_block_function(vec![
            assign("t", vec![read("a")]),
            assign("a", vec![read("b")]),
            assign("x", vec![read("t")]),
        ]);
        coalesce_function(&mut f);
        assert_eq!(block_kinds(&f).len(), 3);
    }

    #[test]
    fn substitutes_pure_copy_into_call_args() {
        // t = y; f(t)  =>  f(y)
        let mut f = one_block_function(vec![
            assign("t", vec![read("y")]),
            Statement::new_kind(StatementKind::CallAssign {
                style: CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(thin_vec::thin_vec!["f".to_string()]),
                },
                rets: thin_vec::ThinVec::new(),
                args: thin_vec::thin_vec![read("t")],
            }),
        ]);
        coalesce_function(&mut f);
        let kinds = block_kinds(&f);
        assert_eq!(kinds.len(), 1);
        let StatementKind::CallAssign { args, .. } = kinds[0] else {
            panic!("expected call");
        };
        assert_eq!(args.as_slice(), &[read("y")]);
    }

    #[test]
    fn no_fusion_when_call_may_write_source() {
        // t = x; g(x); y = t  =>  unchanged (g may write through x)
        let mut f = one_block_function(vec![
            assign("t", vec![read("x")]),
            Statement::new_kind(StatementKind::CallAssign {
                style: CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(thin_vec::thin_vec!["g".to_string()]),
                },
                rets: thin_vec::ThinVec::new(),
                args: thin_vec::thin_vec![read("x")],
            }),
            assign("y", vec![read("t")]),
        ]);
        coalesce_function(&mut f);
        assert_eq!(block_kinds(&f).len(), 3);
    }

    #[test]
    fn substitutes_into_return() {
        // t = y; return t  =>  return y
        let mut f = one_block_function(vec![assign("t", vec![read("y")])]);
        f.blocks[BasicBlockIdx::ZERO].terminator =
            Some(Terminator::new_kind(TerminatorKind::Return {
                args: smallvec![read("t")],
            }));
        coalesce_function(&mut f);
        assert_eq!(block_kinds(&f).len(), 0);
        let term = f.blocks[BasicBlockIdx::ZERO].terminator();
        let TerminatorKind::Return { args } = &term.kind else {
            panic!("expected return");
        };
        assert_eq!(args.as_slice(), &[read("y")]);
    }

    #[test]
    fn versioned_variables_are_untouched() {
        // Already-SSA form must be a no-op.
        let t = local("t").with_version(1);
        let x = local("x").with_version(1);
        let mut f = one_block_function(vec![
            Statement::new_kind(StatementKind::Assign {
                dest: t.clone(),
                sources: smallvec![Exp::Variable(local("a").with_version(0))],
            }),
            Statement::new_kind(StatementKind::Assign {
                dest: x,
                sources: smallvec![Exp::Variable(t)],
            }),
        ]);
        coalesce_function(&mut f);
        assert_eq!(block_kinds(&f).len(), 2);
    }
}
