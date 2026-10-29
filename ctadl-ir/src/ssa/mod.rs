/*!
This module implements Cytron et al's phi placement and SSA renaming.

The SSA form is *pruned*: a phi for a variable goes only where the variable is live on entry to
the block, so no phi is placed that no later use can observe.

After SSA conversion, one may depend on a few things:
- All variables are versioned. Version 0 is the "incoming" version for each variable, conceptually.
- Right before each `return`, there is a `param-flow` instruction that indicates, for each formal
  parameter to the function, which version of that parameter is written back to the caller. This
  is always the entry version, version 0: rebinding a parameter does not change the caller's
  argument (see [`ParameterType`]). This helps generate code that tracks flows on formal
  parameters.

*/
use std::collections::{BTreeSet, HashMap, HashSet};

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

mod frame;
pub use frame::{FrameWrites, coarsen_frame_updates, coarsen_frame_updates_function};

mod mem2reg;
use mem2reg::log_escape_note;
pub use mem2reg::{
    DEFAULT_FULL_THRESHOLD, EscapeKind, EscapeNote, EscapePolicy, FullMode, Mem2Reg, Mem2RegReport,
    Mem2RegStats, STACK_TOP, promote_function, promote_stack_slots, promote_stack_slots_function,
    promote_stack_slots_with,
};

#[derive(Debug)]
struct PhiPlace {
    variables: BTreeSet<Variable>,
    dominators: DominatorTree<BasicBlockIdx>,
}

#[derive(Debug)]
struct SsaRename {
    /// S(*) in the paper is an array of stacks, one per variable, that hold the unique SSA
    /// version numbers. Top of S(V) is used to construct V_i that replaces a use of V.
    s: HashMap<Variable, Vec<usize>>,
    /// C(*) is an array of integers, one per variable. The count C(V) tells how many assignments
    /// to V have been processed.
    c: HashMap<Variable, usize>,
}

/// The IR-to-IR passes that have to run after an import is read and before facts are
/// generated.
///
/// The order of the passes belongs to the pipeline, not to the caller.
/// [`Pipeline::index_default`] is the one place that says what `ctadl index` runs, and
/// [`run_pipeline_with_threads`] is the one place that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipeline {
    /// How frame writes reach SSA. [`FrameWrites::Store`] runs [`coarsen_frame_updates`] first,
    /// which turns the frame's functional updates into stores.
    pub frame: FrameWrites,
    /// Run [`eliminate_dead_temps`], which drops temporaries that are assigned but never read.
    pub dead_temps: bool,
    /// Run [`coalesce_copies`], which merges away copy temporaries that have a single use.
    pub coalesce: bool,
    /// Run [`promote_stack_slots`] with this configuration, which turns stack slots into locals
    /// so SSA versions them. `None` skips it.
    pub mem2reg: Option<Mem2Reg>,
    /// Run [`transform_program`], which places pruned phi nodes using Cytron's algorithm and renames
    /// variables into SSA form.
    pub ssa: bool,
    /// Run [`propagate_copies`], which forwards copies through the SSA graph.
    pub copy_prop: bool,
    /// Passed on to [`transform_program`]. Has no effect unless `ssa` is set.
    ///
    /// Set this whenever the IR comes from a front end that can emit blocks no path reaches.
    /// SSA conversion needs every block to be reachable from the start block and panics
    /// otherwise, and pruning is how a caller makes that true.
    pub prune_unreachable: bool,
}

impl Pipeline {
    /// The passes `ctadl index` runs. This function is the only definition of that set.
    ///
    /// Dead-temp elimination runs before coalescing (see [`eliminate_dead_temps`]). SSA
    /// conversion itself must not run twice; see [`transform`].
    #[inline]
    pub fn index_default() -> Self {
        Pipeline {
            // The frame stays versioned for the slots mem2reg keeps in memory, so a write to one
            // of them overwrites what it held instead of joining every other value stored there.
            frame: FrameWrites::Update,
            dead_temps: true,
            coalesce: true,
            // An escaped frame address is assumed to reach 8 slots. It loses no flow in the
            // regression suite, where `Exact` loses one through a field of a struct passed by
            // pointer, and promotes nearly as much as `Exact`, where `Above` promotes almost
            // nothing.
            mem2reg: Some(Mem2Reg {
                escape: EscapePolicy::Window(8),
                full_above: None,
            }),
            ssa: true,
            copy_prop: true,
            prune_unreachable: true,
        }
    }

