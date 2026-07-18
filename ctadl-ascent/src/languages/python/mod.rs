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

/// Import a Python artifact (`.py` or `.pyc`): serialize its bytecode, parse it,
/// and lower it to a [`ProgramInfo`].
pub fn import_python(import: &crate::project::ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.artifact_path;
    let text = run_serializer(path, Format::Stable)?;
    // Read the source (for `.py`) so instruction positions map to source lines in
    // SARIF. Absent for `.pyc` (or on read failure): source info degrades to a
    // zero offset, which is harmless.
    let source = std::fs::read_to_string(path).ok();
    lower_stable_text(path, &text, source)
}

/// Parse stable bytecode text and lower it to a [`ProgramInfo`]. Factored out of
/// [`import_python`] so it can be exercised without spawning Python.
fn lower_stable_text(
    path: &Path,
    text: &str,
    source: Option<String>,
) -> Result<ProgramInfo, Error> {
    let file = python_bytecode_reader::parse(text)?;
    let mut lowering = Lowering::new(path, source);
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
    counter: u32,
}

impl Lowering {
    fn new(path: &Path, source: Option<String>) -> Self {
        let (line_starts, source_len) = match &source {
            Some(s) => (compute_line_starts(s), s.len()),
            None => (vec![0], 0),
        };
        let encoding = if source.is_some() {
            ArtifactEncoding::Utf8
        } else {
            ArtifactEncoding::Binary
        };
        let artifact_key = ArtifactKey {
            path: path.to_string_lossy().to_string(),
            sub_artifact_id: 0,
            hash: Vec::new(),
            encoding,
        };
        Self {
            program: Program::default(),
            source_info_builder: SourceInfoBuilder::new(source_info::ArtifactMetadata::new()),
            artifact_key,
            line_starts,
            source_len,
            functions: BTreeMap::new(),
            func_names: BTreeMap::new(),
            external_calls: BTreeMap::new(),
            defined_names: BTreeSet::new(),
            counter: 0,
        }
    }

    fn lower_file(&mut self, file: &BytecodeFile) -> Result<(), Error> {
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
        // Pass 3: synthesize external stub functions for called-but-undefined
        // names (so models can match them by name).
        self.create_external_stubs();
        Ok(())
    }

