//! Python-bytecode language frontend.
//!
//! Shells out to the embedded Python serializer (via `python-bytecode-reader`),
//! parses the stable bytecode text into typed records, and lowers those records
//! into CTADL IR by modeling the CPython value-stack machine.
//!
//! ## Model
//!
//! Every code object (the module plus each lexically-nested function /
//! comprehension / class body) becomes a [`FunctionData`]. Within a function we
//! rebuild the CFG from instruction offsets and each instruction's resolved
//! `jump_targets`, then simulate the operand stack **per basic block** to emit
//! `Assign` / `Load` / `Store` / `CallAssign` statements.
//!
//! The stack is simulated starting empty at each block entry; a pop from an empty
//! stack synthesizes a fresh temporary (a value that, in the real interpreter,
//! arrived on the stack from a predecessor block). Flows through *named locals*
//! (`STORE_FAST`/`LOAD_FAST`) and *globals/attributes* (`STORE_GLOBAL`,
//! `STORE_ATTR`, …) — which is how ordinary Python data flow travels — are
//! therefore captured across blocks; only values left on the operand stack across
//! a block boundary are approximated. Unknown opcodes are skipped with a warning.
//!
//! Per the load/store invariant (post-#53), memory reads/writes are
//! [`StatementKind::Load`]/[`StatementKind::Store`] whose field is a symbolic
//! [`FieldPath`]; numeric offsets never appear here, so `MirVerify` holds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactEncoding, ArtifactKey, SourceInfoBuilder};

use crate::error::Error;
use ctadl_ir::mir::call::{CallEdges, CallStyle, VirtualMethodTable};
use ctadl_ir::*;

use python_bytecode_reader::model::{BytecodeFile, CodeObject, ConstEntry, Instruction};
use python_bytecode_reader::{Format, run_serializer};

/// Import a Python artifact: either a single `.py`/`.pyc` file, or a directory
/// that is crawled recursively for every `.py`/`.pyc` file. Each file's bytecode
/// is serialized, parsed, and lowered into a single [`ProgramInfo`]; a directory
/// yields one whole-program IR whose modules resolve calls to one another by name.
pub fn import_python(import: &crate::project::ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.artifact_path;
    if path.is_dir() {
        import_python_dir(path)
    } else {
        let text = run_serializer(path, Format::Stable)?;
        // Read the source (for `.py`) so instruction positions map to source lines
        // in SARIF. Absent for `.pyc` (or on read failure): source info degrades to
        // a zero offset, which is harmless.
        let source = std::fs::read_to_string(path).ok();
        lower_stable_text(path, &text, source)
    }
}

/// Import every `.py`/`.pyc` file found recursively under `dir`, lowering them
/// all into one [`ProgramInfo`]. Files are processed in sorted path order so the
/// result is deterministic, and external stubs are synthesized only once, after
/// every module is registered, so a call in one file resolves to a definition in
/// a sibling file rather than being stubbed out.
fn import_python_dir(dir: &Path) -> Result<ProgramInfo, Error> {
    let files = collect_python_files(dir)?;
    if files.is_empty() {
        return Err(Error::PythonConversion(format!(
            "no .py or .pyc files found under directory: {}",
            dir.display()
        )));
    }
    let mut lowering = Lowering::new();
    for (sub_artifact_id, path) in files.iter().enumerate() {
        let text = run_serializer(path, Format::Stable)?;
        let source = std::fs::read_to_string(path).ok();
        let file = python_bytecode_reader::parse(&text)?;
        lowering.set_file(path, source, sub_artifact_id as u32);
        lowering.lower_module(&file)?;
    }
    // All modules registered: now stub the names still undefined program-wide.
    lowering.create_external_stubs();
    lowering.finish()
}

