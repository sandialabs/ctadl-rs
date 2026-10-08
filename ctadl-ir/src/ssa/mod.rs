/*!
This module implements Cytron et al's phi placement and SSA renaming.

The SSA form is *pruned*: a phi for a variable goes only where the variable is live on entry to
the block, so no phi is placed that no later use can observe.

After SSA conversion, one may depend on a few things:
- All variables are versioned. Version 0 is the "incoming" version for each variable, conceptually.
- Right before each `return`, there is a `param-flow` instruction that indicates, for each formal
  parameter to the function, which variable holds the current version of that parameter. This helps
  generate code that tracks flows on formal parameters.

*/
use std::collections::{HashMap, HashSet};

use internment::ArcIntern;
use smallvec::{SmallVec, smallvec};

use crate::graph::dominators::{DominanceFrontier, DominatorTree};
use crate::graph::{DirectedGraph, Predecessors, StartNode, Successors, reachable};
use crate::index::{idx::Idx, index_vec::IndexVec};
use crate::mir::visit::MutVisitor;
use crate::mir::*;

#[cfg(test)]
mod tests;

mod coalesce;
pub use coalesce::{coalesce_copies, coalesce_function};

mod dead_temps;
pub use dead_temps::{eliminate_dead_temps, eliminate_dead_temps_function};

mod copy_prop;
pub use copy_prop::{propagate_copies, propagate_copies_function};

#[derive(Debug)]
struct PhiPlace {
    variables: HashSet<ArcIntern<Variable>>,
    dominators: DominatorTree<BasicBlockIdx>,
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

/// The IR-to-IR passes that have to run after an import is read and before facts are
/// generated.
///
/// The order of the passes belongs to the pipeline, not to the caller. Before this type
/// existed, the order was four lines inside `ctadl index`, so anything else that read an import
/// had to copy the order out of a command-line function and hope it was still current.
/// [`Pipeline::index_default`] is now the one place that says what `ctadl index` runs, and
/// [`run_pipeline`] is the one place that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipeline {
    /// Run [`eliminate_dead_temps`], which drops temporaries that are assigned but never read.
    pub dead_temps: bool,
    /// Run [`coalesce_copies`], which merges away copy temporaries that have a single use.
    pub coalesce: bool,
    /// Run [`transform_program`], which places pruned phi nodes using Cytron's algorithm and renames
    /// variables into SSA form.
    pub ssa: bool,
    /// Run [`propagate_copies`], which forwards copies through the SSA graph.
    pub copy_prop: bool,
    /// Passed on to [`transform_program_with`]. Has no effect unless `ssa` is set.
    ///
    /// Set this whenever the IR comes from a front end that can emit blocks no path reaches.
    /// SSA conversion needs every block to be reachable from the start block and panics
    /// otherwise, and pruning is how a caller makes that true.
    pub prune_unreachable: bool,
    /// Passed on to [`transform_program_with`]; see [`ParamWriteBack`]. Has no effect unless
    /// `ssa` is set.
    pub param_write_back: ParamWriteBack,
}

/// Which version of each parameter the exit block's param-flow hands back to the parameter's
/// formal. The param-flow is how a callee's writes through a parameter reach its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamWriteBack {
    /// The version live at exit. Right for a front end that writes through a parameter by
    /// assigning to it.
    #[default]
    Exit,
    /// The entry version, version 0. Right for a front end whose parameter variables are
    /// ordinary locals that the callee may rebind: the caller's argument cannot change, and a
    /// write through it is a field store on the entry version. Reassigning a parameter, as in
    /// `p0 = p0.f` before a return, then does not flow `p0.f` back to the caller's argument.
    Entry,
}

impl Pipeline {
    /// The passes `ctadl index` runs. This function is the only definition of that set.
    ///
    /// Dead-temp elimination runs first for two reasons. It removes definitions that coalescing
    /// cannot remove, because a dead temporary has no use to merge it into. And it makes the
    /// program smaller before coalescing scans it. Both passes do nothing to a program that is
    /// already in SSA form, such as a flowy import, which is why running the whole pipeline
    /// twice gives the same result as running it once.
    #[inline]
    pub fn index_default() -> Self {
        Pipeline {
            dead_temps: true,
            coalesce: true,
            ssa: true,
            copy_prop: true,
            prune_unreachable: true,
            param_write_back: ParamWriteBack::Exit,
        }
    }

