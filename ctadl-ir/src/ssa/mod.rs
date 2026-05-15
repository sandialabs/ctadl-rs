/*!
This module implements Cytron et al's phi placement and SSA renaming.

After SSA conversion, one may depend on a few things:
- All variables are versioned. Version 0 is the "incoming" version for each variable, conceptually.
- Right before each `return`, there is a `param-flow` instruction that indicates, for each formal
  parameter to the function, which variable holds the current version of that parameter. This helps
  generate code that tracks flows on formal parameters.

*/
use std::collections::{HashMap, HashSet};

use internment::ArcIntern;
use smallvec::{SmallVec, smallvec};

use crate::graph::{DirectedGraph, Predecessors, StartNode, Successors, reachable};
use crate::index::{idx::Idx, index_vec::IndexVec};
use crate::mir::visit::MutVisitor;
use crate::mir::*;

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct PhiPlace {
    variables: HashSet<ArcIntern<Variable>>,
}

#[derive(Debug)]
struct SsaRename {
    /// S(*) in the paper is an array of stacks, one per variable, that hold the unique SSA
    /// version numbers. Top of S(V) is used to construct V_i that replaces a use of V.
    s: HashMap<ArcIntern<Variable>, Vec<usize>>,
    /// C(*) is an array of integers, one per variable. The counte C(V) tells how many assignments
    /// to V have been processed.
    c: HashMap<ArcIntern<Variable>, usize>,
}

pub fn transform_program(program: &mut Program, prune: bool) {
    for (_, f) in program.functions.iter_enumerated_mut() {
        log::debug!("f: {f}");
        transform(f, prune);
    }
}

/// Transform a function into SSA form. All [`VariableRef`]s are *versioned* so that each one has
/// exactly one definition and the definition dominates all its uses.
///
/// - `prune`: Prune unreachable CFG blocks before transforming
///
/// Precondition: the function isn't already in SSA form.
pub fn transform(function: &mut FunctionData, prune: bool) {
    if function.blocks.is_empty() {
        return;
    }
    if prune {
        prune_unreachable_nodes(function);
    }
    // Forward returns into the exit block as a new return. Change the former returns into gotos
    log::trace!("begin ssa transform");
    complete(function);
    let phi = PhiPlace::new(function);
    log::trace!("blocks after phi place: {}", function.blocks);
    SsaRename::new(&mut function.blocks, phi);

    // Set version 0 of parameters to incoming parameters
    for idx in function.params.iter_enumerated().map(|(i, _)| i) {
        let blocks = function.blocks.blocks_mut_preserves_cfg();
        let variable = VariableRef::new_parameter(idx);
        blocks[BasicBlockIdx::START_BLOCK].push_front(Statement::new_kind(StatementKind::Assign {
            dest: variable.with_version(0),
            sources: smallvec![Exp::AccessPath(AccessPath::without_fields(variable))],
        }));
    }
    // Set version 0 of global heap to global
    {
        let blocks = function.blocks.blocks_mut_preserves_cfg();
        let variable = VariableRef {
            variable: ArcIntern::new(Variable::GlobalHeap),
            version: None,
        };
        blocks[BasicBlockIdx::START_BLOCK].push_front(Statement::new_kind(StatementKind::Assign {
            dest: variable.with_version(0),
            sources: smallvec![Exp::AccessPath(AccessPath::without_fields(variable))],
        }));
    }
    log::trace!("assume that version 0 is initial version");
    log::trace!("blocks after rename: {}", function);
    function.verify().unwrap();
}

fn prune_unreachable_nodes(function: &mut FunctionData) {
    let reachable_indices: Vec<BasicBlockIdx> = reachable(&function.blocks).collect();
    if reachable_indices.len() == function.blocks.num_nodes() {
        return;
    }

    let mut mapping = HashMap::new();
    for (new_idx, &old_idx) in reachable_indices.iter().enumerate() {
        mapping.insert(old_idx, BasicBlockIdx::new(new_idx));
    }

    let mut new_blocks = IndexVec::new();
    for &old_idx in &reachable_indices {
        let mut data = function.blocks[old_idx].clone();

        // Update terminator targets
        if let Some(term) = &mut data.terminator
            && let TerminatorKind::Goto { targets } = &mut term.kind
        {
            for target in targets {
                *target = *mapping
                    .get(target)
                    .expect("Successor of reachable block should be reachable");
            }
        }

        // Update Phi nodes (if any - though we likely call this before SSA)
        for stmt in data.iter_mut() {
            if let StatementKind::Phi { operands, .. } = &mut stmt.kind {
                operands.retain(|(pred, _)| mapping.contains_key(pred));
                for (pred, _) in operands.iter_mut() {
                    *pred = *mapping.get(pred).unwrap();
                }
            }
        }

        new_blocks.push(data);
    }

    *function.blocks.blocks_mut() = new_blocks;
}