/// Recursively collect every `.py`/`.pyc` file under `dir`, in sorted path order.
fn collect_python_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, Error> {
    let mut out = Vec::new();
    collect_python_files_into(dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_python_files_into(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // `symlink_metadata` so a symlinked directory is not followed into (and a
        // symlink cycle cannot make the crawl loop forever).
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_python_files_into(&path, out)?;
        } else if metadata.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext == "py" || ext == "pyc")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse stable bytecode text and lower it to a [`ProgramInfo`]. Factored out of
/// [`import_python`] so it can be exercised without spawning Python.
fn lower_stable_text(
    path: &Path,
    text: &str,
    source: Option<String>,
) -> Result<ProgramInfo, Error> {
    let file = python_bytecode_reader::parse(text)?;
    let mut lowering = Lowering::new();
    lowering.set_file(path, source, 0);
    lowering.lower_file(&file)?;
    lowering.finish()
}

/// One nested-code-object function, resolved during a first pass.
struct FuncInfo {
    idx: FunctionIdx,
    /// Number of leading `varnames` that are parameters.
    nparams: usize,
}

struct Lowering {
    program: Program,
    source_info_builder: SourceInfoBuilder,
    artifact_key: ArtifactKey,
    /// Byte offset of the start of each source line (`line_starts[l]` = line l+1).
    line_starts: Vec<usize>,
    source_len: usize,
    /// document-order code id → resolved function.
    functions: BTreeMap<u32, FuncInfo>,
    /// code id → function name (for resolving `code N` const references).
    func_names: BTreeMap<u32, String>,
    /// Names called somewhere via a direct call → max argument count seen. Used to
    /// synthesize external stub functions (so JSON models' `find: methods` can
    /// match callees like `source`/`sink` that have no Python definition).
    external_calls: BTreeMap<String, usize>,
    /// Names of functions actually defined by the program.
    defined_names: BTreeSet<String>,
    /// Class function name → its `__init__` function name. A call to the class
    /// (whose body function shares the class's name) is lowered as instantiation:
    /// a fresh instance passed to `__init__`.
    class_init: BTreeMap<String, String>,
    /// Within the function being lowered, local variables currently holding a
    /// function pointer (from `MAKE_FUNCTION` / a `def`), so a later call through
    /// the local resolves as a direct call rather than an unresolved funcptr call.
    func_ptr_locals: BTreeMap<String, String>,
    /// True while lowering a generator body, so `YIELD_VALUE` feeds the
    /// generator-result object and every `RETURN` yields that object.
    in_generator: bool,
    /// True while lowering a code object whose `FOR_ITER` pops the iterator on the
    /// loop-exhaust edge (<=3.11, `dis.stack_effect(FOR_ITER, jump=True) == -1`),
    /// detected by the absence of the 3.12+ `END_FOR` cleanup opcode. On 3.12+ the
    /// iterator (and the value) stay on the exit edge and `END_FOR` cleans them up.
    for_exhaust_pops_iter: bool,
    /// True while lowering a code object compiled by a <=3.10 interpreter (no
    /// `RESUME`, added in 3.11). Those versions push the function's qualname above
    /// its code object before `MAKE_FUNCTION`; 3.11+ dropped that stack slot.
    pre_3_11: bool,
    /// Exception-handler blocks (those beginning with `PUSH_EXC_INFO`) of the
    /// function currently being lowered, so a `raise` can route to them.
    handler_blocks: Vec<BasicBlockIdx>,
    /// Entry-stack overrides for successor blocks whose stack, on that edge,
    /// differs from the predecessor's snapshot. Used by `FOR_ITER` on <=3.11,
    /// where the loop-exhaust edge pops the iterator (unlike the loop-body edge).
    block_entry_overrides: BTreeMap<BasicBlockIdx, Vec<Slot>>,
    /// Per source line, the number of column-less instructions already placed on
    /// it. <=3.10 has no per-instruction columns, so every instruction on a line
    /// would share one byte offset and the SARIF step dedup would merge distinct
    /// code-flow steps (e.g. a call and the sink on the same line). Handing each a
    /// distinct synthetic column keeps the steps separate. Reset per code object.
    line_column_cursor: BTreeMap<i64, usize>,
    counter: u32,
}

impl Lowering {
    /// An empty lowering with no file loaded. Call [`Lowering::set_file`] before
    /// lowering each module; a single [`Lowering`] accumulates every module of a
    /// directory import into one program.
    fn new() -> Self {
        Self {
            program: Program::default(),
            source_info_builder: SourceInfoBuilder::new(source_info::ArtifactMetadata::new()),
            artifact_key: ArtifactKey {
                path: String::new(),
                sub_artifact_id: 0,
                hash: Vec::new(),
                encoding: ArtifactEncoding::Binary,
            },
            line_starts: vec![0],
            source_len: 0,
            functions: BTreeMap::new(),
            func_names: BTreeMap::new(),
            external_calls: BTreeMap::new(),
            defined_names: BTreeSet::new(),
            class_init: BTreeMap::new(),
            func_ptr_locals: BTreeMap::new(),
            in_generator: false,
            for_exhaust_pops_iter: false,
            pre_3_11: false,
            handler_blocks: Vec::new(),
            block_entry_overrides: BTreeMap::new(),
            line_column_cursor: BTreeMap::new(),
            counter: 0,
        }
    }

    /// Point the lowering at the module about to be lowered: its source (for line
    /// mapping) and the [`ArtifactKey`] that tags every span it emits. `source` is
    /// `None` for `.pyc` (or on read failure), degrading spans to a zero offset.
    /// `sub_artifact_id` distinguishes files within a directory import.
    fn set_file(&mut self, path: &Path, source: Option<String>, sub_artifact_id: u32) {
        let (line_starts, source_len) = match &source {
            Some(s) => (compute_line_starts(s), s.len()),
            None => (vec![0], 0),
        };
        let encoding = if source.is_some() {
            ArtifactEncoding::Utf8
        } else {
            ArtifactEncoding::Binary
        };
        self.artifact_key = ArtifactKey {
            path: path.to_string_lossy().to_string(),
            sub_artifact_id,
            hash: Vec::new(),
            encoding,
        };
        self.line_starts = line_starts;
        self.source_len = source_len;
        // Per-file cursor: synthesized columns restart within each file's lines.
        self.line_column_cursor.clear();
    }

    /// Lower one module and synthesize external stubs for its unresolved calls.
    /// Used for a single-file import; a directory import instead calls
    /// [`Lowering::lower_module`] per file and stubs once at the end.
    fn lower_file(&mut self, file: &BytecodeFile) -> Result<(), Error> {
        self.lower_module(file)?;
        // Synthesize external stub functions for called-but-undefined names (so
        // models can match them by name).
        self.create_external_stubs();
        Ok(())
    }

    /// Register and lower every code object of one module into the shared program,
    /// without synthesizing external stubs (deferred so a directory import resolves
    /// names defined in sibling modules before stubbing whatever remains).
    fn lower_module(&mut self, file: &BytecodeFile) -> Result<(), Error> {
        // The `code N` const references and the id→function maps are scoped to a
        // single module (each module numbers its code objects from 0), so reset
        // them before registering this module. Function *indices* in `program`
        // keep growing, so already-lowered modules are untouched.
        self.functions.clear();
        self.func_names.clear();
        // Pass 1: create a function for every code object, numbering them in the
        // same pre-order the serializer used (module first, then nested in
        // co_consts order), so `code N` references resolve.
        let mut ordered = Vec::new();
        for co in &file.code_objects {
            self.register_code(co, &mut ordered);
        }
        // Pass 2: lower each code object's body.
        for (id, co) in &ordered {
            self.lower_code_object(*id, co)?;
        }
        Ok(())
    }

    /// Recursively assign function indices in document (pre-order) order.
    fn register_code<'a>(
        &mut self,
        co: &'a CodeObject,
        ordered: &mut Vec<(u32, &'a CodeObject)>,
    ) -> String {
        let id = ordered.len() as u32;
        let idx = self.program.new_function();
        let name = self.unique_function_name(&co.name);
        self.program[idx].set_name(name.clone());
        self.program[idx].set_return_type(ReturnType { arity: 1 });
        // Positional + keyword-only parameters, plus the `*args` / `**kwargs`
        // collector slots (which CPython lays out immediately after them in
        // `co_varnames`). Counting them as parameters lets a positional call bind
        // taint into `*args`, and lets the body's `LOAD_FAST args` resolve to it.
        let mut nparams = (co.arg_count + co.kwonly_count).max(0) as usize;
        if co.flags & CO_VARARGS != 0 {
            nparams += 1;
        }
        if co.flags & CO_VARKEYWORDS != 0 {
            nparams += 1;
        }
        let nparams = nparams.min(co.varnames.len());
        for _ in 0..nparams {
            self.program[idx]
                .params
                .parameters
                .push(ParameterType::ByRef);
        }
        self.defined_names.insert(co.name.clone());
        self.func_names.insert(id, name.clone());
        self.functions.insert(id, FuncInfo { idx, nparams });
        ordered.push((id, co));
        // Register nested code objects, noting any `__init__` method so a call to
        // this (class) function can be lowered as instantiation.
        for nested in &co.nested_code_objects {
            let nested_name = self.register_code(nested, ordered);
            if nested.name == "__init__" {
                self.class_init.insert(name.clone(), nested_name);
            }
        }
        name
    }

    /// A program-unique function name derived from `base` (the code object's
    /// `co_name`). The first use keeps `base`; later collisions get a suffix.
    fn unique_function_name(&self, base: &str) -> String {
        let base = if base.is_empty() { "<anonymous>" } else { base };
        if !self.name_taken(base) {
            return base.to_string();
        }
        let mut n = 1;
        loop {
            let candidate = format!("{base}#{n}");
            if !self.name_taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    fn name_taken(&self, name: &str) -> bool {
        self.program.functions.iter().any(|f| f.name == name)
    }

    fn lower_code_object(&mut self, id: u32, co: &CodeObject) -> Result<(), Error> {
        let func_idx = self.functions[&id].idx;
        let nparams = self.functions[&id].nparams;
        self.in_generator = co.flags & CO_GENERATOR != 0;
        // <=3.11 `FOR_ITER` pops the iterator on exhaust; 3.12+ keeps it and cleans
        // up with `END_FOR`. The presence of `END_FOR` in the code object is the
        // version-robust discriminator (a for-loop on 3.12+ always emits one).
        self.for_exhaust_pops_iter = !co
            .instructions
            .iter()
            .any(|i| matches!(i.opname.as_str(), "END_FOR"));
        // `RESUME` starts every 3.11+ code object; its absence marks a <=3.10 one,
        // where `MAKE_FUNCTION` also consumes a qualname pushed above the code.
        self.pre_3_11 = !co.instructions.iter().any(|i| i.opname == "RESUME");
        self.func_ptr_locals.clear();
        self.block_entry_overrides.clear();
        self.line_column_cursor.clear();

        // --- Build the CFG: leaders → blocks ---
        // A leader is the start of a basic block. Restrict to leaders that are
        // real instruction offsets, then create exactly one block per leader (so
        // no block is left without instructions or a terminator).
        let leaders = compute_leaders(&co.instructions);
        let leader_offsets: Vec<i64> = co
            .instructions
            .iter()
            .map(|i| i.offset)
            .filter(|o| leaders.contains(o))
            .collect();
        let mut block_ids: Vec<BasicBlockIdx> = Vec::with_capacity(leader_offsets.len());
        for _ in &leader_offsets {
            block_ids.push(self.program[func_idx].blocks.new_block());
        }
        // Map each block-leader offset to its block index.
        let mut block_of_offset: BTreeMap<i64, BasicBlockIdx> = BTreeMap::new();
        for (leader_off, bidx) in leader_offsets.iter().zip(block_ids.iter()) {
            block_of_offset.insert(*leader_off, *bidx);
        }

        // Group each instruction into the block of the last leader at or before it.
        let mut blocks: Vec<Vec<usize>> = vec![Vec::new(); leader_offsets.len()];
        for (i, insn) in co.instructions.iter().enumerate() {
            let bi = leader_offsets
                .iter()
                .rposition(|&lo| lo <= insn.offset)
                .unwrap_or(0);
            blocks[bi].push(i);
        }

        // Exception handlers begin with `PUSH_EXC_INFO` (3.11+); a `raise` routes to
        // them (the exception-table edge is not carried in `jump_targets`).
        self.handler_blocks = blocks
            .iter()
            .enumerate()
            .filter(|(_, ins)| {
                ins.first()
                    .map(|&ii| co.instructions[ii].opname == "PUSH_EXC_INFO")
                    .unwrap_or(false)
            })
            .map(|(bi, _)| block_ids[bi])
            .collect();

        // <=3.11 has no `PUSH_EXC_INFO`: a handler is the jump target of a `SETUP_*`
        // and the interpreter pushes the exception triple (traceback, value, type)
        // before entering it. Route `raise` to those blocks too, seeding each with
        // the in-flight exception (`<exc>`) so `except E as e`'s `STORE_FAST` binds
        // the tainted value. `entry_stacks` is seeded below, once it exists.
        let setup_handler_offsets: BTreeSet<i64> = co
            .instructions
            .iter()
            .filter(|i| is_setup_exception(i.opname.as_str()))
            .flat_map(|i| i.jump_targets.iter().copied())
            .collect();

        // Operand-stack values that live across a block boundary (loop
        // accumulators, the `FOR_ITER` iterator, exception values) are threaded
        // through: each block is simulated starting from an entry stack seeded by
        // its first-processed predecessor. Blocks run in ascending-offset order, so
        // a pre-loop predecessor seeds a loop header before the back-edge is seen
        // (first-writer-wins keeps that loop-invariant seed).
        let mut entry_stacks: BTreeMap<BasicBlockIdx, Vec<Slot>> = BTreeMap::new();

        // Seed each <=3.11 `SETUP_*` handler with the exception triple, all modeled
        // as the in-flight exception `<exc>` so however the handler shuffles the
        // triple, the value it binds carries the raised exception's taint.
        for off in &setup_handler_offsets {
            if let Some(&block) = block_of_offset.get(off) {
                let exc =
                    || Slot::val(Exp::Variable(VariableRef::new_local(EXC_LOCAL.to_string())));
                entry_stacks.insert(block, vec![exc(), exc(), exc()]);
                if !self.handler_blocks.contains(&block) {
                    self.handler_blocks.push(block);
                }
            }
        }

        // Offset immediately following the last instruction of each block (its
        // fallthrough successor start), if any.
        for (bi, insn_indices) in blocks.iter().enumerate() {
            let block_idx = block_ids[bi];
            let mut sim =
                StackSim::with_entry(entry_stacks.get(&block_idx).cloned().unwrap_or_default());
            let mut stmts: Vec<Statement> = Vec::new();
            let mut terminator: Option<Terminator> = None;

            for &ii in insn_indices {
                let insn = &co.instructions[ii];
                let si = self.source_info_for(insn);
                let is_last = ii == *insn_indices.last().unwrap();
                if let Some(term) = self.lower_instruction(
                    co,
                    nparams,
                    insn,
                    &mut sim,
                    &mut stmts,
                    si,
                    is_last,
                    &block_of_offset,
                )? {
                    terminator = Some(term);
                }
            }

            // No control-flow terminator emitted: fall through to the next block,
            // else return.
            if terminator.is_none() {
                let last_off = insn_indices
                    .last()
                    .map(|&ii| co.instructions[ii].offset)
                    .unwrap_or(0);
                let next = block_of_offset
                    .range((
                        std::ops::Bound::Excluded(last_off),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .map(|(_, b)| *b);
                terminator = Some(match next {
                    Some(b) => Terminator::new_kind(TerminatorKind::Goto {
                        targets: smallvec![b],
                    }),
                    None => Terminator::new_kind(TerminatorKind::Return {
                        args: smallvec![Exp::new_bytes(Vec::new())],
                    }),
                });
            }

            // Thread this block's exit stack to each successor that has not yet
            // been seeded (first-writer-wins, so a loop header keeps its pre-loop
            // seed rather than the back-edge's).
            if let Some(Terminator {
                kind: TerminatorKind::Goto { targets },
                ..
            }) = &terminator
            {
                let exit = sim.snapshot();
                for succ in targets {
                    // A per-edge override (e.g. a `FOR_ITER` exhaust edge that popped
                    // the iterator) wins over the predecessor's plain exit snapshot.
                    let seed = self
                        .block_entry_overrides
                        .get(succ)
                        .cloned()
                        .unwrap_or_else(|| exit.clone());
                    entry_stacks.entry(*succ).or_insert(seed);
                }
            }

            let block = &mut self.program[func_idx].blocks.blocks_mut()[block_idx];
            for s in stmts {
                block.push_back(s);
            }
            block.terminator = terminator;
        }

        Ok(())
    }

    /// Lower one instruction, mutating the stack `sim` and appending statements.
    /// Returns a terminator if this instruction ends the block with control flow
    /// (return/jump); `None` means fall through.
    #[allow(clippy::too_many_arguments)]
    fn lower_instruction(
        &mut self,
        co: &CodeObject,
        nparams: usize,
        insn: &Instruction,
        sim: &mut StackSim,
        stmts: &mut Vec<Statement>,
        si: SourceInfo,
        is_last: bool,
        block_of_offset: &BTreeMap<i64, BasicBlockIdx>,
    ) -> Result<Option<Terminator>, Error> {
        let op = insn.opname.as_str();

        // A helper to compute the Goto terminator from resolved jump targets plus
        // (for conditional jumps) the fallthrough.
        let goto_from = |targets: &[i64], fallthrough: bool| -> Terminator {
            let mut out: SmallVec<[BasicBlockIdx; 4]> = smallvec![];
            for t in targets {
                if let Some(b) = block_of_offset.get(t)
                    && !out.contains(b)
                {
                    out.push(*b);
                }
            }
            if fallthrough
                && let Some((_, b)) = block_of_offset
                    .range((
                        std::ops::Bound::Excluded(insn.offset),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                && !out.contains(b)
            {
                out.push(*b);
            }
            Terminator::new_kind(TerminatorKind::Goto { targets: out })
        };

        match op {
            // --- No stack effect ---
            "RESUME" | "NOP" | "PRECALL" | "CACHE" | "MAKE_CELL" | "COPY_FREE_VARS"
            | "KW_NAMES" | "EXTENDED_ARG" | "RETURN_GENERATOR" | "SETUP_ANNOTATIONS"
            | "POP_BLOCK" | "POP_EXCEPT" | "NOT_TAKEN" => {}

            // Entry of an exception handler: push the in-flight exception so the
            // handler's `STORE_FAST` (`except E as e`) binds it. Connected to the
            // `raise` via the `<exc>` local.
            "PUSH_EXC_INFO" => {
                let ex = VariableRef::new_local(EXC_LOCAL.to_string());
                sim.push(Slot::val(Exp::Variable(ex)));
            }

            // --- Loads of closure cells (shared with nested scopes) ---
            // `LOAD_CLOSURE` (<=3.12) pushes a cell to build the closure tuple for a
            // nested `MAKE_FUNCTION`; 3.13+ uses a plain `LOAD_FAST` of the cell
            // instead. Model both as loading the cell's value: leaving `LOAD_CLOSURE`
            // unhandled underflows the stack (the following `BUILD_TUPLE` pops an
            // absent operand) and drops the captured value.
            "LOAD_DEREF" | "LOAD_CLASSDEREF" | "LOAD_CLOSURE" => {
                let name = self.deref_name(co, insn);
                let v = self.load_cell(stmts, &name, si);
                sim.push(Slot::val(Exp::Variable(v)));
            }

            // --- Loads of locals ---
            "LOAD_FAST" | "LOAD_FAST_CHECK" | "LOAD_FAST_AND_CLEAR" | "LOAD_FAST_BORROW" => {
                let idx = insn.arg.unwrap_or(0) as usize;
                // A fast index beyond `varnames` addresses a cell variable in its
                // defining scope: route it to the shared cell namespace.
                if idx >= co.varnames.len() {
                    let name = self.deref_name(co, insn);
                    let v = self.load_cell(stmts, &name, si);
                    sim.push(Slot::val(Exp::Variable(v)));
                } else {
                    let v = self.local_ref(co, nparams, idx);
                    // Carry a function-pointer name so a call through this local
                    // resolves to a direct call.
                    let fname = co
                        .varnames
                        .get(idx)
                        .and_then(|n| self.func_ptr_locals.get(n).cloned());
                    sim.push(Slot::named(Exp::Variable(v), fname));
                }
            }
            "LOAD_FAST_LOAD_FAST" | "LOAD_FAST_BORROW_LOAD_FAST_BORROW" => {
                let arg = insn.arg.unwrap_or(0) as usize;
                let a = self.local_ref(co, nparams, arg >> 4);
                let b = self.local_ref(co, nparams, arg & 0xF);
                sim.push(Slot::val(Exp::Variable(a)));
                sim.push(Slot::val(Exp::Variable(b)));
            }

            // --- Stores of closure cells (shared with nested scopes) ---
            "STORE_DEREF" => {
                let name = self.deref_name(co, insn);
                let v = self.pop_exp(sim);
                self.store_cell(stmts, &name, v, si);
            }

            // --- Stores of locals ---
            "STORE_FAST" | "STORE_FAST_MAYBE_NULL" => {
                let idx = insn.arg.unwrap_or(0) as usize;
                let slot = self.pop_slot(sim);
                if idx >= co.varnames.len() {
                    let name = self.deref_name(co, insn);
                    self.store_cell(stmts, &name, slot.exp, si);
                } else {
                    let dest = self.local_ref(co, nparams, idx);
                    self.track_func_ptr_local(co, idx, &slot);
                    stmts.push(Statement::new(StatementKind::assign(dest, [slot.exp]), si));
                }
            }
            "STORE_FAST_STORE_FAST" => {
                let arg = insn.arg.unwrap_or(0) as usize;
                let d1 = self.local_ref(co, nparams, arg >> 4);
                let d2 = self.local_ref(co, nparams, arg & 0xF);
                let v1 = self.pop_exp(sim);
                let v2 = self.pop_exp(sim);
                stmts.push(Statement::new(StatementKind::assign(d1, [v1]), si));
                stmts.push(Statement::new(StatementKind::assign(d2, [v2]), si));
            }
            "STORE_FAST_LOAD_FAST" => {
                let arg = insn.arg.unwrap_or(0) as usize;
                let d = self.local_ref(co, nparams, arg >> 4);
                let l = self.local_ref(co, nparams, arg & 0xF);
                let v = self.pop_exp(sim);
                stmts.push(Statement::new(StatementKind::assign(d, [v]), si));
                sim.push(Slot::val(Exp::Variable(l)));
            }

            // --- Return of a const ---
            "RETURN_CONST" => {
                let exp = self.const_exp(co, insn);
                return Ok(Some(self.return_terminator(exp)));
            }

            // --- Loads of consts ---
            "LOAD_CONST" | "LOAD_CONST_IMMORTAL" | "LOAD_SMALL_INT" => {
                let (exp, name) = self.const_slot(co, insn);
                sim.push(Slot::named(exp, name));
            }

            // --- Loads of globals / names ---
            "LOAD_GLOBAL" | "LOAD_NAME" => {
                let name = self.name_operand(co, insn);
                // 3.11+: `LOAD_GLOBAL` pushes a NULL alongside the callable (for the
                // following CALL). `dis` marks it in `argrepr` ("NULL + name" on
                // 3.11/3.12, "name + NULL" on 3.13+); <=3.10 pushes no NULL and the
                // `arg` is a plain name index, so the low bit is meaningless there.
                // Keying on the `argrepr` marker keeps this version-robust — a low-bit
                // test spuriously fires for any odd name index on 3.9/3.10.
                if op == "LOAD_GLOBAL" && pushes_null_marker(insn) {
                    sim.push(Slot::null());
                }
                let v = self.load_global(stmts, &name, si);
                sim.push(Slot::named(Exp::Variable(v), Some(name)));
            }
            "LOAD_BUILD_CLASS" => {
                sim.push(Slot::named(
                    Exp::new_str("__build_class__"),
                    Some("__build_class__".to_string()),
                ));
            }

            // --- Stores of globals / names ---
            "STORE_GLOBAL" | "STORE_NAME" => {
                let name = self.name_operand(co, insn);
                let v = self.pop_exp(sim);
                self.store_global(stmts, &name, v, si);
            }
            "DELETE_GLOBAL" | "DELETE_NAME" | "DELETE_FAST" | "DELETE_DEREF" => {}

            // --- Attributes ---
            "LOAD_ATTR" => {
                let name = self.name_operand(co, insn);
                let obj = self.pop_exp(sim);
                let obj_var = self.as_variable(stmts, obj, si);
                let loaded = self.load_attr(stmts, obj_var.clone(), &name, si);
                // 3.12+ merged `LOAD_METHOD` into `LOAD_ATTR`: a method load pushes
                // self alongside the method for the following CALL, marked in
                // `argrepr` ("NULL|self"). <=3.11 keeps a separate `LOAD_METHOD` and
                // `LOAD_ATTR`'s `arg` is a plain name index, so the low bit is
                // meaningless there — key on the marker instead.
                if pushes_null_marker(insn) {
                    sim.push(Slot::val(Exp::Variable(obj_var)));
                }
                sim.push(Slot::named(Exp::Variable(loaded), Some(name)));
            }
            "LOAD_METHOD" => {
                let name = self.name_operand(co, insn);
                let obj = self.pop_exp(sim);
                let obj_var = self.as_variable(stmts, obj, si);
                let loaded = self.load_attr(stmts, obj_var.clone(), &name, si);
                sim.push(Slot::val(Exp::Variable(obj_var)));
                sim.push(Slot::named(Exp::Variable(loaded), Some(name)));
            }
            "STORE_ATTR" => {
                let name = self.name_operand(co, insn);
                let obj = self.pop_exp(sim);
                let value = self.pop_exp(sim);
                let obj_var = self.as_variable(stmts, obj, si);
                self.store_attr(stmts, obj_var, &name, value, si);
            }
            "DELETE_ATTR" => {
                self.pop_exp(sim);
            }

            // --- Subscript (container[key]) ---
            // 3.14 merged `BINARY_SUBSCR` into `BINARY_OP` with a subscript operator
            // (`argrepr == "[]"`); route it here rather than to the generic binary op
            // so the element is read from the container's `.item` field.
            "BINARY_SUBSCR" | "BINARY_SLICE" | "BINARY_OP"
                if op != "BINARY_OP" || insn.argrepr.as_deref() == Some("[]") =>
            {
                let _key = self.pop_exp(sim);
                let container = self.pop_exp(sim);
                let cvar = self.as_variable(stmts, container, si);
                let loaded = self.load_attr(stmts, cvar, ITEM_FIELD, si);
                sim.push(Slot::val(Exp::Variable(loaded)));
            }
            "STORE_SUBSCR" | "STORE_SLICE" => {
                let _key = self.pop_exp(sim);
                let container = self.pop_exp(sim);
                let value = self.pop_exp(sim);
                let cvar = self.as_variable(stmts, container, si);
                self.store_attr(stmts, cvar, ITEM_FIELD, value, si);
            }

            // --- Iteration ---
            "GET_ITER" | "GET_AITER" | "GET_YIELD_FROM_ITER" => {
                let it = self.pop_exp(sim);
                let v = self.as_variable(stmts, it, si);
                sim.push(Slot::val(Exp::Variable(v)));
            }
            "FOR_ITER" => {
                // The yielded element is item(iter). On the loop-body (continue)
                // edge the iterator stays and the element is pushed on top. On the
                // loop-exhaust edge the stack is version-dependent: 3.12+ keeps both
                // iterator and element (cleaned up by `END_FOR` [+ `POP_TOP`/
                // `POP_ITER`] at the exit target); <=3.11 pops the iterator and
                // pushes nothing. Give the exit block that popped stack directly.
                if self.for_exhaust_pops_iter {
                    let mut exit_stack = sim.snapshot();
                    exit_stack.pop(); // the iterator, gone on the exhaust edge
                    for t in &insn.jump_targets {
                        if let Some(&b) = block_of_offset.get(t) {
                            self.block_entry_overrides.insert(b, exit_stack.clone());
                        }
                    }
                }
                let iter = sim.peek_exp(&mut || self.fresh());
                let ivar = self.as_variable(stmts, iter, si);
                let elt = self.load_attr(stmts, ivar, ITEM_FIELD, si);
                sim.push(Slot::val(Exp::Variable(elt)));
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, true)));
                }
            }
            // Cleans up after a 3.12+ `FOR_ITER` loop. The element the exhaust edge
            // carries is always popped; the iterator is popped here too on 3.12
            // (`END_FOR` stack_effect -2), but on 3.13+ (-1) a following
            // `POP_TOP`/`POP_ITER` pops it instead. Detect that by the next opcode so
            // the post-loop stack stays aligned and later stores bind correctly.
            "END_FOR" => {
                self.pop_exp(sim);
                let next_pops_iter = co
                    .instructions
                    .iter()
                    .find(|i| i.offset > insn.offset)
                    .map(|i| matches!(i.opname.as_str(), "POP_TOP" | "POP_ITER"))
                    .unwrap_or(false);
                if !next_pops_iter {
                    self.pop_exp(sim);
                }
            }
            "END_ASYNC_FOR" => {
                self.pop_exp(sim);
            }
            // 3.14 iterator cleanup after a loop; pops the iterator `FOR_ITER` left.
            "POP_ITER" => {
                self.pop_exp(sim);
            }

            // --- Container building ---
            // A collection literal models each element as living in the container's
            // shared `.item` field, so a later subscript / unpack / iteration
            // (which all read `.item`) sees the element's taint.
            "BUILD_TUPLE" | "BUILD_LIST" | "BUILD_SET" => {
                let n = insn.arg.unwrap_or(0) as usize;
                let elts = self.pop_n(sim, n);
                let tmp = self.new_container(stmts, si);
                for e in elts {
                    self.store_attr(stmts, tmp.clone(), ITEM_FIELD, e, si);
                }
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            // A string join (f-strings, `str.join`-style concatenation) taints the
            // whole result, not an element field: keep it a direct assignment so the
            // concatenated value itself carries the taint.
            "BUILD_STRING" => {
                let n = insn.arg.unwrap_or(0) as usize;
                let elts = self.pop_n(sim, n);
                let tmp = self.fresh();
                stmts.push(Statement::new(StatementKind::assign(tmp.clone(), elts), si));
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            "BUILD_MAP" => {
                // n key/value pairs; only the values carry data-flow of interest.
                let n = insn.arg.unwrap_or(0) as usize;
                let elts = self.pop_n(sim, n * 2);
                let tmp = self.new_container(stmts, si);
                for pair in elts.chunks(2) {
                    if let [_key, value] = pair {
                        self.store_attr(stmts, tmp.clone(), ITEM_FIELD, value.clone(), si);
                    }
                }
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            "BUILD_CONST_KEY_MAP" => {
                // n values on the stack, then a constant tuple of keys on top.
                let n = insn.arg.unwrap_or(0) as usize;
                let _keys = self.pop_exp(sim);
                let vals = self.pop_n(sim, n);
                let tmp = self.new_container(stmts, si);
                for v in vals {
                    self.store_attr(stmts, tmp.clone(), ITEM_FIELD, v, si);
                }
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            "LIST_APPEND" | "SET_ADD" => {
                let i = insn.arg.unwrap_or(1) as usize;
                let value = self.pop_exp(sim);
                if let Some(container) = sim.peek_at(i) {
                    let cvar = self.as_variable(stmts, container.exp.clone(), si);
                    self.store_attr(stmts, cvar, ITEM_FIELD, value, si);
                }
            }
            "MAP_ADD" => {
                let i = insn.arg.unwrap_or(1) as usize;
                let value = self.pop_exp(sim);
                let _key = self.pop_exp(sim);
                if let Some(container) = sim.peek_at(i) {
                    let cvar = self.as_variable(stmts, container.exp.clone(), si);
                    self.store_attr(stmts, cvar, ITEM_FIELD, value, si);
                }
            }
            "LIST_EXTEND" | "SET_UPDATE" | "DICT_UPDATE" | "DICT_MERGE" => {
                let value = self.pop_exp(sim);
                let i = insn.arg.unwrap_or(1) as usize;
                if let Some(container) = sim.peek_at(i) {
                    let cvar = self.as_variable(stmts, container.exp.clone(), si);
                    self.store_attr(stmts, cvar, ITEM_FIELD, value, si);
                }
            }

            // --- Unpacking ---
            "UNPACK_SEQUENCE" => {
                let n = insn.arg.unwrap_or(0) as usize;
                let seq = self.pop_exp(sim);
                let svar = self.as_variable(stmts, seq, si);
                // Each unpacked element is item(seq). Push n of them.
                for _ in 0..n {
                    let elt = self.load_attr(stmts, svar.clone(), ITEM_FIELD, si);
                    sim.push(Slot::val(Exp::Variable(elt)));
                }
            }

            // --- Function creation ---
            "MAKE_FUNCTION" => {
                // The code object is the slot that must survive as the function
                // pointer, but its stack position is version-dependent:
                //   <=3.10: [defaults?, kwdefaults?, annotations?, closure?, code,
                //           qualname]  — qualname on top, extras below the code.
                //   3.11/3.12: [ ...extras, code]  — code on top, extras below.
                //   3.13+: [code]  — `arg` is absent; closure etc. arrive later via
                //           SET_FUNCTION_ATTRIBUTE.
                // In every case the flag extras sit *below* the code, so pop the
                // qualname (<=3.10 only) and the code off the top, then the extras.
                let extra = insn
                    .arg
                    .map(|a| (a as u32).count_ones() as usize)
                    .unwrap_or(0);
                if self.pre_3_11 {
                    self.pop_exp(sim); // qualname
                }
                let code_slot = sim.pop(&mut || self.fresh());
                self.pop_n(sim, extra); // defaults/kwdefaults/annotations/closure
                sim.push(code_slot);
            }
            "SET_FUNCTION_ATTRIBUTE" => {
                // TOS is the function, TOS1 the attribute value (defaults, closure,
                // …). Pop the value, keep the function on the stack.
                let func = self.pop_slot(sim);
                self.pop_exp(sim);
                sim.push(func);
            }

            // --- Calls ---
            "CALL" | "CALL_FUNCTION" | "CALL_METHOD" | "CALL_KW" | "CALL_FUNCTION_KW" => {
                let argc = insn.arg.unwrap_or(0) as usize;
                self.lower_call(op, argc, sim, stmts, si);
            }
            "CALL_FUNCTION_EX" => {
                self.lower_call_ex(insn, sim, stmts, si);
            }

            // --- Copies / stack shuffles ---
            "COPY" => {
                let i = insn.arg.unwrap_or(1) as usize;
                let slot = sim
                    .peek_at(i)
                    .cloned()
                    .unwrap_or_else(|| Slot::val(Exp::Variable(self.fresh())));
                sim.push(slot);
            }
            "DUP_TOP" => {
                let slot = sim
                    .peek_at(1)
                    .cloned()
                    .unwrap_or_else(|| Slot::val(Exp::Variable(self.fresh())));
                sim.push(slot);
            }
            "SWAP" => {
                let i = insn.arg.unwrap_or(1) as usize;
                sim.swap(i);
            }
            "POP_TOP" => {
                self.pop_exp(sim);
            }
            "PUSH_NULL" => sim.push(Slot::null()),

            // --- Yields ---
            // Each yielded value becomes an element of the generator-result object;
            // the value the resumed `yield` evaluates to (a `.send()` argument) is
            // modeled as an unknown fresh value.
            "YIELD_VALUE" => {
                let v = self.pop_exp(sim);
                let gr = VariableRef::new_local(GEN_RESULT.to_string());
                self.store_attr(stmts, gr, ITEM_FIELD, v, si);
                sim.push(Slot::val(Exp::Variable(self.fresh())));
            }

            // --- Returns / raises ---
            "RETURN_VALUE" => {
                let v = self.pop_exp(sim);
                return Ok(Some(self.return_terminator(v)));
            }
            "RAISE_VARARGS" | "RERAISE" => {
                let n = if op == "RAISE_VARARGS" {
                    insn.arg.unwrap_or(0) as usize
                } else {
                    0
                };
                let popped = self.pop_n(sim, n);
                // `raise exc` / `raise exc from cause`: the exception object is the
                // first-pushed operand. Store it into `<exc>` so a handler recovers
                // it, since the raise→handler edge is not in `jump_targets`.
                if let Some(exc) = popped.first() {
                    let ex = VariableRef::new_local(EXC_LOCAL.to_string());
                    stmts.push(Statement::new(StatementKind::assign(ex, [exc.clone()]), si));
                }
                // Route to this function's handlers so their blocks are reachable
                // and `<exc>` flows in; fall back to a return when there are none.
                if self.handler_blocks.is_empty() {
                    return Ok(Some(Terminator::new_kind(TerminatorKind::Return {
                        args: smallvec![Exp::new_bytes(Vec::new())],
                    })));
                }
                return Ok(Some(Terminator::new_kind(TerminatorKind::Goto {
                    targets: self.handler_blocks.iter().copied().collect(),
                })));
            }

            // --- Imports ---
            // `IMPORT_NAME` pops the fromlist (TOS) and level (TOS1) and pushes the
            // imported module; model the module as a global of that name.
            "IMPORT_NAME" => {
                let name = self.name_operand(co, insn);
                self.pop_exp(sim); // fromlist
                self.pop_exp(sim); // level
                let module = self.load_global(stmts, &name, si);
                sim.push(Slot::named(Exp::Variable(module), Some(name)));
            }
            // `IMPORT_FROM` reads an attribute of the module on top of the stack
            // *without* popping it (the module stays for a following `IMPORT_FROM`
            // or its cleanup `POP_TOP`). Like `LOAD_ATTR`, but non-consuming.
            "IMPORT_FROM" => {
                let name = self.name_operand(co, insn);
                let module = sim.peek_exp(&mut || self.fresh());
                let mvar = self.as_variable(stmts, module, si);
                let loaded = self.load_attr(stmts, mvar, &name, si);
                sim.push(Slot::named(Exp::Variable(loaded), Some(name)));
            }
            // `from m import *`: pop the module; the names it binds are unmodeled.
            "IMPORT_STAR" => {
                self.pop_exp(sim);
            }
            // Pushes the `AssertionError` builtin for a failing `assert`.
            "LOAD_ASSERTION_ERROR" => {
                sim.push(Slot::named(
                    Exp::new_str("AssertionError"),
                    Some("AssertionError".to_string()),
                ));
            }

            // --- With statements (3.11+) ---
            // `BEFORE_WITH` / `BEFORE_ASYNC_WITH` replace the context manager with
            // its `__exit__` (kept below for the exception path) and push the
            // `__enter__` result (which a following `STORE_FAST` binds to the `as`
            // target). Model the entered value as derived from the manager so taint
            // flows through `with mgr as x`.
            "BEFORE_WITH" | "BEFORE_ASYNC_WITH" => {
                let mgr = self.pop_exp(sim);
                let mvar = self.as_variable(stmts, mgr, si);
                let exit = self.load_attr(stmts, mvar.clone(), "__exit__", si);
                let entered = self.load_attr(stmts, mvar, "__enter__", si);
                sim.push(Slot::val(Exp::Variable(exit)));
                sim.push(Slot::val(Exp::Variable(entered)));
            }
            // On the exception path of a `with`, call `__exit__(type, val, tb)` and
            // push its result. Consumes nothing (the exit fn and exc info are left
            // for the surrounding cleanup); model the result as unknown.
            "WITH_EXCEPT_START" => {
                sim.push(Slot::val(Exp::Variable(self.fresh())));
            }

            // --- Exception handling (3.11+) ---
            // `CHECK_EXC_MATCH` tests the raised exception (TOS1) against a type
            // (TOS): pop the type, push the bool result, leave the exception below.
            "CHECK_EXC_MATCH" => {
                self.pop_exp(sim);
                sim.push(Slot::val(Exp::Variable(self.fresh())));
            }
            // `CLEANUP_THROW` (3.12+) cleans up a generator/coroutine `throw`:
            // pops (sub_iter, last_sent, exc) and pushes (None, value). Keep the
            // stack aligned and derive the resumed value from the exception.
            "CLEANUP_THROW" => {
                let exc = self.pop_exp(sim);
                self.pop_exp(sim); // last_sent
                self.pop_exp(sim); // sub_iter
                let val = self.as_variable(stmts, exc, si);
                sim.push(Slot::val(Exp::new_bytes(Vec::new()))); // None
                sim.push(Slot::val(Exp::Variable(val))); // value
            }

            // --- Subscript delete ---
            // `del container[key]`: pop the key (TOS) and container (TOS1).
            "DELETE_SUBSCR" => {
                self.pop_exp(sim); // key
                self.pop_exp(sim); // container
            }

            // --- super() attribute (3.12+) ---
            // Pops (super, class, self) and pushes the attribute loaded from self.
            // The low oparg bit selects the method form, which additionally pushes
            // self (for the following `CALL`), mirroring `LOAD_METHOD`.
            "LOAD_SUPER_ATTR" => {
                self.pop_exp(sim); // global super
                self.pop_exp(sim); // class
                let obj = self.pop_exp(sim); // self
                let name = self.name_operand(co, insn);
                let obj_var = self.as_variable(stmts, obj, si);
                let loaded = self.load_attr(stmts, obj_var.clone(), &name, si);
                if insn.arg.unwrap_or(0) & 1 != 0 {
                    sim.push(Slot::val(Exp::Variable(obj_var)));
                }
                sim.push(Slot::named(Exp::Variable(loaded), Some(name)));
            }

            // --- Intrinsics (3.12+) ---
            // `CALL_INTRINSIC_1` is a unary intrinsic (import-star, typevar,
            // list-to-tuple, unary +/-, …); `CALL_INTRINSIC_2` a binary one. Model
            // each as data-flow through a temp so taint passes through.
            "CALL_INTRINSIC_1" => {
                let a = self.pop_exp(sim);
                let tmp = self.fresh();
                stmts.push(Statement::new(StatementKind::assign(tmp.clone(), [a]), si));
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            "CALL_INTRINSIC_2" => {
                let b = self.pop_exp(sim);
                let a = self.pop_exp(sim);
                let tmp = self.fresh();
                stmts.push(Statement::new(
                    StatementKind::assign(tmp.clone(), [a, b]),
                    si,
                ));
                sim.push(Slot::val(Exp::Variable(tmp)));
            }

            // --- Async / generators ---
            // `GET_AWAITABLE` replaces TOS with its awaitable; the awaited result
            // flows through, so preserve the value.
            "GET_AWAITABLE" => {
                let a = self.pop_exp(sim);
                let v = self.as_variable(stmts, a, si);
                sim.push(Slot::val(Exp::Variable(v)));
            }
            // `END_SEND` (3.12+): `receiver, value -> value`; drop the receiver.
            "END_SEND" => {
                let value = self.pop_slot(sim);
                self.pop_exp(sim); // receiver
                sim.push(value);
            }

            // --- Slice construction ---
            // `BUILD_SLICE` pops `arg` bounds (2 or 3) and pushes the slice object.
            // A slice holds index bounds, not element data, so a fresh value suffices.
            "BUILD_SLICE" => {
                let n = insn.arg.unwrap_or(2) as usize;
                self.pop_n(sim, n);
                sim.push(Slot::val(Exp::Variable(self.fresh())));
            }
            // `UNPACK_EX` (`a, *b, c = seq`): pop the sequence and push
            // (before + 1 + after) targets, each modeled as an element (`.item`).
            "UNPACK_EX" => {
                let arg = insn.arg.unwrap_or(0) as usize;
                let count = (arg & 0xFF) + 1 + (arg >> 8);
                let seq = self.pop_exp(sim);
                let svar = self.as_variable(stmts, seq, si);
                for _ in 0..count {
                    let elt = self.load_attr(stmts, svar.clone(), ITEM_FIELD, si);
                    sim.push(Slot::val(Exp::Variable(elt)));
                }
            }

            // --- Class-body / comprehension locals (3.12+) ---
            // `LOAD_LOCALS` pushes the mapping of local names (used before
            // `LOAD_FROM_DICT_OR_DEREF` in class bodies and comprehensions).
            "LOAD_LOCALS" => {
                sim.push(Slot::val(Exp::Variable(self.fresh())));
            }
            // `LOAD_FROM_DICT_OR_DEREF` pops that mapping and pushes the cell/free
            // variable's value (the name resolves like a `*_DEREF` operand).
            "LOAD_FROM_DICT_OR_DEREF" => {
                self.pop_exp(sim); // the locals mapping
                let name = self.deref_name(co, insn);
                let v = self.load_cell(stmts, &name, si);
                sim.push(Slot::val(Exp::Variable(v)));
            }

            // --- Jumps ---
            "JUMP_FORWARD" | "JUMP_BACKWARD" | "JUMP_ABSOLUTE" | "JUMP_BACKWARD_NO_INTERRUPT" => {
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, false)));
                }
            }
            // <=3.10 exception-type test: compares the raised exception against the
            // handler's type, popping both, and branches to the next handler on a
            // mismatch (falling through into this handler's body on a match).
            "JUMP_IF_NOT_EXC_MATCH" => {
                self.pop_exp(sim);
                self.pop_exp(sim);
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, true)));
                }
            }

            // --- Conditional jumps (fall through + branch) ---
            _ if is_conditional_jump(op) => {
                // These pop a condition value (POP_JUMP_IF_*) or peek (JUMP_IF_*_OR_POP).
                if op.starts_with("POP_JUMP") {
                    self.pop_exp(sim);
                }
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, true)));
                }
            }

            // --- Everything else ---
            _ => {
                self.lower_generic(op, insn, sim, stmts, si);
            }
        }
        Ok(None)
    }

    /// A generic fallback for arithmetic / comparison / unary ops and unknowns.
    /// Recognized binary/unary shapes pop the right number of operands and push a
    /// derived temporary (so taint flows through); truly unknown opcodes are left
    /// as no-ops with a warning.
    fn lower_generic(
        &mut self,
        op: &str,
        insn: &Instruction,
        sim: &mut StackSim,
        stmts: &mut Vec<Statement>,
        si: SourceInfo,
    ) {
        if is_binary_op(op) {
            let b = self.pop_exp(sim);
            let a = self.pop_exp(sim);
            let tmp = self.fresh();
            stmts.push(Statement::new(
                StatementKind::assign(tmp.clone(), [a, b]),
                si,
            ));
            sim.push(Slot::val(Exp::Variable(tmp)));
        } else if is_unary_op(op) {
            let a = self.pop_exp(sim);
            let tmp = self.fresh();
            stmts.push(Statement::new(StatementKind::assign(tmp.clone(), [a]), si));
            sim.push(Slot::val(Exp::Variable(tmp)));
        } else {
            log::warn!(
                "python frontend: unhandled opcode {op} (arg {:?}); treating as no-op",
                insn.arg
            );
        }
    }

    /// Lower a call: pop `argc` args plus the callable and (3.11+) a self/NULL
    /// slot, resolve the callee, and push a result temporary.
    fn lower_call(
        &mut self,
        op: &str,
        argc: usize,
        sim: &mut StackSim,
        stmts: &mut Vec<Statement>,
        si: SourceInfo,
    ) {
        // Stack shape by version: 3.11+ `CALL` has [callable, self_or_null, args];
        // `CALL_KW` also has a trailing kwnames tuple; older `CALL_FUNCTION` has
        // no self/null slot.
        let extra_below = match op {
            "CALL" | "CALL_METHOD" | "CALL_KW" | "CALL_FUNCTION_KW" => 2,
            _ => 1, // CALL_FUNCTION (<=3.10)
        };
        let total = argc + extra_below;
        // Pop everything the call consumes, deepest-first once reversed.
        let mut popped = self.pop_n_slots(sim, total);
        popped.reverse();

        // The callee is the first slot carrying a name (a global/method/function
        // pointer). When none is named — a call through a local holding a function
        // (`f = ...; f()`) — the callable is the deepest non-NULL slot (its fixed
        // position in the `[callable, self_or_null, args...]` layout); an indirect
        // call through that value then resolves via the function pointer it holds.
        // NULL slots are dropped; the remaining non-null slots are the arguments.
        let callee_pos = popped
            .iter()
            .position(|s| s.name.is_some())
            .or_else(|| popped.iter().position(|s| !s.is_null));
        let callee = callee_pos.map(|p| popped[p].clone());
        let mut args: ctadl_ir::ThinVec<Exp> = ctadl_ir::ThinVec::new();
        for (i, slot) in popped.iter().enumerate() {
            if Some(i) == callee_pos || slot.is_null {
                continue;
            }
            args.push(slot.exp.clone());
        }

        // A method call carries a non-NULL receiver in the slot just below the
        // callee (a plain function call has a NULL there instead). For the common
        // container-mutating methods we model the element flow into the receiver's
        // `.item` field, which is how the value later comes back out via
        // subscript/iteration. The call itself is still emitted below so any
        // interprocedural summary (user-defined methods) also applies.
        if let (Some(cpos), Some(name)) = (callee_pos, callee.as_ref().and_then(|c| c.name.clone()))
            && cpos >= 1
            && !popped[cpos - 1].is_null
        {
            let receiver = popped[cpos - 1].exp.clone();
            // The actual arguments in push order (receiver and callee excluded).
            let call_args: Vec<Exp> = popped[cpos + 1..]
                .iter()
                .filter(|s| !s.is_null)
                .map(|s| s.exp.clone())
                .collect();
            self.model_container_method(&name, receiver, &call_args, stmts, si);
        }

        // Instantiating a class: a call whose callee names a class (its body
        // function) is lowered as `instance = new; __init__(instance, args...)`,
        // and the call's result is the instance (so `self.x = ...` in `__init__`
        // is visible on the constructed object). The class body itself only
        // installs methods, so running it is unnecessary for data flow.
        if let Some(name) = callee.as_ref().and_then(|c| c.name.clone())
            && let Some(init) = self.class_init.get(&name).cloned()
        {
            let instance = self.new_container(stmts, si);
            let mut init_args: ctadl_ir::ThinVec<Exp> =
                ctadl_ir::thin_vec![Exp::Variable(instance.clone())];
            init_args.extend(args.iter().cloned());
            let style = CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(ctadl_ir::thin_vec![init]),
            };
            let ret = self.fresh();
            stmts.push(Statement::new(
                StatementKind::CallAssign {
                    style,
                    rets: ctadl_ir::thin_vec![ret],
                    args: init_args,
                },
                si,
            ));
            sim.push(Slot::val(Exp::Variable(instance)));
            return;
        }

        let style = match callee {
            Some(slot) => self.call_style_for(slot),
            None => CallStyle::FuncPtrCall {
                callee: AccessPath::without_fields(self.fresh()),
                signature: None,
            },
        };
        // Record the arg count against any direct-call name for external stubs.
        self.record_external_call(&style, args.len());

        let ret = self.fresh();
        stmts.push(Statement::new(
            StatementKind::CallAssign {
                style,
                rets: ctadl_ir::thin_vec![ret.clone()],
                args,
            },
            si,
        ));
        sim.push(Slot::val(Exp::Variable(ret)));
    }

    /// Lower `CALL_FUNCTION_EX` (a call with splatted `*args` / `**kwargs`).
    ///
    /// The stack, top-down, is `[kwargs?, args_iterable, callable, self_or_NULL]`.
    /// Since a splatted value's binding to a specific positional/keyword parameter
    /// is not recoverable here, we collapse the args iterable, the kwargs mapping,
    /// and each one's element (`.item`) into a single aggregate and route it into
    /// the callee's first parameter — imprecise, but it preserves reachability.
    fn lower_call_ex(
        &mut self,
        insn: &Instruction,
        sim: &mut StackSim,
        stmts: &mut Vec<Statement>,
        si: SourceInfo,
    ) {
        // Whether a kwargs slot sits on top of the args iterable.
        //   <=3.13: `CALL_FUNCTION_EX` carries a flags oparg; bit 0 marks the slot.
        //   3.14: the oparg is gone and the slot is *always* present — a dict, or a
        //   NULL (pushed via `PUSH_NULL`) when the call has no keyword splat.
        let has_kwargs = match insn.arg {
            None => true,
            Some(a) => a & 1 == 1,
        };
        // The mapping may be a NULL placeholder (3.14 no-kwargs); pop it either way,
        // but only treat a real value as a taint contributor.
        let kwargs = if has_kwargs {
            let slot = self.pop_slot(sim);
            (!slot.is_null).then_some(slot.exp)
        } else {
            None
        };
        let args_iter = self.pop_exp(sim);
        let callee = self.pop_slot(sim);
        let _self_or_null = self.pop_slot(sim);

        let mut contributors: Vec<Exp> = vec![args_iter.clone()];
        let ai = self.as_variable(stmts, args_iter, si);
        let ai_item = self.load_attr(stmts, ai, ITEM_FIELD, si);
        contributors.push(Exp::Variable(ai_item));
        if let Some(kw) = kwargs {
            contributors.push(kw.clone());
            let kv = self.as_variable(stmts, kw, si);
            let kv_item = self.load_attr(stmts, kv, ITEM_FIELD, si);
            contributors.push(Exp::Variable(kv_item));
        }
        let merged = self.fresh();
        stmts.push(Statement::new(
            StatementKind::assign(merged.clone(), contributors.clone()),
            si,
        ));
        // Also expose the aggregate's taint via `.item` so a callee that subscripts
        // its collected `*args` recovers it.
        for c in contributors {
            self.store_attr(stmts, merged.clone(), ITEM_FIELD, c, si);
        }

        let style = self.call_style_for(callee);
        self.record_external_call(&style, 1);
        let ret = self.fresh();
        stmts.push(Statement::new(
            StatementKind::CallAssign {
                style,
                rets: ctadl_ir::thin_vec![ret.clone()],
                args: ctadl_ir::thin_vec![Exp::Variable(merged)],
            },
            si,
        ));
        sim.push(Slot::val(Exp::Variable(ret)));
    }

    /// Record a direct call's argument count so a bodyless external stub with
    /// enough parameters is synthesized for a called-but-undefined name.
    fn record_external_call(&mut self, style: &CallStyle, argc: usize) {
        if let CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(edges),
        } = style
            && let Some(name) = edges.first()
        {
            let entry = self.external_calls.entry(name.to_string()).or_insert(0);
            *entry = (*entry).max(argc);
        }
    }

    /// Model a call to a well-known container-mutating method by routing the added
    /// element(s) into the receiver's shared `.item` field, so a later
    /// subscript/iteration recovers the taint. Unknown method names are ignored
    /// here (the ordinary call is still emitted by the caller).
    fn model_container_method(
        &mut self,
        name: &str,
        receiver: Exp,
        args: &[Exp],
        stmts: &mut Vec<Statement>,
        si: SourceInfo,
    ) {
        let rvar = self.as_variable(stmts, receiver, si);
        match name {
            // Single value appended/added: `list.append`, `set.add`,
            // `deque.appendleft`, `set.discard`.
            "append" | "add" | "appendleft" | "discard" => {
                if let Some(v) = args.first() {
                    self.store_attr(stmts, rvar, ITEM_FIELD, v.clone(), si);
                }
            }
            // `list.insert(i, value)`: the value is the last argument.
            "insert" => {
                if let Some(v) = args.last() {
                    self.store_attr(stmts, rvar, ITEM_FIELD, v.clone(), si);
                }
            }
            // Bulk merge from another container: pull the source's elements
            // (`other.item`) into the receiver's `.item`.
            "extend" | "extendleft" | "update" => {
                if let Some(other) = args.first() {
                    let ovar = self.as_variable(stmts, other.clone(), si);
                    let elt = self.load_attr(stmts, ovar, ITEM_FIELD, si);
                    self.store_attr(stmts, rvar, ITEM_FIELD, Exp::Variable(elt), si);
                }
            }
            _ => {}
        }
    }

    /// The call style for a resolved callee slot: a direct call to a named target,
    /// else an indirect call through the callee value.
    fn call_style_for(&self, callee: Slot) -> CallStyle {
        if let Some(name) = &callee.name {
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(ctadl_ir::thin_vec![name.clone()]),
            }
        } else if let Exp::Variable(v) = callee.exp {
            CallStyle::FuncPtrCall {
                callee: AccessPath::without_fields(v),
                signature: None,
            }
        } else {
            CallStyle::FuncPtrCall {
                callee: AccessPath::without_fields(VariableRef::new_local("<callee>".to_string())),
                signature: None,
            }
        }
    }

    /// Create a bodyless (external) function for every called-but-undefined name,
    /// so JSON models' `find: methods` can match callees like `source` / `sink`.
    fn create_external_stubs(&mut self) {
        let calls: Vec<(String, usize)> = self
            .external_calls
            .iter()
            .filter(|(name, _)| !self.defined_names.contains(name.as_str()))
            .map(|(n, c)| (n.clone(), *c))
            .collect();
        for (name, argc) in calls {
            if self.name_taken(&name) {
                continue;
            }
            let idx = self.program.new_function();
            self.program[idx].set_name(name);
            self.program[idx].set_return_type(ReturnType { arity: 1 });
            for _ in 0..argc {
                self.program[idx]
                    .params
                    .parameters
                    .push(ParameterType::ByRef);
            }
            if argc >= 1 {
                self.emit_external_stub_body(idx, argc);
            }
        }
    }

    /// Give an external stub a conservative taint summary: its return value
    /// carries every argument, so an unknown callee is assumed to derive its
    /// result from its inputs. The result also exposes the arguments as its
    /// elements (`.item`, for container-like builtins) and as its `.args` tuple
    /// (for exception constructors, so `except E as e: e.args[0]` recovers them).
    fn emit_external_stub_body(&mut self, idx: FunctionIdx, argc: usize) {
        let si = self.zero_source_info();
        let params: Vec<Exp> = (0..argc)
            .map(|i| Exp::Variable(VariableRef::new_parameter(ParameterIdx::new(i))))
            .collect();
        let mut stmts: Vec<Statement> = Vec::new();

        let ret = self.fresh();
        // ret aliases the arguments (identity / transfer-style callees).
        stmts.push(Statement::new(
            StatementKind::assign(ret.clone(), params.clone()),
            si,
        ));
        // ret.item := each argument (container-returning callees).
        for p in &params {
            self.store_attr(&mut stmts, ret.clone(), ITEM_FIELD, p.clone(), si);
        }
        // ret.args := (args...) with each argument an element (exceptions).
        let argtuple = self.new_container(&mut stmts, si);
        for p in &params {
            self.store_attr(&mut stmts, argtuple.clone(), ITEM_FIELD, p.clone(), si);
        }
        self.store_attr(
            &mut stmts,
            ret.clone(),
            ARGS_FIELD,
            Exp::Variable(argtuple),
            si,
        );

        let block_idx = self.program[idx].blocks.new_block();
        let block = &mut self.program[idx].blocks.blocks_mut()[block_idx];
        for s in stmts {
            block.push_back(s);
        }
        block.terminator = Some(Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![Exp::Variable(ret)],
        }));
    }

    // --- Value helpers ----------------------------------------------------

    /// A variable reference for local slot `idx`: a parameter if `idx < nparams`,
    /// else a named local.
    fn local_ref(&self, co: &CodeObject, nparams: usize, idx: usize) -> VariableRef {
        if idx < nparams {
            VariableRef::new_parameter(ParameterIdx::new(idx))
        } else {
            let name = co
                .varnames
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("v{idx}"));
            VariableRef::new_local(name)
        }
    }

    /// The symbolic name operand (`co_names[arg]`), falling back to `argval`.
    fn name_operand(&self, co: &CodeObject, insn: &Instruction) -> String {
        if let ConstEntry::Str(s) = &insn.argval {
            return s.clone();
        }
        insn.arg
            .and_then(|a| co.names.get(a as usize))
            .cloned()
            .unwrap_or_else(|| "<name>".to_string())
    }

    /// Materialize a `LOAD_CONST` operand as an expression (used by `RETURN_CONST`).
    fn const_exp(&self, co: &CodeObject, insn: &Instruction) -> Exp {
        self.const_slot(co, insn).0
    }

    /// A `LOAD_CONST` slot: an expression plus (for a code constant) the target
    /// function name so a following call resolves to it.
    fn const_slot(&self, co: &CodeObject, insn: &Instruction) -> (Exp, Option<String>) {
        // Prefer the const table (arg index) but fall back to argval.
        let entry = insn
            .arg
            .and_then(|a| co.consts.get(a as usize))
            .unwrap_or(&insn.argval);
        self.entry_to_slot(entry)
    }

    fn entry_to_slot(&self, entry: &ConstEntry) -> (Exp, Option<String>) {
        match entry {
            ConstEntry::None => (Exp::new_bytes(Vec::new()), None),
            ConstEntry::Bool(b) => (Exp::new_bytes(vec![*b as u8]), None),
            ConstEntry::Int(i) => (Exp::new_bytes(i.to_be_bytes().to_vec()), None),
            ConstEntry::Float(s) | ConstEntry::Other(s) => (Exp::new_str(s), None),
            ConstEntry::Str(s) => (Exp::new_str(s), None),
            ConstEntry::Bytes(s) => (Exp::new_bytes(s.bytes().collect()), None),
            ConstEntry::Code(id) => {
                let name = self.func_names.get(id).cloned();
                match &name {
                    Some(n) => (
                        Exp::ObjectRef(ctadl_ir::CallObject::FunctionPtr(n.as_str().into())),
                        name,
                    ),
                    None => (Exp::new_bytes(Vec::new()), None),
                }
            }
        }
    }

    /// Pop the top of the stack as an expression (fresh temp on underflow).
    fn pop_exp(&mut self, sim: &mut StackSim) -> Exp {
        let mut counter = self.counter;
        let slot = sim.pop(&mut || {
            let v = VariableRef::new_local(format!("temp_{counter}"));
            counter += 1;
            v
        });
        self.counter = counter;
        slot.exp
    }

    fn pop_n(&mut self, sim: &mut StackSim, n: usize) -> Vec<Exp> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.pop_exp(sim));
        }
        out.reverse();
        out
    }

    /// Pop the top slot (name/null metadata preserved), fresh temp on underflow.
    fn pop_slot(&mut self, sim: &mut StackSim) -> Slot {
        let mut counter = self.counter;
        let slot = sim.pop(&mut || {
            let v = VariableRef::new_local(format!("temp_{counter}"));
            counter += 1;
            v
        });
        self.counter = counter;
        slot
    }

    fn pop_n_slots(&mut self, sim: &mut StackSim, n: usize) -> Vec<Slot> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let mut counter = self.counter;
            let slot = sim.pop(&mut || {
                let v = VariableRef::new_local(format!("temp_{counter}"));
                counter += 1;
                v
            });
            self.counter = counter;
            out.push(slot);
        }
        out
    }

    /// A fresh, empty container temporary usable as a load/store base for its
    /// element (`.item`) field.
    fn new_container(&mut self, stmts: &mut Vec<Statement>, si: SourceInfo) -> VariableRef {
        let tmp = self.fresh();
        stmts.push(Statement::new(StatementKind::assign(tmp.clone(), []), si));
        tmp
    }

    /// Ensure `exp` is a bare variable, materializing a copy if it is a
    /// constant/access-path (so it can be used as a load/store base).
    fn as_variable(&mut self, stmts: &mut Vec<Statement>, exp: Exp, si: SourceInfo) -> VariableRef {
        match exp {
            Exp::Variable(v) => v,
            other => {
                let tmp = self.fresh();
                stmts.push(Statement::new(
                    StatementKind::assign(tmp.clone(), [other]),
                    si,
                ));
                tmp
            }
        }
    }

    /// Record (or clear) whether local `idx` currently holds a function pointer,
    /// so a later `LOAD_FAST` + call through it becomes a direct call.
    fn track_func_ptr_local(&mut self, co: &CodeObject, idx: usize, slot: &Slot) {
        let Some(varname) = co.varnames.get(idx).cloned() else {
            return;
        };
        if let Exp::ObjectRef(ctadl_ir::CallObject::FunctionPtr(n)) = &slot.exp {
            self.func_ptr_locals.insert(varname, n.to_string());
        } else {
            self.func_ptr_locals.remove(&varname);
        }
    }

    /// The resolved name of a cell/free variable, taken from the instruction's
    /// `argval` (which `dis` fills in for `*_DEREF` and cell `*_FAST` ops).
    fn deref_name(&self, co: &CodeObject, insn: &Instruction) -> String {
        if let ConstEntry::Str(s) = &insn.argval {
            return s.clone();
        }
        insn.arg
            .and_then(|a| co.varnames.get(a as usize))
            .cloned()
            .unwrap_or_else(|| format!("cell{}", insn.arg.unwrap_or(0)))
    }

    /// Load a closure cell from the shared cell namespace.
    fn load_cell(&mut self, stmts: &mut Vec<Statement>, name: &str, si: SourceInfo) -> VariableRef {
        self.load_attr(
            stmts,
            VariableRef::new_global(),
            &format!("{CELL_PREFIX}{name}"),
            si,
        )
    }

    /// Store a closure cell into the shared cell namespace.
    fn store_cell(&mut self, stmts: &mut Vec<Statement>, name: &str, src: Exp, si: SourceInfo) {
        self.store_attr(
            stmts,
            VariableRef::new_global(),
            &format!("{CELL_PREFIX}{name}"),
            src,
            si,
        );
    }

    fn load_global(
        &mut self,
        stmts: &mut Vec<Statement>,
        name: &str,
        si: SourceInfo,
    ) -> VariableRef {
        self.load_attr(stmts, VariableRef::new_global(), name, si)
    }

    fn store_global(&mut self, stmts: &mut Vec<Statement>, name: &str, src: Exp, si: SourceInfo) {
        self.store_attr(stmts, VariableRef::new_global(), name, src, si);
    }

    /// `dest = base.field` (a symbolic field load).
    fn load_attr(
        &mut self,
        stmts: &mut Vec<Statement>,
        base: VariableRef,
        field: &str,
        si: SourceInfo,
    ) -> VariableRef {
        let dest = self.fresh();
        stmts.push(Statement::new(
            StatementKind::load(
                dest.clone(),
                AccessPath::without_fields(base),
                FieldPath::symbol(field),
            ),
            si,
        ));
        dest
    }

    /// `store base.field := src` (a symbolic field store).
    fn store_attr(
        &mut self,
        stmts: &mut Vec<Statement>,
        base: VariableRef,
        field: &str,
        src: Exp,
        si: SourceInfo,
    ) {
        stmts.push(Statement::new(
            StatementKind::store(
                AccessPath::without_fields(base),
                FieldPath::symbol(field),
                src,
            ),
            si,
        ));
    }

    fn return_terminator(&self, exp: Exp) -> Terminator {
        // A generator hands its caller the result object accumulating every
        // `yield`, not the bytecode-level return value (which is `None` / the
        // `StopIteration` payload).
        let arg = if self.in_generator {
            Exp::Variable(VariableRef::new_local(GEN_RESULT.to_string()))
        } else {
            exp
        };
        Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![arg],
        })
    }

    fn fresh(&mut self) -> VariableRef {
        let n = self.counter;
        self.counter += 1;
        VariableRef::new_local(format!("temp_{n}"))
    }

    /// Source info for an instruction, mapping its position to a byte span in the
    /// source (so SARIF reports the right line). Falls back to a zero span.
    fn source_info_for(&mut self, insn: &Instruction) -> SourceInfo {
        // Prefer the full source span (3.11+, via PEP 657 `co_positions`). <=3.10
        // has no per-instruction column info, so fall back to `starts_line` at
        // column 0: without it every instruction collapses onto line 1, which both
        // misreports lines and merges otherwise-distinct code-flow steps (source and
        // sink landing on the same location), breaking the source->sink connection.
        // (line, column, has_real_column). <=3.10 supplies no column, so it is
        // synthesized below to keep same-line steps distinct.
        let line_col = match &insn.position {
            Some(p) if p.start_line >= 1 => {
                Some((p.start_line, p.start_column.max(0) as usize, true))
            }
            _ => insn
                .starts_line
                .filter(|&l| l >= 1)
                .map(|l| (l, 0usize, false)),
        };
        let (start, len) = match line_col {
            Some((line1, col, has_column)) => {
                let line = (line1 - 1) as usize;
                let base = self.line_starts.get(line).copied().unwrap_or(0);
                // Bytes remaining on this line, so a synthetic column never spills
                // onto the next line and misreports `startLine`.
                let line_len = self
                    .line_starts
                    .get(line + 1)
                    .copied()
                    .unwrap_or(self.source_len)
                    .saturating_sub(base);
                let col = if has_column {
                    col
                } else {
                    // Distinct per-instruction column within the line (see
                    // `line_column_cursor`), clamped to stay on the line.
                    let cursor = self.line_column_cursor.entry(line1).or_insert(0);
                    let c = (*cursor).min(line_len.saturating_sub(1));
                    *cursor += 1;
                    c
                };
                let start = (base + col).min(self.source_len.max(1).saturating_sub(1)) as u32;
                (start, 1u32)
            }
            None => (0, 1),
        };
        SourceInfo::new(self.source_info_builder.span_for(
            self.artifact_key.clone(),
            start,
            source_info::SpanLen::ByteLen(len),
        ))
    }

    /// A `SourceInfo` with no meaningful span, for synthesized statements that do
    /// not correspond to a source location (external-stub bodies).
    fn zero_source_info(&mut self) -> SourceInfo {
        SourceInfo::new(self.source_info_builder.span_for(
            self.artifact_key.clone(),
            0,
            source_info::SpanLen::ByteLen(1),
        ))
    }

    fn finish(self) -> Result<ProgramInfo, Error> {
        log::trace!("python IR program:\n{}", self.program);
        self.program
            .verify()
            .map_err(|e| Error::PythonConversion(format!("IR verification failed: {e}")))?;
        Ok(ProgramInfo {
            program: self.program,
            vmt: VirtualMethodTable::default(),
            source_info: self.source_info_builder.finish(),
        })
    }
}