    /// Run no passes at all, leaving the IR as the front end produced it.
    #[inline]
    pub fn none() -> Self {
        Pipeline {
            dead_temps: false,
            coalesce: false,
            ssa: false,
            copy_prop: false,
            prune_unreachable: false,
            param_write_back: ParamWriteBack::Exit,
        }
    }

    /// Run SSA renaming and unreachable-block pruning, and none of the cleanup passes.
    #[inline]
    pub fn ssa_only() -> Self {
        Pipeline {
            ssa: true,
            prune_unreachable: true,
            ..Pipeline::none()
        }
    }

    /// Sets [`Pipeline::prune_unreachable`]. This lets a caller pass an index option through
    /// without writing out the rest of the pipeline again.
    #[inline]
    #[must_use]
    pub fn prune(mut self, prune_unreachable: bool) -> Self {
        self.prune_unreachable = prune_unreachable;
        self
    }

    /// Sets [`Pipeline::param_write_back`].
    #[inline]
    #[must_use]
    pub fn param_write_back(mut self, param_write_back: ParamWriteBack) -> Self {
        self.param_write_back = param_write_back;
        self
    }

    /// A short name for this pipeline, to record in a log line or a report which passes ran.
    /// For example, `dt+co+ssa(prune)+cp`, or `none`.
    ///
    /// The same `Pipeline` value always produces the same string, and two pipelines produce the
    /// same string only when they are equal.
    pub fn tag(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if self.dead_temps {
            parts.push("dt".into());
        }
        if self.coalesce {
            parts.push("co".into());
        }
        if self.ssa {
            let mut opts: Vec<&str> = Vec::new();
            if self.prune_unreachable {
                opts.push("prune");
            }
            if self.param_write_back == ParamWriteBack::Entry {
                opts.push("entry");
            }
            parts.push(if opts.is_empty() {
                "ssa".into()
            } else {
                format!("ssa({})", opts.join(","))
            });
        }
        if self.copy_prop {
            parts.push("cp".into());
        }
        if parts.is_empty() {
            // `prune_unreachable` does nothing on its own, without `ssa`. So "none" is the
            // right answer here: this pipeline runs no passes.
            return "none".to_string();
        }
        parts.join("+")
    }
}

impl Default for Pipeline {
    #[inline]
    fn default() -> Self {
        Pipeline::index_default()
    }
}

/// Runs the passes that `p` selects, in the only order that is valid for them.
///
/// `ctadl index` used to contain this code directly. Anything that reads an import and then
/// generates facts should call this rather than list the passes again.
pub fn run_pipeline(program: &mut Program, p: Pipeline) {
    if p.dead_temps {
        eliminate_dead_temps(program);
    }
    if p.coalesce {
        coalesce_copies(program);
    }
    if p.ssa {
        transform_program_with(program, p.prune_unreachable, p.param_write_back);
    }
    if p.copy_prop {
        propagate_copies(program);
    }
}

/// Transforms every function in the program into SSA form by calling [`transform`] on each one.
///
/// Every function has to meet the preconditions listed on [`transform`].
pub fn transform_program(program: &mut Program, prune: bool) {
    transform_program_with(program, prune, ParamWriteBack::Exit);
}

/// [`transform_program`], choosing which parameter versions the exit param-flow writes back.
pub fn transform_program_with(program: &mut Program, prune: bool, write_back: ParamWriteBack) {
    for (_, f) in program.functions.iter_enumerated_mut() {
        log::debug!("f: {f}");
        transform_with(f, prune, PhiMode::Pruned, write_back);
    }
}