/// Completes the CFG by adding an exit node and tying the start node and every node that has no
/// successors to the exit node. This is required to compute dominators and place phi nodes.
///
/// The goal is to rewrite all the blocks that do "return x" with a goto to an exit block that
/// handles the return.
///
/// ```text
/// block_0: ... return a;
/// block_1: ... return b;
/// ```
///
/// turns into:
///
/// ```text
/// block_0: ... _$ret = a; goto 2;
/// block_1: ... _$ret = b; goto 2;
/// block_2: param-flow <params+globals>; return _$ret;
/// ```
///
/// At the end, the function will have exactly one return and it'll be the terminator of the
/// exit block. For this to be correct, it's important that _$ret be a fresh variable, which is
/// why we prefixed it in an odd way.
///
/// Precondition: Function has a start block.
fn complete(function: &mut FunctionData) {
    // Creates block data for a "return <retvars>" block. Since we're going to rewrite all CFG
    // blocks to add the assignments and gotos, we don't actually wire up the exit block until the
    // end of this function.
    let retvars: Vec<_> = (0..function.return_type.arity)
        .map(|i| VariableRef::new_local(format!("_$ret{i}").to_string()))
        .collect();

    // Exit block observes parameters and returns retvars
    let exit_block_contents = BasicBlockData::new_stmts(
        [Statement::new_kind(StatementKind::param_flow(
            function.num_parameters(),
        ))]
        .into_iter()
        .collect(),
        Some(Terminator::new_kind(TerminatorKind::Return {
            args: retvars
                .iter()
                .map(|v| Exp::new_access_path(AccessPath::without_fields(v.clone())))
                .collect(),
        })),
    );

    // Rewrite blocks to target single exit block
    let exit = function.blocks.next_index();
    let mut exit_visitor = SingleExitRewrite { exit, retvars };
    exit_visitor.visit_function_data(FunctionIdx::new(0), function);

    // Let's wire up the exit block.
    // For dominator reasons, we add the exit block as a successor to the entry block
    let TerminatorKind::Goto { targets } =
        &mut function[BasicBlockIdx::START_BLOCK].terminator_mut().kind
    else {
        // We've previously rewritten all the returns to gotos, so this is unreachable
        unreachable!()
    };
    if !targets.contains(&exit) {
        targets.push(exit);
    }

    // Add exit block to function
    let blocks = &mut function.blocks;
    let blocks = blocks.blocks_mut();
    blocks.push(exit_block_contents);
}

struct SingleExitRewrite {
    exit: BasicBlockIdx,
    retvars: Vec<VariableRef>,
}

// Records returns and turns all control flow into gotos by rewriting returns into a goto to
// the exit block.
impl MutVisitor for SingleExitRewrite {
    // Instrument basic block and return
    fn visit_basic_block_data(
        &mut self,
        _function: FunctionIdx,
        _bb: BasicBlockIdx,
        data: &mut BasicBlockData,
    ) {
        // Create assignment of <retvar>* = <return'd var>
        if let TerminatorKind::Return { args } = &data.terminator().kind {
            let args = args.clone();
            // assign returned values into retvars
            for (retvar, arg) in std::iter::zip(&self.retvars, &args) {
                data.push_back(Statement::new_kind(StatementKind::assign(
                    retvar.clone(),
                    [arg.clone()],
                )));
            }
        }
        // Finally, replace return with goto of exit block to get the graph into the shape
        // we need it for dominators/ssa transformation.
        if matches!(data.terminator().kind, TerminatorKind::Return { .. }) {
            *data.terminator_mut() = Terminator::new_kind(TerminatorKind::Goto {
                targets: smallvec![self.exit],
            });
        }
    }
}

impl PhiPlace {
    /// Place phi functions. Figure 11 in the Cytron et al paper. The returns are used to
    /// initialize variable sets.
    fn new(function: &mut FunctionData) -> Self {
        let mut phi_place = Self {
            variables: Default::default(),
        };
        let blocks = &mut function.blocks;
        // Script-a in the paper. Maps variable to all the blocks that assign that variable.
        let mut a: HashMap<ArcIntern<Variable>, SmallVec<[BasicBlockIdx; 4]>> = Default::default();
        // Set of all variables.
        let variables = &mut phi_place.variables;

        // Initialize `a` and `variables`.
        for (bb, data) in blocks.iter_enumerated() {
            for stmt in data.iter() {
                for v in stmt.iter_dst_var() {
                    a.entry(v.variable.clone()).or_default().push(bb);
                    variables.insert(v.variable.clone());
                }
                for v in stmt.iter_src_var() {
                    variables.insert(v.variable.clone());
                }
            }
            for v in data.terminator().iter_src_var() {
                variables.insert(v.variable.clone());
            }
        }

        let assigns_of = |variable: &ArcIntern<Variable>| -> SmallVec<[BasicBlockIdx; 4]> {
            a.get(variable).cloned().unwrap_or_default()
        };

        // Worklist of CFG nodes.
        let mut w: Vec<BasicBlockIdx> = Vec::new();
        // work[x] indicates whether x has ever been added to w during the current iteration of the
        // outer loop.
        let mut work: IndexVec<BasicBlockIdx, usize> = IndexVec::from_elem_n(0, blocks.num_nodes());
        // has_already[x] indices whether a phi-function for v has been inserted at x.
        let mut has_already: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, blocks.num_nodes());
        let mut iter_count = 0;