/// The symbolic field used to model a container element (subscript, iteration,
/// append). All elements of a collection alias this single field — a
/// field-insensitive but sound model of collection contents.
const ITEM_FIELD: &str = "item";

/// `co_flags` bit for a `*args` collector parameter (`CO_VARARGS`).
const CO_VARARGS: i64 = 0x04;
/// `co_flags` bit for a `**kwargs` collector parameter (`CO_VARKEYWORDS`).
const CO_VARKEYWORDS: i64 = 0x08;
/// `co_flags` bit marking a generator function (`CO_GENERATOR`).
const CO_GENERATOR: i64 = 0x20;

/// The synthetic local modeling a generator's produced sequence: each `yield`
/// stores into its `.item` field, and the generator's returns hand back this
/// object, so a caller iterating the generator recovers the yielded taint.
const GEN_RESULT: &str = "<gen_result>";

/// The synthetic per-function local carrying the in-flight exception, connecting
/// a `raise` (which stores into it) to a handler's `PUSH_EXC_INFO` (which loads
/// it) — the raise→catch edge is not present in `jump_targets`.
const EXC_LOCAL: &str = "<exc>";

/// The symbolic field modeling `BaseException.args` (the tuple of constructor
/// arguments), so `except E as e: e.args[0]` recovers a raised value.
const ARGS_FIELD: &str = "args";