/// Transform a function into SSA form. All [`VariableRef`]s are *versioned* so that each one has
/// exactly one definition and the definition dominates all its uses.
///
/// - `prune`: Prune unreachable CFG blocks before transforming
///
/// This function is where a function's control-flow graph stops being something under
/// construction and starts being a graph that gets walked. Up to this point a
/// [`BasicBlockData`] is allowed to be missing its terminator, because front ends build the
/// statements of a block before they know its successors. From here on that is not allowed:
/// this pass computes dominators, a dominance frontier, and predecessor sets, and all three
/// read the graph through the terminators.
///
/// # Preconditions
///
/// - Every block has a terminator. A block without one is a block still being built, and this
///   pass has no way to know where control leaves it.
/// - Every `goto` names at least one target, and every target is a block of this function.
/// - Every block is reachable from the start block. Passing `prune` makes this true by deleting
///   the unreachable blocks first, which is what callers that read imported code should do,
///   because a front end can easily emit code no path reaches.
/// - The function is not already in SSA form. Running this twice would version the versions.
///
/// The first two preconditions are what [`FunctionData::verify`] checks, so a caller holding IR
/// from a source it does not control can call that first and get a list of errors. There is no
/// such check for the last two.
///
/// A caller that breaks a precondition gets a panic, not an error. A missing terminator panics
/// in [`BasicBlockData::terminator`]. A block that no path reaches trips the assertion in
/// [`DominatorTree::new`], which says the graph contains nodes not reachable from entry. This
/// is deliberate: the IR is wrong at that point, and the passes downstream of SSA would
/// otherwise produce quietly wrong facts.
///
/// # Postconditions
///
/// The function passes [`FunctionData::verify`], which this pass asserts before returning. It
/// has exactly one `return`, and that `return` is the terminator of the single exit block this
/// pass adds. Every variable use names a version, and version 0 is the incoming version. The
/// form is pruned: every phi is reached by some non-phi use (see the module documentation).
#[inline]
pub fn transform(function: &mut FunctionData, prune: bool) {
    transform_with(function, prune, PhiMode::Pruned, ParamWriteBack::Exit);
}

/// Which phis [`PhiPlace`] inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhiMode {
    /// Every block in the iterated dominance frontier of a variable's definitions gets a phi.
    /// Only the tests use this, as the baseline pruned SSA is checked against.
    #[cfg_attr(not(test), allow(dead_code))]
    Minimal,
    /// Of those blocks, only the ones where the variable is live on entry get a phi.
    Pruned,
}

/// Fills in the exit param-flow's parameters with their entry versions, version 0. Runs after
/// renaming, on a param-flow that [`complete`] built without parameters.
fn write_back_entry_params(function: &mut FunctionData) {
    let params: IndexVec<ParameterIdx, VariableRef> = function
        .params
        .iter_enumerated()
        .map(|(i, _)| VariableRef::new_parameter(i).with_version(0))
        .collect();
    let blocks = function.blocks.blocks_mut_preserves_cfg();
    let exit = blocks
        .iter_mut()
        .rev()
        .find(|b| {
            b.terminator
                .as_ref()
                .is_some_and(|t| matches!(t.kind, TerminatorKind::Return { .. }))
        })
        .expect("SSA has a single exit block");
    for st in exit.statements.iter_mut() {
        if let StatementKind::ParamFlow { params: p, .. } = &mut st.kind {
            *p = params;
            return;
        }
    }
    unreachable!("the exit block has a param-flow");
}

fn transform_with(
    function: &mut FunctionData,
    prune: bool,
    mode: PhiMode,
    write_back: ParamWriteBack,
) {
    if function.blocks.is_empty() {
        return;
    }
    if prune {
        prune_unreachable_nodes(function);
    }
    // Forward returns into the exit block as a new return. Change the former returns into gotos
    log::trace!("begin ssa transform");
    complete(function, write_back);
    let phi = PhiPlace::new(function, mode);
    log::trace!("blocks after phi place: {}", function.blocks);
    SsaRename::new(&mut function.blocks, phi);

    // Set version 0 of parameters to incoming parameters
    for idx in function.params.iter_enumerated().map(|(i, _)| i) {
        let blocks = function.blocks.blocks_mut_preserves_cfg();
        let variable = VariableRef::new_parameter(idx);
        blocks[BasicBlockIdx::START_BLOCK].push_front(Statement::new_kind(StatementKind::Assign {
            dest: variable.with_version(0),
            sources: smallvec![Exp::Variable(variable)],
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
            sources: smallvec![Exp::Variable(variable)],
        }));
    }
    if write_back == ParamWriteBack::Entry {
        write_back_entry_params(function);
    }
    log::trace!("assume that version 0 is initial version");
    log::trace!("blocks after rename: {}", function);
    function.verify().unwrap();
}