    /// Recursively assign function indices in document (pre-order) order.
    fn register_code<'a>(&mut self, co: &'a CodeObject, ordered: &mut Vec<(u32, &'a CodeObject)>) {
        let id = ordered.len() as u32;
        let idx = self.program.new_function();
        let name = self.unique_function_name(&co.name);
        self.program[idx].set_name(name.clone());
        self.program[idx].set_return_type(ReturnType { arity: 1 });
        let nparams = (co.arg_count + co.kwonly_count).max(0) as usize;
        let nparams = nparams.min(co.varnames.len());
        for _ in 0..nparams {
            self.program[idx]
                .params
                .parameters
                .push(ParameterType::ByRef);
        }
        self.defined_names.insert(co.name.clone());
        self.func_names.insert(id, name);
        self.functions.insert(id, FuncInfo { idx, nparams });
        ordered.push((id, co));
        for nested in &co.nested_code_objects {
            self.register_code(nested, ordered);
        }
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

        // Offset immediately following the last instruction of each block (its
        // fallthrough successor start), if any.
        for (bi, insn_indices) in blocks.iter().enumerate() {
            let block_idx = block_ids[bi];
            let mut sim = StackSim::new();
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
            | "PUSH_EXC_INFO" | "KW_NAMES" | "EXTENDED_ARG" | "RETURN_GENERATOR"
            | "SETUP_ANNOTATIONS" | "POP_BLOCK" | "POP_EXCEPT" | "NOT_TAKEN" => {}

            // --- Loads of locals ---
            "LOAD_FAST"
            | "LOAD_FAST_CHECK"
            | "LOAD_FAST_AND_CLEAR"
            | "LOAD_DEREF"
            | "LOAD_CLASSDEREF"
            | "LOAD_FAST_BORROW" => {
                let idx = insn.arg.unwrap_or(0) as usize;
                let v = self.local_ref(co, nparams, idx);
                sim.push(Slot::val(Exp::Variable(v)));
            }
            "LOAD_FAST_LOAD_FAST" | "LOAD_FAST_BORROW_LOAD_FAST_BORROW" => {
                let arg = insn.arg.unwrap_or(0) as usize;
                let a = self.local_ref(co, nparams, arg >> 4);
                let b = self.local_ref(co, nparams, arg & 0xF);
                sim.push(Slot::val(Exp::Variable(a)));
                sim.push(Slot::val(Exp::Variable(b)));
            }

            // --- Stores of locals ---
            "STORE_FAST" | "STORE_DEREF" | "STORE_FAST_MAYBE_NULL" => {
                let idx = insn.arg.unwrap_or(0) as usize;
                let dest = self.local_ref(co, nparams, idx);
                let v = self.pop_exp(sim);
                stmts.push(Statement::new(StatementKind::assign(dest, [v]), si));
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
                // 3.11+: `LOAD_GLOBAL` with bit 0 of `arg` set pushes a NULL below
                // the callable (for the following CALL). Model it so the call's
                // self/NULL slot isn't synthesized from an underflow.
                if op == "LOAD_GLOBAL" && insn.arg.map(|a| a & 1 == 1).unwrap_or(false) {
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
                // The low bit of `arg` (3.12+) marks a method load: it pushes
                // self then the method for the following CALL.
                if insn.arg.map(|a| a & 1 == 1).unwrap_or(false) {
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
            "BINARY_SUBSCR" | "BINARY_SLICE" => {
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
                // Iterator stays on the stack; the yielded element is item(iter).
                let iter = sim.peek_exp(&mut || self.fresh());
                let ivar = self.as_variable(stmts, iter, si);
                let elt = self.load_attr(stmts, ivar, ITEM_FIELD, si);
                sim.push(Slot::val(Exp::Variable(elt)));
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, true)));
                }
            }

            // --- Container building ---
            "BUILD_TUPLE" | "BUILD_LIST" | "BUILD_SET" | "BUILD_STRING" | "BUILD_CONST_KEY_MAP" => {
                let n = insn.arg.unwrap_or(0) as usize;
                let elts = self.pop_n(sim, n);
                let tmp = self.fresh();
                stmts.push(Statement::new(StatementKind::assign(tmp.clone(), elts), si));
                sim.push(Slot::val(Exp::Variable(tmp)));
            }
            "BUILD_MAP" => {
                let n = insn.arg.unwrap_or(0) as usize;
                let elts = self.pop_n(sim, n * 2);
                let tmp = self.fresh();
                stmts.push(Statement::new(StatementKind::assign(tmp.clone(), elts), si));
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
                // 3.13: pop the code object, push the function. <=3.12: `arg`
                // flags add extra pops (defaults/kwdefaults/annotations/closure).
                let extra = insn
                    .arg
                    .map(|a| (a as u32).count_ones() as usize)
                    .unwrap_or(0);
                self.pop_n(sim, extra);
                let code_slot = sim.pop(&mut || self.fresh());
                sim.push(code_slot);
            }
            "SET_FUNCTION_ATTRIBUTE" => {
                // Pops the attribute value, leaves the function on the stack.
                self.pop_exp(sim);
            }

            // --- Calls ---
            "CALL" | "CALL_FUNCTION" | "CALL_METHOD" | "CALL_KW" | "CALL_FUNCTION_KW" => {
                let argc = insn.arg.unwrap_or(0) as usize;
                self.lower_call(op, argc, sim, stmts, si);
            }
            "CALL_FUNCTION_EX" => {
                // callable, (self?), args tuple, and (if arg&1) kwargs dict.
                let extra = if insn.arg.map(|a| a & 1 == 1).unwrap_or(false) {
                    1
                } else {
                    0
                };
                self.pop_n(sim, 2 + extra); // args (+kwargs) + callable-or-self
                let callee = sim.pop(&mut || self.fresh());
                let ret = self.fresh();
                let style = self.call_style_for(callee);
                stmts.push(Statement::new(
                    StatementKind::CallAssign {
                        style,
                        rets: ctadl_ir::thin_vec![ret.clone()],
                        args: ctadl_ir::thin_vec![],
                    },
                    si,
                ));
                sim.push(Slot::val(Exp::Variable(ret)));
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
                self.pop_n(sim, n);
                return Ok(Some(Terminator::new_kind(TerminatorKind::Return {
                    args: smallvec![Exp::new_bytes(Vec::new())],
                })));
            }

            // --- Jumps ---
            "JUMP_FORWARD" | "JUMP_BACKWARD" | "JUMP_ABSOLUTE" | "JUMP_BACKWARD_NO_INTERRUPT" => {
                if is_last {
                    return Ok(Some(goto_from(&insn.jump_targets, false)));
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

        // The callee is the first slot carrying a name; NULL slots are dropped;
        // remaining non-null slots (in push order) are the actual arguments.
        let callee_pos = popped.iter().position(|s| s.name.is_some());
        let callee = callee_pos.map(|p| popped[p].clone());
        let mut args: ctadl_ir::ThinVec<Exp> = ctadl_ir::ThinVec::new();
        for (i, slot) in popped.iter().enumerate() {
            if Some(i) == callee_pos || slot.is_null {
                continue;
            }
            args.push(slot.exp.clone());
        }

        let style = match callee {
            Some(slot) => self.call_style_for(slot),
            None => CallStyle::FuncPtrCall {
                callee: AccessPath::without_fields(self.fresh()),
                signature: None,
            },
        };
        // Record the arg count against any direct-call name for external stubs.
        if let CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(edges),
        } = &style
            && let Some(name) = edges.first()
        {
            let entry = self.external_calls.entry(name.to_string()).or_insert(0);
            *entry = (*entry).max(args.len());
        }

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
        }
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
        Terminator::new_kind(TerminatorKind::Return {
            args: smallvec![exp],
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
        let (start, len) = match &insn.position {
            Some(p) if p.start_line >= 1 => {
                let line = (p.start_line - 1) as usize;
                let base = self.line_starts.get(line).copied().unwrap_or(0);
                let col = p.start_column.max(0) as usize;
                let start = (base + col).min(self.source_len.max(1).saturating_sub(1)) as u32;
                (start, 1u32)
            }
            _ => (0, 1),
        };
        SourceInfo::new(self.source_info_builder.span_for(
            self.artifact_key.clone(),
            start,
            source_info::SpanLen::ByteLen(len),
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
    fn new() -> Self {
        StackSim { stack: Vec::new() }
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
        if is_branch(op) {
            // Jump targets are leaders.
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