/// Field-name prefix for a closure cell variable, kept in a shared namespace so a
/// value stored into a cell by the enclosing function is read back by the nested
/// function that closes over it.
const CELL_PREFIX: &str = "cell:";

/// A value on the simulated operand stack.
#[derive(Clone)]
struct Slot {
    exp: Exp,
    /// A symbolic name for a callable operand (global/attr/method/code), used to
    /// resolve a following `CALL` to a direct target.
    name: Option<String>,
    /// A `PUSH_NULL` / self-or-null placeholder.
    is_null: bool,
}

impl Slot {
    fn val(exp: Exp) -> Self {
        Slot {
            exp,
            name: None,
            is_null: false,
        }
    }
    fn null() -> Self {
        Slot {
            exp: Exp::new_bytes(Vec::new()),
            name: None,
            is_null: true,
        }
    }
    fn named(exp: Exp, name: Option<String>) -> Self {
        Slot {
            exp,
            name,
            is_null: false,
        }
    }
}

/// The simulated operand stack for one basic block.
struct StackSim {
    stack: Vec<Slot>,
}

impl StackSim {
    /// Start the simulation from a known entry stack (threaded from a predecessor
    /// block), or empty (`vec![]`) for the entry block.
    fn with_entry(stack: Vec<Slot>) -> Self {
        StackSim { stack }
    }