/// Deletes the blocks that no path from the start block reaches, and renumbers the ones that
/// remain so the indices stay contiguous. `goto` targets and phi operands are updated to the new
/// numbering.
///
/// This is how a caller satisfies the reachability precondition of [`transform`]. Dominators
/// are only defined for blocks the start block reaches, so an unreachable block left in place
/// is a panic later, not a wrong answer.
fn prune_unreachable_nodes(function: &mut FunctionData) {
    let reachable_indices = reachable(&function.blocks);
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
/// Preconditions: the function has a start block, and every block has a terminator. This runs
/// as part of [`transform`], so it inherits that function's preconditions.
fn complete(function: &mut FunctionData, write_back: ParamWriteBack) {
    // Creates block data for a "return <retvars>" block. Since we're going to rewrite all CFG
    // blocks to add the assignments and gotos, we don't actually wire up the exit block until the
    // end of this function.
    let retvars: Vec<_> = (0..function.return_type.arity)
        .map(|i| {
            let idx = function.intern_local(&format!("_$ret{i}"));
            VariableRef::new_local_idx(idx)
        })
        .collect();

    // Exit block observes parameters and returns retvars. Under `ParamWriteBack::Entry` the
    // parameters are left out until after renaming (see `write_back_entry_params`), so that
    // renaming neither gives them their exit versions nor places phis to merge those.
    let observed = match write_back {
        ParamWriteBack::Exit => function.num_parameters(),
        ParamWriteBack::Entry => 0,
    };
    let exit_block_contents = BasicBlockData::new_stmts(
        [Statement::new_kind(StatementKind::param_flow(observed))]
            .into_iter()
            .collect(),
        Some(Terminator::new_kind(TerminatorKind::Return {
            args: retvars.iter().map(|v| Exp::Variable(v.clone())).collect(),
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
    ///
    /// With [`PhiMode::Pruned`], a phi for `v` goes at `y` only if `y` is in the iterated
    /// dominance frontier of `v`'s definitions *and* `v` is live on entry to `y`. This is the
    /// pruned SSA of Choi, Cytron and Ferrante. The worklist still visits every block of the
    /// iterated dominance frontier; liveness decides only whether a phi is inserted there.
    fn new(function: &mut FunctionData, mode: PhiMode) -> Self {
        let dominators = DominatorTree::new(&function.blocks);
        let mut phi_place = Self {
            variables: Default::default(),
            dominators,
        };
        // Script-a in the paper. Maps variable to all the blocks that assign that variable.
        let mut a: HashMap<ArcIntern<Variable>, SmallVec<[BasicBlockIdx; 4]>> = Default::default();
        // Maps each variable to the blocks that read it before any assignment to it in the same
        // block: the blocks where it is live on entry because of a use inside the block.
        let mut upward_uses: HashMap<ArcIntern<Variable>, SmallVec<[BasicBlockIdx; 4]>> =
            Default::default();
        // Set of all variables.
        let variables = &mut phi_place.variables;

        // Initialize `a`, `upward_uses` and `variables`. A statement reads its sources before it
        // writes its destinations, so `x = x + 1` is an upward-exposed use of `x`.
        let mut killed: HashSet<ArcIntern<Variable>> = HashSet::new();
        let mut used: HashSet<ArcIntern<Variable>> = HashSet::new();
        for (bb, data) in function.blocks.iter_enumerated() {
            killed.clear();
            used.clear();
            let mut note_use = |v: &VariableRef, killed: &HashSet<_>| {
                if !killed.contains(&v.variable) && used.insert(v.variable.clone()) {
                    upward_uses.entry(v.variable.clone()).or_default().push(bb);
                }
            };
            for stmt in data.iter() {
                for v in stmt.iter_src_var() {
                    variables.insert(v.variable.clone());
                    note_use(v, &killed);
                }
                for v in stmt.iter_dst_var() {
                    a.entry(v.variable.clone()).or_default().push(bb);
                    variables.insert(v.variable.clone());
                    killed.insert(v.variable.clone());
                }
            }
            for v in data.terminator().iter_src_var() {
                variables.insert(v.variable.clone());
                note_use(v, &killed);
            }
        }

        let assigns_of = |variable: &ArcIntern<Variable>| -> SmallVec<[BasicBlockIdx; 4]> {
            a.get(variable).cloned().unwrap_or_default()
        };

        // Worklist of CFG nodes.
        let mut w: Vec<BasicBlockIdx> = Vec::new();
        // work[x] indicates whether x has ever been added to w during the current iteration of the
        // outer loop.
        let mut work: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, function.blocks.num_nodes());
        // has_already[x] indices whether a phi-function for v has been considered at x.
        let mut has_already: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, function.blocks.num_nodes());
        // defines[x] == iter_count means x assigns v; live_in[x] == iter_count means v is live on
        // entry to x. Stamped with iter_count, like `work`, so they need no clearing per variable.
        let mut defines: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, function.blocks.num_nodes());
        let mut live_in: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, function.blocks.num_nodes());
        let mut live_w: Vec<BasicBlockIdx> = Vec::new();
        let mut iter_count = 0;

        let df = DominanceFrontier::new(&function.blocks, &phi_place.dominators);
        for v in variables.clone() {
            assert!(w.is_empty());
            iter_count += 1;
            let defs = assigns_of(&v);
            if mode == PhiMode::Pruned {
                // Live-in set of v: walk backward from the upward-exposed uses, and stop at
                // blocks that assign v. A block that both assigns v and reads it first is
                // already a seed, so stopping there loses nothing.
                for &x in &defs {
                    defines[x] = iter_count;
                }
                for &x in upward_uses
                    .get(&v)
                    .map(|u| u.as_slice())
                    .unwrap_or_default()
                {
                    live_in[x] = iter_count;
                    live_w.push(x);
                }
                while let Some(x) = live_w.pop() {
                    for p in function.blocks.predecessors(x) {
                        if live_in[p] < iter_count && defines[p] < iter_count {
                            live_in[p] = iter_count;
                            live_w.push(p);
                        }
                    }
                }
            }
            // Set up worklist with set of basic blocks with assignments to v.
            for x in defs {
                work[x] = iter_count;
                w.push(x);
            }
            while let Some(x) = w.pop() {
                let df_y: SmallVec<[_; 4]> = df.iter(x).collect();
                for y in df_y.into_iter() {
                    if has_already[y] < iter_count {
                        if mode == PhiMode::Minimal || live_in[y] == iter_count {
                            // Insert a phi func with placeholder copies of predecessor operand
                            let operands = function
                                .blocks
                                .predecessors(y)
                                .map(|pred| (pred, VariableRef::new_var_ref(v.clone())))
                                .collect();
                            let block_data = &mut function.blocks.blocks_mut_preserves_cfg()[y];
                            block_data.push_front(Statement::new_kind(StatementKind::Phi {
                                dest: VariableRef::new_var_ref(v.clone()),
                                operands,
                            }));
                        }
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
        result.search(blocks, blocks.start_node(), &place.dominators);
        result
    }

    fn search(
        &mut self,
        blocks: &mut BasicBlocks,
        x: BasicBlockIdx,
        dominators: &DominatorTree<BasicBlockIdx>,
    ) {
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
        for y in dominators.successors(x).collect::<SmallVec<[_; 4]>>() {
            self.search(blocks, y, dominators);
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