    /// Run no passes at all, leaving the IR as the front end produced it.
    #[inline]
    pub fn none() -> Self {
        Pipeline {
            frame: FrameWrites::Update,
            dead_temps: false,
            coalesce: false,
            mem2reg: None,
            ssa: false,
            copy_prop: false,
            prune_unreachable: false,
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

    /// Sets [`Pipeline::mem2reg`], for the same reason as [`Pipeline::prune`].
    #[inline]
    #[must_use]
    pub fn mem2reg(mut self, mem2reg: Option<Mem2Reg>) -> Self {
        self.mem2reg = mem2reg;
        self
    }

    /// Sets [`Pipeline::frame`], for the same reason as [`Pipeline::prune`].
    #[inline]
    #[must_use]
    pub fn frame(mut self, frame: FrameWrites) -> Self {
        self.frame = frame;
        self
    }

    /// A short name for this pipeline, to record in a log line or a report which passes ran.
    /// For example, `fs+dt+co+ssa(prune)+cp`, or `none`.
    ///
    /// The same `Pipeline` value always produces the same string, and two pipelines produce the
    /// same string only when they are equal.
    pub fn tag(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(6);
        if self.frame == FrameWrites::Store {
            parts.push("fs".into());
        }
        if self.dead_temps {
            parts.push("dt".into());
        }
        if self.coalesce {
            parts.push("co".into());
        }
        if let Some(policy) = self.mem2reg {
            parts.push(format!("m2r({})", policy.tag()));
        }
        if self.ssa {
            parts.push(if self.prune_unreachable {
                "ssa(prune)".into()
            } else {
                "ssa".into()
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

/// Runs the passes that `p` selects, in the only order that is valid for them, on one thread.
///
/// Anything that reads an import and then generates facts should call this (or
/// [`run_pipeline_with_threads`]) rather than list the passes again.
pub fn run_pipeline(program: &mut Program, p: Pipeline) {
    run_pipeline_with_threads(program, p, 1);
}

/// What [`run_function_pipeline`] did to one function, summed over a program for the log.
#[derive(Debug, Default, Clone)]
struct PipelineStats {
    coarsened: usize,
    dead_temps: usize,
    coalesced: usize,
    copy_prop: usize,
    mem2reg: Mem2RegStats,
    notes: Vec<EscapeNote>,
}

impl std::ops::Add for PipelineStats {
    type Output = Self;
    fn add(mut self, o: Self) -> Self {
        self.coarsened += o.coarsened;
        self.dead_temps += o.dead_temps;
        self.coalesced += o.coalesced;
        self.copy_prop += o.copy_prop;
        self.mem2reg += o.mem2reg;
        self.notes.extend(o.notes);
        self
    }
}

/// [`run_pipeline`] on `threads` threads. `0` and `1` both mean the calling thread only.
///
/// Every pass reads and writes one function and nothing else, so running all of them on one
/// function before moving to the next gives the same program as running each pass over the whole
/// program in turn. That is what lets the functions be handed out to threads: each thread runs the
/// whole pipeline on the functions it takes. The threads come from a pool built for this call, not
/// rayon's global one, so `threads` is exactly the number used.
pub fn run_pipeline_with_threads(program: &mut Program, p: Pipeline, threads: usize) {
    run_pipeline_with(program, p, threads, &FunctionModes::default());
}

/// Per-function overrides of a [`Pipeline`], from a model's `modes`, keyed by function name.
#[derive(Debug, Default, Clone)]
pub struct FunctionModes {
    /// Forces or forbids full stack-slot promotion (see [`promote_stack_slots_with`]).
    pub mem2reg: HashMap<String, FullMode>,
    /// Overrides [`Pipeline::frame`].
    pub frame: HashMap<String, FrameWrites>,
}

/// [`run_pipeline_with_threads`], with the overrides in `modes` for the functions it names.
/// Returns what mem2reg did, if it ran, with its notes ordered by function name.
pub fn run_pipeline_with(
    program: &mut Program,
    p: Pipeline,
    threads: usize,
    modes: &FunctionModes,
) -> Option<Mem2RegReport> {
    let functions = &mut program.functions.raw;
    let mut stats = if threads <= 1 {
        functions
            .iter_mut()
            .map(|f| run_function_pipeline(f, p, modes))
            .fold(PipelineStats::default(), |a, b| a + b)
    } else {
        use rayon::prelude::*;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("ctadl-ssa-{i}"))
            .build()
            .expect("spawning the IR pass thread pool");
        // Function sizes are skewed: one function can take a large share of the pipeline's
        // time. Starting the largest functions first keeps one of them from being picked up last
        // and leaving every other thread idle while it runs.
        let mut work: Vec<&mut FunctionData> = functions.iter_mut().collect();
        work.sort_by_cached_key(|f| {
            std::cmp::Reverse(
                f.blocks
                    .iter_enumerated()
                    .map(|(_, b)| b.len())
                    .sum::<usize>(),
            )
        });
        pool.install(|| {
            work.into_par_iter()
                .with_max_len(1)
                .map(|f| run_function_pipeline(f, p, modes))
                .reduce(PipelineStats::default, |a, b| a + b)
        })
    };
    log::debug!(
        "frame({}): coarsened {} frame update(s) into stores",
        p.frame.tag(),
        stats.coarsened
    );
    if p.dead_temps {
        log::debug!(
            "dead_temps: deleted {} dead temporary def(s)",
            stats.dead_temps
        );
    }
    if p.coalesce {
        log::debug!(
            "coalesce_copies: removed {} fused copy statement(s)",
            stats.coalesced
        );
    }
    if p.copy_prop {
        log::debug!(
            "copy_prop: removed {} copy/trivial-phi statement(s)",
            stats.copy_prop
        );
    }
    let config = p.mem2reg?;
    log::info!("mem2reg({}): {:?}", config.tag(), stats.mem2reg);
    stats.notes.sort_by(|a, b| a.function.cmp(&b.function));
    Some(Mem2RegReport {
        stats: stats.mem2reg,
        notes: stats.notes,
    })
}

/// Runs the passes that `p` selects on one function, in pipeline order.
fn run_function_pipeline(
    f: &mut FunctionData,
    p: Pipeline,
    modes: &FunctionModes,
) -> PipelineStats {
    let mut stats = PipelineStats::default();
    if modes.frame.get(&f.name).copied().unwrap_or(p.frame) == FrameWrites::Store {
        stats.coarsened = coarsen_frame_updates_function(f);
    }
    if p.dead_temps {
        stats.dead_temps = eliminate_dead_temps_function(f);
    }
    if p.coalesce {
        stats.coalesced = coalesce_function(f);
    }
    if let Some(config) = p.mem2reg {
        let (m2r, note) = promote_function(f, config, modes.mem2reg.get(&f.name).copied());
        stats.mem2reg = m2r;
        stats.notes.extend(note.inspect(log_escape_note));
    }
    if p.ssa {
        log::debug!("f: {f}");
        transform_with(f, p.prune_unreachable, PhiMode::Pruned);
    }
    if p.copy_prop {
        stats.copy_prop = propagate_copies_function(f);
    }
    stats
}

/// Transforms every function in the program into SSA form by calling [`transform`] on each one.
///
/// Every function has to meet the preconditions listed on [`transform`].
pub fn transform_program(program: &mut Program, prune: bool) {
    for (_, f) in program.functions.iter_enumerated_mut() {
        log::debug!("f: {f}");
        transform_with(f, prune, PhiMode::Pruned);
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
/// form is pruned: every phi is reached by some non-phi use (see the module documentation). The
/// exit param-flow writes back every parameter at its entry version.
#[inline]
pub fn transform(function: &mut FunctionData, prune: bool) {
    transform_with(function, prune, PhiMode::Pruned);
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

fn transform_with(function: &mut FunctionData, prune: bool, mode: PhiMode) {
    if function.blocks.is_empty() {
        return;
    }
    if prune {
        prune_unreachable_nodes(function);
    }
    // Forward returns into the exit block as a new return. Change the former returns into gotos
    log::trace!("begin ssa transform");
    complete(function);
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
        let variable = VariableRef::new_global();
        blocks[BasicBlockIdx::START_BLOCK].push_front(Statement::new_kind(StatementKind::Assign {
            dest: variable.with_version(0),
            sources: smallvec![Exp::Variable(variable)],
        }));
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
pub fn prune_unreachable_nodes(function: &mut FunctionData) {
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
fn complete(function: &mut FunctionData) {
    // Creates block data for a "return <retvars>" block. Since we're going to rewrite all CFG
    // blocks to add the assignments and gotos, we don't actually wire up the exit block until the
    // end of this function.
    let retvars: Vec<_> = (0..function.return_type.arity)
        .map(|i| {
            let idx = function.intern_local(&format!("_$ret{i}"));
            VariableRef::new_local_idx(idx)
        })
        .collect();

    // Exit block observes parameters and returns retvars. Every parameter is written back at its
    // entry version, because rebinding a parameter does not reach the caller. It goes in already
    // versioned, so that renaming neither gives it its exit version nor places phis to merge
    // that version.
    let params = function
        .params
        .iter_enumerated()
        .map(|(i, _)| VariableRef::new_parameter(i).with_version(0))
        .collect();
    let param_flow = StatementKind::ParamFlow {
        params,
        global: VariableRef::new_global(),
    };
    let exit_block_contents = BasicBlockData::new_stmts(
        [Statement::new_kind(param_flow)].into_iter().collect(),
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
        if let TerminatorKind::Return { args } = &data.terminator().kind {
            let args = args.clone();
            // Create assignment of <retvar>* = <return'd var>
            for (retvar, arg) in std::iter::zip(&self.retvars, &args) {
                data.push_back(Statement::new_kind(StatementKind::assign(
                    retvar.clone(),
                    [arg.clone()],
                )));
            }
            // Finally, replace return with goto of exit block to get the graph into the shape
            // we need it for dominators/ssa transformation.
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
        let mut a: HashMap<Variable, SmallVec<[BasicBlockIdx; 4]>> = Default::default();
        // Maps each variable to the blocks that read it before any assignment to it in the same
        // block: the blocks where it is live on entry because of a use inside the block.
        let mut upward_uses: HashMap<Variable, SmallVec<[BasicBlockIdx; 4]>> = Default::default();
        // Set of all variables.
        let variables = &mut phi_place.variables;

        // Initialize `a`, `upward_uses` and `variables`. A statement reads its sources before it
        // writes its destinations, so `x = x + 1` is an upward-exposed use of `x`. A use that
        // already names a version (a parameter's write-back from `complete`) is not a use of
        // whatever version reaches it, so it keeps nothing live.
        let mut killed: HashSet<Variable> = HashSet::new();
        let mut used: HashSet<Variable> = HashSet::new();
        for (bb, data) in function.blocks.iter_enumerated() {
            killed.clear();
            used.clear();
            let mut note_use = |v: &VariableRef, killed: &HashSet<_>| {
                if !killed.contains(&v.variable) && used.insert(v.variable) {
                    upward_uses.entry(v.variable).or_default().push(bb);
                }
            };
            for stmt in data.iter() {
                for v in stmt.iter_src_var().filter(|v| v.version.is_none()) {
                    variables.insert(v.variable);
                    note_use(v, &killed);
                }
                for v in stmt.iter_dst_var() {
                    a.entry(v.variable).or_default().push(bb);
                    variables.insert(v.variable);
                    killed.insert(v.variable);
                }
            }
            for v in data.terminator().iter_src_var() {
                variables.insert(v.variable);
                note_use(v, &killed);
            }
        }

        let assigns_of = |variable: &Variable| -> SmallVec<[BasicBlockIdx; 4]> {
            a.get(variable).cloned().unwrap_or_default()
        };

        // Worklist of CFG nodes.
        let mut w: Vec<BasicBlockIdx> = Vec::new();
        // work[x] indicates whether x has ever been added to w during the current iteration of the
        // outer loop.
        let mut work: IndexVec<BasicBlockIdx, usize> =
            IndexVec::from_elem_n(0, function.blocks.num_nodes());
        // has_already[x] indicates whether a phi-function for v has been considered at x.
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
                                .map(|pred| (pred, VariableRef::new(v)))
                                .collect();
                            let block_data = &mut function.blocks.blocks_mut_preserves_cfg()[y];
                            block_data.push_front(Statement::new_kind(StatementKind::Phi {
                                dest: VariableRef::new(v),
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
            s.insert(*v, vec![0]);
            c.insert(*v, 1);
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
                // A use that already names a version is left as it is (see `complete`).
                for v in stmt.iter_src_var_mut().filter(|v| v.version.is_none()) {
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
    fn s(&self, v: &Variable) -> &Vec<usize> {
        self.s.get(v).unwrap()
    }

    #[inline]
    fn s_mut(&mut self, v: &Variable) -> &mut Vec<usize> {
        self.s.get_mut(v).unwrap()
    }

    #[inline]
    fn c(&self, v: &Variable) -> &usize {
        self.c.get(v).unwrap()
    }

    #[inline]
    fn c_mut(&mut self, v: &Variable) -> &mut usize {
        self.c.get_mut(v).unwrap()
    }
}