    /// A snapshot of the current stack, for threading to successor blocks.
    fn snapshot(&self) -> Vec<Slot> {
        self.stack.clone()
    }

    fn push(&mut self, slot: Slot) {
        self.stack.push(slot);
    }

    /// Pop, synthesizing a fresh-temp value on underflow (a value that came from a
    /// predecessor block's stack).
    fn pop(&mut self, fresh: &mut dyn FnMut() -> VariableRef) -> Slot {
        self.stack
            .pop()
            .unwrap_or_else(|| Slot::val(Exp::Variable(fresh())))
    }

    /// Peek the top's expression without popping (fresh temp if empty).
    fn peek_exp(&mut self, fresh: &mut dyn FnMut() -> VariableRef) -> Exp {
        match self.stack.last() {
            Some(s) => s.exp.clone(),
            None => {
                let slot = Slot::val(Exp::Variable(fresh()));
                let exp = slot.exp.clone();
                self.stack.push(slot);
                exp
            }
        }
    }

    /// The slot `i` positions from the top (1 = top).
    fn peek_at(&self, i: usize) -> Option<&Slot> {
        if i == 0 || i > self.stack.len() {
            return None;
        }
        self.stack.get(self.stack.len() - i)
    }

    /// Swap the top with the slot `i` positions down (`SWAP(i)`).
    fn swap(&mut self, i: usize) {
        let len = self.stack.len();
        if i >= 1 && i <= len {
            self.stack.swap(len - 1, len - i);
        }
    }
}