        for v in variables.clone() {
            assert!(w.is_empty());
            iter_count += 1;
            // Set up worklist with set of basic blocks with assignments to v.
            for x in assigns_of(&v) {
                work[x] = iter_count;
                w.push(x);
            }
            while let Some(x) = w.pop() {
                let df_y: SmallVec<[_; 4]> = blocks.dominance_frontier().iter(x).collect();
                for y in df_y.into_iter() {
                    if has_already[y] < iter_count {
                        // Insert a phi func with placeholder copies of predecessor operand
                        let operands = blocks
                            .predecessors(y)
                            .map(|pred| (pred, VariableRef::new_var_ref(v.clone())))
                            .collect();
                        let block_data = &mut blocks[y];
                        block_data.push_front(Statement::new_kind(StatementKind::Phi {
                            dest: VariableRef::new_var_ref(v.clone()),
                            operands,
                        }));
                        // Done with placing

                        has_already[y] = iter_count;
                        if work[y] < iter_count {
                            work[y] = iter_count;
                            w.push(y);
                        }
                    }
                }
            }
        }

        phi_place
    }
}

impl SsaRename {
    /// Version 0 is the incoming version of the variable.
    fn new(blocks: &mut BasicBlocks, place: PhiPlace) -> Self {
        let mut c: HashMap<_, _> = Default::default();
        let mut s: HashMap<_, _> = Default::default();
        // Initialize so that version 0 is the already-set version of each variable.
        for v in &place.variables {
            s.insert(v.clone(), vec![0]);
            c.insert(v.clone(), 1);
        }
        // Rewrite all the blocks
        let mut result = Self { s, c };
        result.search(blocks, blocks.start_node());
        result
    }

    fn search(&mut self, blocks: &mut BasicBlocks, x: BasicBlockIdx) {
        use StatementKind::*;

        for (_z, stmt) in blocks[x].iter_enumerated_mut() {
            // Ensure to rewrite assignment-uses and param-flow-uses
            if !matches!(stmt.kind, Phi { .. }) {
                for v in stmt.iter_src_var_mut() {
                    let i = *self.s(&v.variable).last().unwrap();
                    // Replace use of v with v_i
                    v.version = Some(i.try_into().unwrap());
                }
            }
            for v in stmt.iter_dst_var_mut() {
                let i = *self.c(&v.variable);
                v.version = Some(i.try_into().unwrap());
                self.s_mut(&v.variable).push(i);
                *self.c_mut(&v.variable) = i + 1;
            }
        }
        for v in blocks[x].terminator_mut().iter_src_var_mut() {
            let i = *self.s(&v.variable).last().unwrap();
            // Replace use of v with v_i
            v.version = Some(i.try_into().unwrap());
        }
        // Note: this code has to find the operand that references the successor, which takes
        // O(|successors|*|phi_operands|) time. I am assuming, for the time being, that this cost
        // is small. If we need to optimize this, we can compute a basic-block-wide which_pred
        // table before doing renaming so that this computation is just a lookup.
        for y in blocks.successors(x).collect::<SmallVec<[_; 4]>>() {
            for f in blocks[y].iter_mut() {
                if let StatementKind::Phi { operands, .. } = &mut f.kind {
                    for (op_pred, op) in operands {
                        if x == *op_pred {
                            let i = *self
                                .s(&op.variable)
                                .last()
                                .unwrap_or_else(|| panic!("Cannot get top {}", op.variable));
                            op.version = Some(i.try_into().unwrap());
                            break;
                        }
                    }
                }
            }
        }
        for y in blocks
            .dominators()
            .successors(x)
            .collect::<SmallVec<[_; 4]>>()
        {
            self.search(blocks, y);
        }
        for (_z, stmt) in blocks[x].iter_enumerated() {
            for v in stmt.iter_dst_var() {
                self.s_mut(&v.variable).pop();
            }
        }
    }

    #[inline]
    fn s(&self, v: &ArcIntern<Variable>) -> &Vec<usize> {
        self.s.get(v).unwrap()
    }

    #[inline]
    fn s_mut(&mut self, v: &ArcIntern<Variable>) -> &mut Vec<usize> {
        self.s.get_mut(v).unwrap()
    }

    #[inline]
    fn c(&self, v: &ArcIntern<Variable>) -> &usize {
        self.c.get(v).unwrap()
    }

    #[inline]
    fn c_mut(&mut self, v: &ArcIntern<Variable>) -> &mut usize {
        self.c.get_mut(v).unwrap()
    }
}