// --- Free functions -------------------------------------------------------

/// Byte offset of the start of each source line.
fn compute_line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Block leaders: the first instruction, every jump target, and the instruction
/// following any branch/jump/return.
fn compute_leaders(instructions: &[Instruction]) -> BTreeSet<i64> {
    let mut leaders = BTreeSet::new();
    if let Some(first) = instructions.first() {
        leaders.insert(first.offset);
    }
    let offsets: Vec<i64> = instructions.iter().map(|i| i.offset).collect();
    for (i, insn) in instructions.iter().enumerate() {
        let op = insn.opname.as_str();
        if is_branch(op) || is_setup_exception(op) {
            // Jump targets are leaders. For `SETUP_*` (<=3.11 exception setup) the
            // target is the handler block, reached on the implicit exception edge.
            for t in &insn.jump_targets {
                leaders.insert(*t);
            }
            // The fallthrough instruction is a leader.
            if let Some(next) = offsets.get(i + 1) {
                leaders.insert(*next);
            }
        } else if is_return(op)
            && let Some(next) = offsets.get(i + 1)
        {
            leaders.insert(*next);
        }
        // Any instruction Python marked as a jump target is a leader.
        if insn.is_jump_target {
            leaders.insert(insn.offset);
        }
    }
    leaders
}

/// Whether an instruction pushes an extra NULL/self slot alongside its named
/// operand for a following `CALL` — the 3.11+ `LOAD_GLOBAL` NULL and the 3.12+
/// `LOAD_ATTR` method-load self. `dis` records it in `argrepr` ("NULL + name",
/// "name + NULL", "NULL|self + name", …) uniformly across those versions, while
/// <=3.10/<=3.11 (respectively) push no such slot and encode a plain name index
/// in `arg` — so a low-bit test on `arg` misfires there. Keying on the marker is
/// version-robust.
fn pushes_null_marker(insn: &Instruction) -> bool {
    insn.argrepr.as_deref().is_some_and(|s| s.contains("NULL"))
}

fn is_return(op: &str) -> bool {
    matches!(
        op,
        "RETURN_VALUE" | "RETURN_CONST" | "RAISE_VARARGS" | "RERAISE"
    )
}

fn is_unconditional_jump(op: &str) -> bool {
    matches!(
        op,
        "JUMP_FORWARD" | "JUMP_BACKWARD" | "JUMP_ABSOLUTE" | "JUMP_BACKWARD_NO_INTERRUPT"
    )
}

fn is_conditional_jump(op: &str) -> bool {
    op.starts_with("POP_JUMP_IF")
        || op.starts_with("POP_JUMP_FORWARD_IF")
        || op.starts_with("POP_JUMP_BACKWARD_IF")
        || matches!(op, "JUMP_IF_TRUE_OR_POP" | "JUMP_IF_FALSE_OR_POP" | "SEND")
}

fn is_branch(op: &str) -> bool {
    is_unconditional_jump(op) || is_conditional_jump(op) || op == "FOR_ITER"
}

/// `SETUP_*` block-setup ops (<=3.11) whose jump target is an exception handler
/// entered on the implicit exception edge (3.12+ uses a zero-cost exception table
/// instead). The frontend treats those targets as handler blocks.
fn is_setup_exception(op: &str) -> bool {
    matches!(
        op,
        "SETUP_FINALLY" | "SETUP_EXCEPT" | "SETUP_WITH" | "SETUP_CLEANUP" | "SETUP_ASYNC_WITH"
    )
}

fn is_binary_op(op: &str) -> bool {
    matches!(
        op,
        "BINARY_OP"
            | "COMPARE_OP"
            | "IS_OP"
            | "CONTAINS_OP"
            | "BINARY_ADD"
            | "BINARY_SUBTRACT"
            | "BINARY_MULTIPLY"
            | "BINARY_TRUE_DIVIDE"
            | "BINARY_FLOOR_DIVIDE"
            | "BINARY_MODULO"
            | "BINARY_POWER"
            | "BINARY_AND"
            | "BINARY_OR"
            | "BINARY_XOR"
            | "BINARY_LSHIFT"
            | "BINARY_RSHIFT"
            | "BINARY_MATRIX_MULTIPLY"
    )
}

fn is_unary_op(op: &str) -> bool {
    matches!(
        op,
        "UNARY_NEGATIVE"
            | "UNARY_POSITIVE"
            | "UNARY_NOT"
            | "UNARY_INVERT"
            | "TO_BOOL"
            | "GET_LEN"
            | "FORMAT_SIMPLE"
            | "FORMAT_VALUE"
            | "CONVERT_VALUE"
            | "COPY_FREE_VARS"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small hand-written stable-text document exercising locals, globals, a
    /// call, an attribute store, and a branch — lowered without spawning Python.
    /// Guards that the frontend produces `MirVerify`-clean Load/Store IR.
    const STABLE: &str = r#"
bytecode_format 1
code_object {
  name "transfer"
  qualname "transfer"
  filename "t.py"
  first_line 1
  flags 3
  arg_count 1
  kwonly_count 0
  names []
  varnames ["a", "b"]
  consts [none]
  instruction { offset 0 opname RESUME opcode 149 arg 0 argval int 0 argrepr "" starts_line 1 is_jump_target false jump_targets [] position 1:0-1:0 }
  instruction { offset 2 opname LOAD_FAST opcode 85 arg 0 argval str "a" argrepr "a" starts_line 2 is_jump_target false jump_targets [] position 2:8-2:9 }
  instruction { offset 4 opname STORE_FAST opcode 110 arg 1 argval str "b" argrepr "b" starts_line 2 is_jump_target false jump_targets [] position 2:4-2:5 }
  instruction { offset 6 opname LOAD_FAST opcode 85 arg 1 argval str "b" argrepr "b" starts_line 3 is_jump_target false jump_targets [] position 3:11-3:12 }
  instruction { offset 8 opname RETURN_VALUE opcode 36 arg none argval none argrepr "" starts_line 3 is_jump_target false jump_targets [] position 3:4-3:12 }
}
"#;

    #[test]
    fn lowers_and_verifies() {
        let info =
            lower_stable_text(Path::new("t.py"), STABLE, Some("x\ny\nz\n".to_string())).unwrap();
        // One function, `transfer`, with one by-ref parameter.
        assert_eq!(info.program.functions.len(), 1);
        let f = &info.program.functions[FunctionIdx::new(0)];
        assert_eq!(f.name, "transfer");
        assert_eq!(f.num_parameters(), 1);
        // The IR verifies (Load/Store invariants hold) — `finish` already called
        // `verify`, but assert the structure is non-empty too.
        assert!(!f.blocks.is_empty());
    }

    #[test]
    fn external_stub_created_for_undefined_call() {
        // A module that calls an undefined global `sink` should get a bodyless
        // `sink` stub so models can match it by name.
        let text = r#"
bytecode_format 1
code_object {
  name "<module>"
  qualname "<module>"
  filename "t.py"
  first_line 1
  flags 0
  arg_count 0
  kwonly_count 0
  names ["sink"]
  varnames []
  consts [none]
  instruction { offset 0 opname RESUME opcode 149 arg 0 argval int 0 argrepr "" starts_line 0 is_jump_target false jump_targets [] position 0:0-1:0 }
  instruction { offset 2 opname LOAD_GLOBAL opcode 91 arg 1 argval str "sink" argrepr "sink + NULL" starts_line 1 is_jump_target false jump_targets [] position 1:0-1:4 }
  instruction { offset 12 opname CALL opcode 53 arg 0 argval int 0 argrepr "" starts_line 1 is_jump_target false jump_targets [] position 1:0-1:6 }
  instruction { offset 20 opname POP_TOP opcode 32 arg none argval none argrepr "" starts_line 1 is_jump_target false jump_targets [] position 1:0-1:6 }
  instruction { offset 22 opname RETURN_CONST opcode 103 arg 0 argval none argrepr "None" starts_line 1 is_jump_target false jump_targets [] position 1:0-1:6 }
}
"#;
        let info = lower_stable_text(Path::new("t.py"), text, None).unwrap();
        assert!(
            info.program.functions.iter().any(|f| f.name == "sink"),
            "expected a `sink` external stub function"
        );
    }
}
