// Instruction flow abstraction: iterate every instruction in a JAR with
// Dataflow/Call/Other kind and source/destination locations.

use crate::error::{ClassFileError, ClassFileResult};
use crate::parse_utils::{read_i32_be, read_u16_be, read_u8};
use crate::parser::ClassFileParser;
use crate::types::{ClassFile, CpEntry, MethodInfo};
use std::collections::{BTreeSet, HashMap};
use std::slice::Iter;

// ============== Location (source/destination) ==============

/// Common representation for a source or destination of dataflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// Local variable slot (index into local variable array).
    Register(u16),
    /// Method parameter (index 0 = receiver for instance methods, or first arg for static).
    Parameter(u16),
    /// Constant value pushed by ldc / const instructions.
    Constant(ConstantValue),
    /// Value that resides on the operand stack, identified by a function-wide slot id.
    ///
    /// These ids are assigned by the stack-slot normalization pass and are stable
    /// across the whole method, independent of changing stack depth.
    StackSlot(u32),
    /// Value consumed from the operand stack (0 = top, 1 = second, etc.).
    StackInput(u8),
    /// Value produced onto the operand stack.
    StackOutput,
    /// Field access (getfield/putfield/getstatic/putstatic).
    FieldRef(FieldRef),
    /// Array element access (*aload/*astore): logical location for the element.
    ArrayElement,
}

/// Constant value from the constant pool or const instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstantValue {
    Integer(i32),
    Long(i64),
    Float(u32),
    Double(u64),
    String(String),
    ClassRef(String),
    Null,
}

/// Resolved field reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRef {
    pub class_name: String,
    pub field_name: String,
    pub descriptor: String,
}

// ============== Instruction kind ==============

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionKind {
    Dataflow,
    Call,
    Other,
}

// ============== DataflowInfo / CallInfo ==============

#[derive(Debug, Clone)]
pub struct DataflowInfo {
    pub sources: Vec<Location>,
    pub destinations: Vec<Location>,
}

#[derive(Debug, Clone)]
pub enum CallKind {
    Static,
    Virtual,
    Special,
    Interface,
    Dynamic,
}

/// Resolved method target for invoke* (except invokedynamic which uses bootstrap + name/type).
#[derive(Debug, Clone)]
pub struct MethodTarget {
    pub class_name: String,
    pub method_name: String,
    pub descriptor: String,
}

#[derive(Debug, Clone)]
pub struct CallInfo {
    /// For invokestatic/virtual/special/interface: the resolved method.
    pub target: Option<MethodTarget>,
    /// For invokedynamic: bootstrap method index and name:descriptor.
    pub dynamic_bootstrap: Option<u16>,
    pub dynamic_name_and_type: Option<String>,
    pub call_kind: CallKind,
    /// Number of stack slots consumed (receiver + args, or args only for static).
    pub stack_slots_consumed: u8,

    /// Instance receiver location for non-static calls.
    /// Uses `Location::StackInput(depth)` where depth=0 is top-of-stack at the call site.
    pub receiver: Option<Location>,
    /// One entry per consumed argument stack slot (slot-based; long/double => 2).
    /// Uses `Location::StackInput(depth)` where depth=0 is top-of-stack at the call site.
    pub arguments: Vec<Location>,
    /// Return value location for non-void calls.
    /// Uses `Location::StackOutput` at decode time (normalization rewrites it to `StackSlot`).
    pub return_value: Option<Location>,
}

// ============== InstructionFlowInfo ==============

#[derive(Debug, Clone)]
pub struct InstructionFlowInfo<'a> {
    pub kind: InstructionKind,
    pub method: &'a MethodInfo,
    pub class_file: &'a ClassFile,
    pub pc: u32,
    /// Opcode offset from the start of the raw `.class` bytes (`code_byte_offset_in_classfile + pc`).
    ///
    /// For JAR-loaded classes, this is relative to that entry's decompressed `.class` data, not the ZIP file.
    pub file_byte_offset: u32,
    /// Size of this instruction in bytes (including `wide` prefix when decoded as one step).
    pub byte_length: u32,
    pub opcode: u8,
    pub mnemonic: &'static str,
    pub dataflow: Option<DataflowInfo>,
    pub call: Option<CallInfo>,
}

// ============== Basic blocks / CFG ==============

/// A basic block: a maximal straight-line sequence of instructions with a single entry
/// and single exit (for normal control-flow).
#[derive(Debug, Clone)]
pub struct BasicBlock<'a> {
    /// Index of this block in the enclosing method's block list.
    pub index: usize,
    /// Bytecode offset (pc) of the first instruction in this block.
    pub start_pc: u32,
    /// Bytecode offset just past the last instruction in this block (half-open range).
    pub end_pc: u32,
    /// Range of instruction indices within the enclosing method's instruction vector.
    instr_range: std::ops::Range<usize>,
    /// Indices of predecessor blocks.
    pub predecessors: Vec<usize>,
    /// Indices of successor blocks.
    pub successors: Vec<usize>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> BasicBlock<'a> {
    /// Slice of `InstructionFlowInfo` values that belong to this block.
    pub fn instructions<'b>(
        &self,
        cfg: &'b MethodBasicBlocks<'a>,
    ) -> &'b [InstructionFlowInfo<'a>] {
        &cfg.instructions[self.instr_range.clone()]
    }
}

/// Basic-block view of a single method: all decoded instructions plus their partitioning
/// into basic blocks with predecessors/successors.
#[derive(Debug, Clone)]
pub struct MethodBasicBlocks<'a> {
    pub method: &'a MethodInfo,
    pub class_file: &'a ClassFile,
    instructions: Vec<InstructionFlowInfo<'a>>,
    blocks: Vec<BasicBlock<'a>>,
}

impl<'a> MethodBasicBlocks<'a> {
    /// All basic blocks in this method, in index order.
    pub fn blocks(&self) -> &[BasicBlock<'a>] {
        &self.blocks
    }

    /// All decoded instructions in this method, in increasing pc order.
    pub fn instructions(&self) -> &[InstructionFlowInfo<'a>] {
        &self.instructions
    }

    /// Instructions that belong to the block at `block_index`.
    pub fn block_instructions(&self, block_index: usize) -> &[InstructionFlowInfo<'a>] {
        let block = &self.blocks[block_index];
        &self.instructions[block.instr_range.clone()]
    }
}

#[derive(Debug, Clone)]
struct InstrCtrl {
    pc: u32,
    next_pc: u32,
    branch_targets: Vec<u32>,
    falls_through: bool,
}

type StackSlotId = u32;

#[derive(Debug, Clone)]
struct StackState {
    slots: Vec<StackSlotId>,
}

/// Compute basic blocks and a control-flow graph for a single method.
pub fn compute_basic_blocks_for_method<'a>(
    cf: &'a ClassFile,
    method: &'a MethodInfo,
) -> ClassFileResult<MethodBasicBlocks<'a>> {
    let code_attr = method
        .code
        .as_ref()
        .ok_or(ClassFileError::InvalidClassFile(
            "method has no Code attribute",
        ))?;
    let code = &code_attr.code;
    let code_len = code.len();

    let mut instructions = Vec::new();
    let mut ctrl = Vec::new();
    let mut pc_to_instr_index: HashMap<u32, usize> = HashMap::new();

    let mut pc: usize = 0;
    while pc < code_len {
        let (info, next_pc) = decode_flow_instruction(code, pc, cf, method)?;
        let opcode = info.opcode;
        let this_pc_u32 = pc as u32;
        let next_pc_u32 = next_pc as u32;

        let mut branch_targets = Vec::new();
        let falls_through;

        match opcode {
            // Conditional branches (including ifnull/ifnonnull).
            0x99..=0x9e | 0x9f..=0xa4 | 0xa5..=0xa6 | 0xc6..=0xc7 => {
                if pc + 2 <= code_len {
                    let offset = i16::from_be_bytes([code[pc + 1], code[pc + 2]]) as i32;
                    let target = (pc as i32 + offset) as u32;
                    branch_targets.push(target);
                }
                falls_through = next_pc < code_len;
            }
            // Unconditional branches (short).
            0xa7 | 0xa8 => {
                if pc + 2 <= code_len {
                    let offset = i16::from_be_bytes([code[pc + 1], code[pc + 2]]) as i32;
                    let target = (pc as i32 + offset) as u32;
                    branch_targets.push(target);
                }
                falls_through = false;
            }
            // Unconditional branches (wide).
            0xc8 | 0xc9 => {
                if pc + 4 <= code_len {
                    let offset = read_i32_be(code, pc + 1)?;
                    let target = (pc as i32 + offset) as u32;
                    branch_targets.push(target);
                }
                falls_through = false;
            }
            // tableswitch
            0xaa => {
                let align = (pc + 1 + 3) & !3;
                if align + 12 > code_len {
                    return Err(ClassFileError::InvalidClassFile("tableswitch truncated"));
                }
                let default_offset = read_i32_be(code, align)?;
                let low = read_i32_be(code, align + 4)?;
                let high = read_i32_be(code, align + 8)?;
                let n = (high - low + 1) as usize;
                let default_pc = (pc as i32 + default_offset) as u32;
                branch_targets.push(default_pc);
                for i in 0..n {
                    let off = read_i32_be(code, align + 12 + i * 4)?;
                    let target = (pc as i32 + off) as u32;
                    branch_targets.push(target);
                }
                branch_targets.sort_unstable();
                branch_targets.dedup();
                falls_through = false;
            }
            // lookupswitch
            0xab => {
                let align = (pc + 1 + 3) & !3;
                if align + 8 > code_len {
                    return Err(ClassFileError::InvalidClassFile("lookupswitch truncated"));
                }
                let default_offset = read_i32_be(code, align)?;
                let npairs = read_i32_be(code, align + 4)? as usize;
                let default_pc = (pc as i32 + default_offset) as u32;
                branch_targets.push(default_pc);
                for i in 0..npairs {
                    let off = read_i32_be(code, align + 12 + i * 8)?;
                    let target = (pc as i32 + off) as u32;
                    branch_targets.push(target);
                }
                branch_targets.sort_unstable();
                branch_targets.dedup();
                falls_through = false;
            }
            // Returns, athrow, ret: terminate block.
            0xac..=0xb1 | 0xbf | 0xa9 => {
                falls_through = false;
            }
            // All other opcodes fall through if there is another instruction.
            _ => {
                falls_through = next_pc < code_len;
            }
        }

        let instr_index = instructions.len();
        pc_to_instr_index.insert(this_pc_u32, instr_index);
        instructions.push(info);
        ctrl.push(InstrCtrl {
            pc: this_pc_u32,
            next_pc: next_pc_u32,
            branch_targets,
            falls_through,
        });

        pc = next_pc;
    }

    // Identify basic block leaders.
    let mut leaders: BTreeSet<u32> = BTreeSet::new();
    leaders.insert(0);

    // Exception handlers start at handler_pc.
    for ex in &code_attr.exception_table {
        leaders.insert(ex.handler_pc as u32);
    }

    for m in &ctrl {
        for &t in &m.branch_targets {
            if (t as usize) < code_len {
                leaders.insert(t);
            }
        }
        // Only conditional branches that fall through create a new leader at the
        // fall-through target. Straight-line instructions stay in the same block.
        if !m.branch_targets.is_empty() && m.falls_through && (m.next_pc as usize) < code_len {
            leaders.insert(m.next_pc);
        }
    }

    let code_len_u32 = code_len as u32;
    let mut leader_pcs: Vec<u32> = leaders.into_iter().collect();
    leader_pcs.sort_unstable();

    // Build blocks and pc -> block index map.
    let mut blocks = Vec::new();
    let mut pc_to_block_index: HashMap<u32, usize> = HashMap::new();
    let mut current_instr = 0usize;

    for (block_index, &start_pc) in leader_pcs.iter().enumerate() {
        let end_pc = if block_index + 1 < leader_pcs.len() {
            leader_pcs[block_index + 1]
        } else {
            code_len_u32
        };

        let block_start = current_instr;
        while current_instr < ctrl.len() && ctrl[current_instr].pc < end_pc {
            current_instr += 1;
        }
        let block_end = current_instr;

        pc_to_block_index.insert(start_pc, block_index);

        blocks.push(BasicBlock {
            index: block_index,
            start_pc,
            end_pc,
            instr_range: block_start..block_end,
            predecessors: Vec::new(),
            successors: Vec::new(),
            _marker: std::marker::PhantomData,
        });
    }

    // Compute successors for each block.
    for b in 0..blocks.len() {
        let (_start, end) = {
            let blk = &blocks[b];
            (blk.start_pc, blk.end_pc)
        };
        let range = {
            let blk = &blocks[b];
            blk.instr_range.clone()
        };
        if range.is_empty() {
            continue;
        }
        let last_instr_index = range.end - 1;
        let meta = &ctrl[last_instr_index];

        // Branch successors.
        for &t in &meta.branch_targets {
            if let Some(&succ_idx) = pc_to_block_index.get(&t) {
                if !blocks[b].successors.contains(&succ_idx) {
                    blocks[b].successors.push(succ_idx);
                }
            }
        }

        // Fall-through successor.
        if meta.falls_through && end < code_len_u32 {
            if let Some(&succ_idx) = pc_to_block_index.get(&end) {
                if !blocks[b].successors.contains(&succ_idx) {
                    blocks[b].successors.push(succ_idx);
                }
            }
        }
    }

    // Predecessors from successors.
    for b in 0..blocks.len() {
        let succs = blocks[b].successors.clone();
        for s in succs {
            if s < blocks.len() && !blocks[s].predecessors.contains(&b) {
                blocks[s].predecessors.push(b);
            }
        }
    }

    // Approximate exception edges: from any block whose instructions lie in a try
    // range to the handler block.
    for ex in &code_attr.exception_table {
        let handler_pc = ex.handler_pc as u32;
        let handler_block = match pc_to_block_index.get(&handler_pc) {
            Some(&idx) => idx,
            None => continue,
        };
        let start_pc = ex.start_pc as u32;
        let end_pc = ex.end_pc as u32;

        for b in 0..blocks.len() {
            let range = blocks[b].instr_range.clone();
            if range.is_empty() {
                continue;
            }
            let first_pc = ctrl[range.start].pc;
            let last_pc = ctrl[range.end - 1].pc;
            if last_pc < start_pc || first_pc >= end_pc {
                continue;
            }
            if !blocks[b].successors.contains(&handler_block) {
                blocks[b].successors.push(handler_block);
            }
            if !blocks[handler_block].predecessors.contains(&b) {
                blocks[handler_block].predecessors.push(b);
            }
        }
    }

    Ok(MethodBasicBlocks {
        method,
        class_file: cf,
        instructions,
        blocks,
    })
}

fn returns_value(descriptor: &str) -> bool {
    match descriptor.rsplit_once(')') {
        Some((_params, ret)) => ret != "V",
        None => false,
    }
}

/// Normalize stack-related locations in a method so that all stack uses and
/// definitions refer to function-wide stack slots instead of per-instruction
/// StackInput/StackOutput.
pub fn normalize_stack_slots_for_method<'a>(
    cfg: &mut MethodBasicBlocks<'a>,
) -> ClassFileResult<()> {
    let blocks_len = cfg.blocks.len();
    if blocks_len == 0 {
        return Ok(());
    }

    let mut in_state: Vec<Option<StackState>> = vec![None; blocks_len];
    let mut out_state: Vec<Option<StackState>> = vec![None; blocks_len];
    let mut worklist: Vec<usize> = Vec::new();

    in_state[0] = Some(StackState { slots: Vec::new() });
    worklist.push(0);

    while let Some(b) = worklist.pop() {
        let entry = in_state[b].clone().ok_or(ClassFileError::InvalidClassFile(
            "missing stack state for block",
        ))?;

        let exit = simulate_block(b, entry, cfg)?;

        let changed = match &out_state[b] {
            None => {
                out_state[b] = Some(exit.clone());
                true
            }
            Some(prev) => prev.slots != exit.slots,
        };

        if !changed {
            continue;
        }

        // Propagate to successors, enforcing consistent stack height/layout.
        let succs = cfg.blocks[b].successors.clone();
        for s in succs {
            if s >= blocks_len {
                continue;
            }
            match &mut in_state[s] {
                None => {
                    in_state[s] = Some(exit.clone());
                    worklist.push(s);
                }
                Some(existing) => {
                    if existing.slots.len() != exit.slots.len() {
                        return Err(ClassFileError::InvalidClassFile(
                            "inconsistent operand stack height at basic-block join",
                        ));
                    }
                    if existing.slots != exit.slots {
                        return Err(ClassFileError::InvalidClassFile(
                            "inconsistent operand stack layout at basic-block join",
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn simulate_block<'a>(
    block_index: usize,
    mut state: StackState,
    cfg: &mut MethodBasicBlocks<'a>,
) -> ClassFileResult<StackState> {
    let block = &cfg.blocks[block_index];
    let range = block.instr_range.clone();

    for idx in range {
        let inst = &mut cfg.instructions[idx];

        // Determine stack effect from dataflow and calls.
        let mut max_stack_input_depth: usize = 0;
        let mut stack_outputs: usize = 0;

        if let Some(df) = inst.dataflow.as_mut() {
            for src in &mut df.sources {
                if let Location::StackInput(depth) = src {
                    let depth_usize = *depth as usize;
                    let len = state.slots.len();
                    if depth_usize >= len {
                        return Err(ClassFileError::InvalidClassFile(
                            "stack underflow while rewriting StackInput",
                        ));
                    }
                    let slot_index = len - 1 - depth_usize;
                    *src = Location::StackSlot(slot_index as StackSlotId);
                    if depth_usize + 1 > max_stack_input_depth {
                        max_stack_input_depth = depth_usize + 1;
                    }
                }
            }

            for dst in &mut df.destinations {
                if let Location::StackOutput = dst {
                    stack_outputs += 1;
                }
            }
        }

        // Additional stack effect for call instructions.
        let mut call_consume: usize = 0;
        let mut call_produce: usize = 0;
        if inst.kind == InstructionKind::Call {
            if let Some(call) = &inst.call {
                call_consume = call.stack_slots_consumed as usize;
                let mut ret_val = false;
                match call.call_kind {
                    CallKind::Dynamic => {
                        if let Some(name_type) = &call.dynamic_name_and_type {
                            if let Some((_name, desc)) = name_type.rsplit_once(':') {
                                ret_val = returns_value(desc);
                            }
                        }
                    }
                    _ => {
                        if let Some(target) = &call.target {
                            ret_val = returns_value(&target.descriptor);
                        }
                    }
                }
                if ret_val {
                    call_produce = 1;
                }
            }

            // Rewrite receiver/arguments stack locations from StackInput(depth)
            // into absolute StackSlot ids based on the current stack state.
            if let Some(call) = inst.call.as_mut() {
                let len = state.slots.len();
                if let Some(receiver) = call.receiver.as_mut() {
                    if let Location::StackInput(depth) = receiver {
                        let depth_usize = *depth as usize;
                        if depth_usize >= len {
                            return Err(ClassFileError::InvalidClassFile(
                                "stack underflow while rewriting call receiver StackInput",
                            ));
                        }
                        let slot_index = len - 1 - depth_usize;
                        *receiver = Location::StackSlot(slot_index as StackSlotId);
                    }
                }
                for arg in &mut call.arguments {
                    if let Location::StackInput(depth) = arg {
                        let depth_usize = *depth as usize;
                        if depth_usize >= len {
                            return Err(ClassFileError::InvalidClassFile(
                                "stack underflow while rewriting call argument StackInput",
                            ));
                        }
                        let slot_index = len - 1 - depth_usize;
                        *arg = Location::StackSlot(slot_index as StackSlotId);
                    }
                }
            }
        }

        let consume = std::cmp::max(max_stack_input_depth, call_consume);
        let produce = stack_outputs.max(call_produce);

        if state.slots.len() < consume {
            return Err(ClassFileError::InvalidClassFile(
                "stack underflow in stack-slot simulation",
            ));
        }

        // Rewrite StackOutput destinations using the *absolute* stack depth position
        // where the produced value will land after consuming `consume` values.
        let old_len = state.slots.len();
        let remaining_len = old_len - consume;
        let mut out_i = 0usize;
        if let Some(df) = inst.dataflow.as_mut() {
            if stack_outputs > 0 {
                for dst in &mut df.destinations {
                    if matches!(dst, Location::StackOutput) {
                        let id = (remaining_len + out_i) as StackSlotId;
                        *dst = Location::StackSlot(id);
                        out_i += 1;
                    }
                }
            }
        }

        // Rewrite call return value location.
        if call_produce == 1 {
            if let Some(call) = inst.call.as_mut() {
                if let Some(ret_loc) = call.return_value.as_mut() {
                    if matches!(ret_loc, Location::StackOutput) {
                        *ret_loc = Location::StackSlot(remaining_len as StackSlotId);
                    }
                }
            }
        }

        // Update stack state: keep the bottom `remaining_len` depths, then
        // produce `produce` values that occupy depths `remaining_len..`.
        state.slots.truncate(remaining_len);
        for i in 0..produce {
            state.slots.push((remaining_len + i) as StackSlotId);
        }
    }

    Ok(state)
}

// ============== Descriptor helpers ==============

/// Returns the number of local variable slots used by the method parameters (JVM convention:
/// long/double use 2 slots, rest use 1).
pub fn descriptor_param_slot_count(descriptor: &str) -> usize {
    let s = match descriptor.strip_prefix('(') {
        Some(x) => x,
        None => return 0,
    };
    let mut slots = 0usize;
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() && b[i] != b')' {
        match b[i] {
            b'J' | b'D' => {
                slots += 2;
                i += 1;
            }
            b'L' => {
                slots += 1;
                i += 1;
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'[' => {
                slots += 1;
                i += 1;
                while i < b.len() && b[i] == b'[' {
                    i += 1;
                }
                if i < b.len() && b[i] == b'L' {
                    while i < b.len() && b[i] != b';' {
                        i += 1;
                    }
                    i += 1;
                } else if i < b.len() {
                    i += 1;
                }
            }
            _ => {
                slots += 1;
                i += 1;
            }
        }
    }
    slots
}

/// Returns true if the given local slot index is a parameter (slot < param_slot_count).
pub fn is_parameter_slot(slot: u16, param_slot_count: usize) -> bool {
    (slot as usize) < param_slot_count
}

// ============== Instruction length (for pc advance) ==============

fn instruction_length(code: &[u8], pc: usize) -> ClassFileResult<usize> {
    if pc >= code.len() {
        return Err(ClassFileError::InvalidClassFile("pc past code"));
    }
    let opcode = code[pc];
    let (base_len, _variable) = match opcode {
        0xaa => {
            let align = (pc + 1 + 3) & !3;
            if align + 12 > code.len() {
                return Err(ClassFileError::InvalidClassFile("tableswitch truncated"));
            }
            let low = read_i32_be(code, align + 4)?;
            let high = read_i32_be(code, align + 8)?;
            let n = (high - low + 1) as usize;
            (align + 12 + n * 4 - pc, false)
        }
        0xab => {
            let align = (pc + 1 + 3) & !3;
            if align + 8 > code.len() {
                return Err(ClassFileError::InvalidClassFile("lookupswitch truncated"));
            }
            let npairs = read_i32_be(code, align + 4)? as usize;
            (align + 8 + npairs * 8 - pc, false)
        }
        0xc4 => {
            if pc + 2 > code.len() {
                return Err(ClassFileError::InvalidClassFile("wide truncated"));
            }
            let subop = code[pc + 1];
            let sub_len = match subop {
                0x15 | 0x16 | 0x17 | 0x18 | 0x19 | 0x36 | 0x37 | 0x38 | 0x39 | 0x3a | 0xa9 => 2,
                0x84 => 4,
                _ => 0,
            };
            let wide_len = if subop == 0x84 { 6 } else { 2 + sub_len };
            (wide_len, false)
        }
        _ => {
            let operands_len = operand_byte_count(opcode, code, pc)?;
            (1 + operands_len, false)
        }
    };
    Ok(base_len)
}

fn operand_byte_count(opcode: u8, _code: &[u8], _pc: usize) -> ClassFileResult<usize> {
    Ok(match opcode {
        0x10 => 1,
        0x11 => 2,
        0x12 => 1,
        0x13 | 0x14 => 2,
        0x15..=0x19 | 0x36..=0x3a => 1,
        0xbc => 1,
        0xbd | 0xc0 | 0xc1 => 2,
        0xb2..=0xb8 => 2,
        0xb9 => 4,
        0xba => 4,
        0xbb => 2,
        0xc5 => 3,
        0x99..=0x9e | 0x9f..=0xa4 | 0xa5..=0xa6 | 0xc6..=0xc7 => 2,
        0xa7 | 0xa8 => 2,
        0xc8 | 0xc9 => 4,
        0x7c => 2,
        _ => 0,
    })
}

// ============== Mnemonic table (for InstructionFlowInfo) ==============

fn mnemonic(opcode: u8) -> &'static str {
    match opcode {
        0x00 => "nop",
        0x01 => "aconst_null",
        0x02 => "iconst_m1",
        0x03 => "iconst_0",
        0x04 => "iconst_1",
        0x05 => "iconst_2",
        0x06 => "iconst_3",
        0x07 => "iconst_4",
        0x08 => "iconst_5",
        0x09 => "lconst_0",
        0x0a => "lconst_1",
        0x0b => "fconst_0",
        0x0c => "fconst_1",
        0x0d => "fconst_2",
        0x0e => "dconst_0",
        0x0f => "dconst_1",
        0x10 => "bipush",
        0x11 => "sipush",
        0x12 => "ldc",
        0x13 => "ldc_w",
        0x14 => "ldc2_w",
        0x15 => "iload",
        0x16 => "lload",
        0x17 => "fload",
        0x18 => "dload",
        0x19 => "aload",
        0x1a => "iload_0",
        0x1b => "iload_1",
        0x1c => "iload_2",
        0x1d => "iload_3",
        0x1e => "lload_0",
        0x1f => "lload_1",
        0x20 => "lload_2",
        0x21 => "lload_3",
        0x22 => "fload_0",
        0x23 => "fload_1",
        0x24 => "fload_2",
        0x25 => "fload_3",
        0x26 => "dload_0",
        0x27 => "dload_1",
        0x28 => "dload_2",
        0x29 => "dload_3",
        0x2a => "aload_0",
        0x2b => "aload_1",
        0x2c => "aload_2",
        0x2d => "aload_3",
        0x2e => "iaload",
        0x2f => "laload",
        0x30 => "faload",
        0x31 => "daload",
        0x32 => "aaload",
        0x33 => "baload",
        0x34 => "caload",
        0x35 => "saload",
        0x36 => "istore",
        0x37 => "lstore",
        0x38 => "fstore",
        0x39 => "dstore",
        0x3a => "astore",
        0x3b => "istore_0",
        0x3c => "istore_1",
        0x3d => "istore_2",
        0x3e => "istore_3",
        0x3f => "lstore_0",
        0x40 => "lstore_1",
        0x41 => "lstore_2",
        0x42 => "lstore_3",
        0x43 => "fstore_0",
        0x44 => "fstore_1",
        0x45 => "fstore_2",
        0x46 => "fstore_3",
        0x47 => "dstore_0",
        0x48 => "dstore_1",
        0x49 => "dstore_2",
        0x4a => "dstore_3",
        0x4b => "astore_0",
        0x4c => "astore_1",
        0x4d => "astore_2",
        0x4e => "astore_3",
        0x4f => "iastore",
        0x50 => "lastore",
        0x51 => "fastore",
        0x52 => "dastore",
        0x53 => "aastore",
        0x54 => "bastore",
        0x55 => "castore",
        0x56 => "dup2_x2",
        0x57 => "pop",
        0x58 => "pop2",
        0x59 => "dup",
        0x5a => "dup_x1",
        0x5b => "dup_x2",
        0x5c => "dup2",
        0x5d => "dup2_x1",
        0x5e => "dup2_x2",
        0x5f => "swap",
        0x60 => "iadd",
        0x61 => "ladd",
        0x62 => "fadd",
        0x63 => "dadd",
        0x64 => "isub",
        0x65 => "lsub",
        0x66 => "fsub",
        0x67 => "dsub",
        0x68 => "imul",
        0x69 => "lmul",
        0x6a => "fmul",
        0x6b => "dmul",
        0x6c => "idiv",
        0x6d => "ldiv",
        0x6e => "fdiv",
        0x6f => "ddiv",
        0x70 => "irem",
        0x71 => "lrem",
        0x72 => "frem",
        0x73 => "drem",
        0x74 => "ineg",
        0x75 => "lneg",
        0x76 => "fneg",
        0x77 => "dneg",
        0x78 => "ishl",
        0x79 => "lshl",
        0x7a => "ishr",
        0x7b => "lshr",
        0x7c => "iushr",
        0x7d => "lushr",
        0x7e => "iand",
        0x7f => "land",
        0x80 => "ior",
        0x81 => "lor",
        0x82 => "ixor",
        0x83 => "lxor",
        0x84 => "iinc",
        0x85..=0x93 => "conv_or_cmp",
        0x94 => "lcmp",
        0x95 => "fcmpl",
        0x96 => "fcmpg",
        0x97 => "dcmpl",
        0x98 => "dcmpg",
        0x99 => "ifeq",
        0x9a => "ifne",
        0x9b => "iflt",
        0x9c => "ifge",
        0x9d => "ifgt",
        0x9e => "ifle",
        0x9f => "if_icmpeq",
        0xa0 => "if_icmpne",
        0xa1 => "if_icmplt",
        0xa2 => "if_icmpge",
        0xa3 => "if_icmpgt",
        0xa4 => "if_icmple",
        0xa5 => "if_acmpeq",
        0xa6 => "if_acmpne",
        0xa7 => "goto",
        0xa8 => "jsr",
        0xa9 => "ret",
        0xaa => "tableswitch",
        0xab => "lookupswitch",
        0xac => "ireturn",
        0xad => "lreturn",
        0xae => "freturn",
        0xaf => "dreturn",
        0xb0 => "areturn",
        0xb1 => "return",
        0xb2 => "getstatic",
        0xb3 => "putstatic",
        0xb4 => "getfield",
        0xb5 => "putfield",
        0xb6 => "invokevirtual",
        0xb7 => "invokespecial",
        0xb8 => "invokestatic",
        0xb9 => "invokeinterface",
        0xba => "invokedynamic",
        0xbb => "new",
        0xbc => "newarray",
        0xbd => "anewarray",
        0xbe => "arraylength",
        0xbf => "athrow",
        0xc0 => "checkcast",
        0xc1 => "instanceof",
        0xc2 => "monitorenter",
        0xc3 => "monitorexit",
        0xc4 => "wide",
        0xc5 => "multianewarray",
        0xc6 => "ifnull",
        0xc7 => "ifnonnull",
        0xc8 => "goto_w",
        0xc9 => "jsr_w",
        _ => "?",
    }
}

fn opcode_kind(opcode: u8) -> InstructionKind {
    match opcode {
        0x01..=0x14 => InstructionKind::Dataflow,
        0x15..=0x35 => InstructionKind::Dataflow,
        0x36..=0x4e => InstructionKind::Dataflow,
        0x4f..=0x55 => InstructionKind::Dataflow,
        0x60..=0x77 => InstructionKind::Dataflow,
        0x78..=0x83 => InstructionKind::Dataflow,
        0x84 => InstructionKind::Dataflow,
        0x85..=0x98 => InstructionKind::Dataflow,
        0xb2..=0xb5 => InstructionKind::Dataflow,
        0xb6..=0xba => InstructionKind::Call,
        _ => InstructionKind::Other,
    }
}

fn local_slot_to_location(slot: u16, param_slot_count: usize) -> Location {
    if is_parameter_slot(slot, param_slot_count) {
        Location::Parameter(slot)
    } else {
        Location::Register(slot)
    }
}

fn resolve_field_ref(cf: &ClassFile, cp_index: u16) -> ClassFileResult<FieldRef> {
    match cf.get_cp(cp_index)? {
        CpEntry::Fieldref {
            class_index,
            name_and_type_index,
        } => {
            let class_name = cf.get_class_name(*class_index)?.to_string();
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            Ok(FieldRef {
                class_name,
                field_name: name.to_string(),
                descriptor: descriptor.to_string(),
            })
        }
        _ => Err(ClassFileError::InvalidClassFile("expected Fieldref")),
    }
}

fn resolve_method_target(cf: &ClassFile, cp_index: u16) -> ClassFileResult<MethodTarget> {
    match cf.get_cp(cp_index)? {
        CpEntry::Methodref {
            class_index,
            name_and_type_index,
        }
        | CpEntry::InterfaceMethodref {
            class_index,
            name_and_type_index,
        } => {
            let class_name = cf.get_class_name(*class_index)?.to_string();
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            Ok(MethodTarget {
                class_name,
                method_name: name.to_string(),
                descriptor: descriptor.to_string(),
            })
        }
        _ => Err(ClassFileError::InvalidClassFile(
            "expected Methodref or InterfaceMethodref",
        )),
    }
}

fn resolve_constant(cf: &ClassFile, cp_index: u16) -> ClassFileResult<ConstantValue> {
    match cf.get_cp(cp_index)? {
        CpEntry::Integer(i) => Ok(ConstantValue::Integer(*i)),
        CpEntry::Long(l) => Ok(ConstantValue::Long(*l)),
        CpEntry::Float(f) => Ok(ConstantValue::Float(*f)),
        CpEntry::Double(d) => Ok(ConstantValue::Double(*d)),
        CpEntry::String { string_index } => {
            let s = cf.get_utf8(*string_index)?.to_string();
            Ok(ConstantValue::String(s))
        }
        CpEntry::Class { name_index } => {
            let s = cf.get_utf8(*name_index)?.to_string();
            Ok(ConstantValue::ClassRef(s))
        }
        _ => Err(ClassFileError::InvalidClassFile("expected constant entry")),
    }
}

/// Decode one instruction at `pc` into `InstructionFlowInfo`. Returns the next `pc`.
/// When the instruction is `wide` (0xc4), decodes the following sub-instruction as one
/// logical instruction (e.g. wide iload with 2-byte index) and does not yield a separate "wide" step.
pub fn decode_flow_instruction<'a>(
    code: &[u8],
    pc: usize,
    cf: &'a ClassFile,
    method: &'a MethodInfo,
) -> ClassFileResult<(InstructionFlowInfo<'a>, usize)> {
    if pc >= code.len() {
        return Err(ClassFileError::InvalidClassFile("pc past code"));
    }
    let opcode = code[pc];
    let len = instruction_length(code, pc)?;
    let next_pc = pc + len;

    let (logical_opcode, mnem, kind, dataflow, call) = if opcode == 0xc4 {
        if pc + 2 > code.len() {
            return Err(ClassFileError::InvalidClassFile("wide truncated"));
        }
        let subop = code[pc + 1];
        let param_slot_count = cf
            .get_utf8(method.descriptor_index)
            .map(descriptor_param_slot_count)
            .unwrap_or(0);
        let is_instance = (method.access_flags & 0x0008) == 0;
        let param_slots = param_slot_count;
        let kind = opcode_kind(subop);
        let mut dataflow = None;
        let call = None;
        if kind == InstructionKind::Dataflow {
            let (sources, destinations) =
                decode_dataflow(code, pc, cf, subop, param_slots, is_instance, true)?;
            dataflow = Some(DataflowInfo {
                sources,
                destinations,
            });
        }
        (subop, mnemonic(subop), kind, dataflow, call)
    } else {
        let mnem = mnemonic(opcode);
        let param_slot_count = cf
            .get_utf8(method.descriptor_index)
            .map(descriptor_param_slot_count)
            .unwrap_or(0);
        let is_instance = (method.access_flags & 0x0008) == 0;
        let param_slots = param_slot_count;
        let kind = opcode_kind(opcode);
        let mut dataflow = None;
        let mut call = None;
        if kind == InstructionKind::Dataflow {
            let (sources, destinations) =
                decode_dataflow(code, pc, cf, opcode, param_slots, is_instance, false)?;
            dataflow = Some(DataflowInfo {
                sources,
                destinations,
            });
        } else if kind == InstructionKind::Call {
            call = Some(decode_call(code, pc, cf, opcode)?);
        }
        (opcode, mnem, kind, dataflow, call)
    };

    let code_attr = method
        .code
        .as_ref()
        .ok_or(ClassFileError::InvalidClassFile(
            "method has no Code attribute",
        ))?;
    let file_byte_offset = code_attr
        .code_byte_offset_in_classfile
        .checked_add(pc as u32)
        .ok_or(ClassFileError::InvalidClassFile(
            "instruction file offset overflow",
        ))?;

    let info = InstructionFlowInfo {
        kind,
        method,
        class_file: cf,
        pc: pc as u32,
        file_byte_offset,
        byte_length: len as u32,
        opcode: logical_opcode,
        mnemonic: mnem,
        dataflow,
        call,
    };
    Ok((info, next_pc))
}

fn decode_dataflow(
    code: &[u8],
    pc: usize,
    cf: &ClassFile,
    opcode: u8,
    param_slot_count: usize,
    _is_instance: bool,
    wide: bool,
) -> ClassFileResult<(Vec<Location>, Vec<Location>)> {
    let mut sources = Vec::new();
    let mut destinations = Vec::new();
    let operand_start = if wide { pc + 2 } else { pc + 1 };

    match opcode {
        0x01 => {
            sources.push(Location::Constant(ConstantValue::Null));
            destinations.push(Location::StackOutput);
        }
        0x02..=0x0f => {
            sources.push(Location::Constant(const_for_const_op(opcode)?));
            destinations.push(Location::StackOutput);
        }
        0x10 => {
            let b = read_u8(code, pc + 1)?;
            sources.push(Location::Constant(ConstantValue::Integer(b as i8 as i32)));
            destinations.push(Location::StackOutput);
        }
        0x11 => {
            let s = read_i32_be(code, pc + 1)? as i16 as i32;
            sources.push(Location::Constant(ConstantValue::Integer(s)));
            destinations.push(Location::StackOutput);
        }
        0x12 | 0x13 => {
            let idx = if opcode == 0x12 {
                read_u8(code, pc + 1)? as u16
            } else {
                read_u16_be(code, pc + 1)?
            };
            let c = resolve_constant(cf, idx)?;
            sources.push(Location::Constant(c));
            destinations.push(Location::StackOutput);
        }
        0x14 => {
            let idx = read_u16_be(code, pc + 1)?;
            let c = resolve_constant(cf, idx)?;
            sources.push(Location::Constant(c));
            destinations.push(Location::StackOutput);
        }
        0x15..=0x19 => {
            let slot = if wide {
                read_u16_be(code, operand_start)?
            } else {
                read_u8(code, operand_start)? as u16
            };
            sources.push(local_slot_to_location(slot, param_slot_count));
            destinations.push(Location::StackOutput);
        }
        0x1a..=0x2d => {
            let slot = match opcode {
                0x1a..=0x1d => (opcode - 0x1a) as u16,
                0x1e..=0x21 => (opcode - 0x1e) as u16,
                0x22..=0x25 => (opcode - 0x22) as u16,
                0x26..=0x29 => (opcode - 0x26) as u16,
                0x2a..=0x2d => (opcode - 0x2a) as u16,
                _ => 0,
            };
            sources.push(local_slot_to_location(slot, param_slot_count));
            destinations.push(Location::StackOutput);
        }
        0x2e..=0x35 => {
            sources.push(Location::StackInput(0));
            sources.push(Location::StackInput(1));
            destinations.push(Location::StackOutput);
        }
        0x36..=0x3a => {
            let slot = if wide {
                read_u16_be(code, operand_start)?
            } else {
                read_u8(code, operand_start)? as u16
            };
            sources.push(Location::StackInput(0));
            destinations.push(local_slot_to_location(slot, param_slot_count));
        }
        0x3b..=0x4e => {
            let slot = match opcode {
                0x3b..=0x3e => (opcode - 0x3b) as u16,
                0x3f..=0x42 => (opcode - 0x3f) as u16,
                0x43..=0x46 => (opcode - 0x43) as u16,
                0x47..=0x4a => (opcode - 0x47) as u16,
                0x4b..=0x4e => (opcode - 0x4b) as u16,
                _ => 0,
            };
            sources.push(Location::StackInput(0));
            destinations.push(local_slot_to_location(slot, param_slot_count));
        }
        0x4f..=0x55 => {
            sources.push(Location::StackInput(0));
            sources.push(Location::StackInput(1));
            sources.push(Location::StackInput(2));
            destinations.push(Location::ArrayElement);
        }
        0x60..=0x77 | 0x78..=0x83 => {
            let (consume, _) = stack_effect(opcode);
            for i in 0..consume {
                sources.push(Location::StackInput(i));
            }
            destinations.push(Location::StackOutput);
        }
        0x84 => {
            let (idx, _const_val) = if wide {
                (
                    read_u16_be(code, operand_start)?,
                    read_u16_be(code, operand_start + 2)? as i16 as i32,
                )
            } else {
                (
                    read_u8(code, operand_start)? as u16,
                    code.get(operand_start + 1).copied().unwrap_or(0) as i8 as i32,
                )
            };
            sources.push(local_slot_to_location(idx, param_slot_count));
            destinations.push(local_slot_to_location(idx, param_slot_count));
        }
        0x85..=0x98 => {
            sources.push(Location::StackInput(0));
            if opcode >= 0x94 && opcode <= 0x98 {
                sources.push(Location::StackInput(1));
            }
            destinations.push(Location::StackOutput);
        }
        0xb2 => {
            let idx = read_u16_be(code, pc + 1)?;
            let fr = resolve_field_ref(cf, idx)?;
            sources.push(Location::FieldRef(fr.clone()));
            destinations.push(Location::StackOutput);
        }
        0xb3 => {
            let idx = read_u16_be(code, pc + 1)?;
            let fr = resolve_field_ref(cf, idx)?;
            sources.push(Location::StackInput(0));
            destinations.push(Location::FieldRef(fr));
        }
        0xb4 => {
            let idx = read_u16_be(code, pc + 1)?;
            let fr = resolve_field_ref(cf, idx)?;
            sources.push(Location::StackInput(0));
            sources.push(Location::FieldRef(fr.clone()));
            destinations.push(Location::StackOutput);
        }
        0xb5 => {
            let idx = read_u16_be(code, pc + 1)?;
            let fr = resolve_field_ref(cf, idx)?;
            sources.push(Location::StackInput(0));
            sources.push(Location::StackInput(1));
            destinations.push(Location::FieldRef(fr));
        }
        _ => {}
    }

    Ok((sources, destinations))
}

fn const_for_const_op(opcode: u8) -> ClassFileResult<ConstantValue> {
    Ok(match opcode {
        0x02 => ConstantValue::Integer(-1),
        0x03..=0x08 => ConstantValue::Integer((opcode - 0x03) as i32),
        0x09 | 0x0a => ConstantValue::Long((opcode - 0x09) as i64),
        0x0b..=0x0d => ConstantValue::Float((opcode - 0x0b) as u32),
        0x0e | 0x0f => ConstantValue::Double((opcode - 0x0e) as u64),
        _ => ConstantValue::Integer(0),
    })
}

fn stack_effect(opcode: u8) -> (u8, u8) {
    match opcode {
        0x60..=0x67 | 0x78..=0x83 => (2, 1),
        0x74..=0x77 => (1, 1),
        0x68..=0x73 => (2, 1),
        0x94..=0x98 => (2, 1),
        _ => (0, 1),
    }
}

fn decode_call(code: &[u8], pc: usize, cf: &ClassFile, opcode: u8) -> ClassFileResult<CallInfo> {
    let build_args = |param_slots: u8| -> Vec<Location> {
        // arguments[k] are slot-based; j=0 is the bottom-most argument slot.
        // StackInput depth is 0 at top-of-stack, so bottom-most arg is at depth param_slots-1.
        let mut args = Vec::with_capacity(param_slots as usize);
        for j in 0..param_slots {
            let depth = (param_slots - 1 - j) as u8;
            args.push(Location::StackInput(depth));
        }
        args
    };

    let (
        target,
        dynamic_bootstrap,
        dynamic_name_and_type,
        call_kind,
        stack_slots_consumed,
        receiver,
        arguments,
        return_value,
    ) = match opcode {
        0xb6 => {
            let idx = read_u16_be(code, pc + 1)?;
            let target = resolve_method_target(cf, idx)?;
            let desc = &target.descriptor;
            let param_slots = descriptor_param_slot_count(desc) as u8;
            let stack_slots_consumed = param_slots + 1; // receiver + args
            let receiver = Some(Location::StackInput(param_slots));
            let arguments = build_args(param_slots);
            let return_value = if returns_value(desc) {
                Some(Location::StackOutput)
            } else {
                None
            };
            (
                Some(target),
                None,
                None,
                CallKind::Virtual,
                stack_slots_consumed,
                receiver,
                arguments,
                return_value,
            )
        }
        0xb7 => {
            let idx = read_u16_be(code, pc + 1)?;
            let target = resolve_method_target(cf, idx)?;
            let desc = &target.descriptor;
            let param_slots = descriptor_param_slot_count(desc) as u8;
            let stack_slots_consumed = param_slots + 1; // receiver + args
            let receiver = Some(Location::StackInput(param_slots));
            let arguments = build_args(param_slots);
            let return_value = if returns_value(desc) {
                Some(Location::StackOutput)
            } else {
                None
            };
            (
                Some(target),
                None,
                None,
                CallKind::Special,
                stack_slots_consumed,
                receiver,
                arguments,
                return_value,
            )
        }
        0xb8 => {
            let idx = read_u16_be(code, pc + 1)?;
            let target = resolve_method_target(cf, idx)?;
            let desc = &target.descriptor;
            let param_slots = descriptor_param_slot_count(desc) as u8;
            let stack_slots_consumed = param_slots; // args only
            let receiver = None;
            let arguments = build_args(param_slots);
            let return_value = if returns_value(desc) {
                Some(Location::StackOutput)
            } else {
                None
            };
            (
                Some(target),
                None,
                None,
                CallKind::Static,
                stack_slots_consumed,
                receiver,
                arguments,
                return_value,
            )
        }
        0xb9 => {
            let idx = read_u16_be(code, pc + 1)?;
            let count = read_u8(code, pc + 3)?;
            let target = resolve_method_target(cf, idx).ok();
            let param_slots = count; // count is argument slots excluding receiver
            let stack_slots_consumed = param_slots + 1;
            let receiver = Some(Location::StackInput(param_slots));
            let arguments = build_args(param_slots);
            let return_value = target.as_ref().and_then(|t| {
                if returns_value(&t.descriptor) {
                    Some(Location::StackOutput)
                } else {
                    None
                }
            });
            (
                target,
                None,
                None,
                CallKind::Interface,
                stack_slots_consumed,
                receiver,
                arguments,
                return_value,
            )
        }
        0xba => {
            let idx = read_u16_be(code, pc + 1)?;
            let (name, desc) = match cf.get_cp(idx) {
                Ok(CpEntry::InvokeDynamic {
                    name_and_type_index,
                    ..
                }) => cf
                    .get_name_and_type(*name_and_type_index)
                    .map(|(n, d)| (n.to_string(), d.to_string())),
                _ => Err(ClassFileError::InvalidClassFile("expected InvokeDynamic")),
            }?;
            let bootstrap = match cf.get_cp(idx)? {
                CpEntry::InvokeDynamic {
                    bootstrap_method_attr_index,
                    ..
                } => *bootstrap_method_attr_index,
                _ => 0,
            };
            let name_type = format!("{}:{}", name, desc);
            let param_slots = descriptor_param_slot_count(&desc) as u8;
            let stack_slots_consumed = param_slots; // args only
            let receiver = None;
            let arguments = build_args(param_slots);
            let return_value = if returns_value(&desc) {
                Some(Location::StackOutput)
            } else {
                None
            };
            (
                None,
                Some(bootstrap),
                Some(name_type),
                CallKind::Dynamic,
                stack_slots_consumed,
                receiver,
                arguments,
                return_value,
            )
        }
        _ => (
            None,
            None,
            None,
            CallKind::Static,
            0,
            None,
            Vec::new(),
            None,
        ),
    };

    Ok(CallInfo {
        target,
        dynamic_bootstrap,
        dynamic_name_and_type,
        call_kind,
        stack_slots_consumed,
        receiver,
        arguments,
        return_value,
    })
}

// ============== Iterator ==============

/// Iterator that yields `InstructionFlowInfo` for every instruction in every method (with code) in the JAR.
pub struct InstructionFlowIter<'a> {
    class_parsers: Iter<'a, ClassFileParser>,
    current_methods: Option<std::vec::IntoIter<(&'a ClassFile, &'a MethodInfo)>>,
    current_code: Option<&'a [u8]>,
    current_cf: Option<&'a ClassFile>,
    current_method: Option<&'a MethodInfo>,
    method_pc: usize,
    method_done: bool,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> InstructionFlowIter<'a> {
    pub(crate) fn new(parsers: &'a [ClassFileParser]) -> Self {
        let mut class_parsers = parsers.iter();
        let first_parser = class_parsers.next();
        let (current_methods, current_code, current_cf, current_method) =
            if let Some(p) = first_parser {
                let cf = p.class_file();
                let methods: Vec<_> = p.methods().map(|m| (cf, m)).collect();
                let mut into_iter = methods.into_iter();
                let first = into_iter.next();
                if let Some((cf, m)) = first {
                    let code = m.code.as_ref().map(|c| c.code.as_slice());
                    (Some(into_iter), code, Some(cf), Some(m))
                } else {
                    (Some(into_iter), None, Some(cf), None)
                }
            } else {
                (None, None, None, None)
            };
        InstructionFlowIter {
            class_parsers,
            current_methods,
            current_code: current_code,
            current_cf: current_cf,
            current_method: current_method,
            method_pc: 0,
            method_done: current_code.is_none(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<'a> Iterator for InstructionFlowIter<'a> {
    type Item = ClassFileResult<InstructionFlowInfo<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.method_done || self.current_cf.is_none() || self.current_method.is_none() {
                self.method_pc = 0;
                self.method_done = false;
                if let Some(ref mut methods) = self.current_methods {
                    if let Some((cf, method)) = methods.next() {
                        self.current_cf = Some(cf);
                        self.current_method = Some(method);
                        self.current_code = method.code.as_ref().map(|c| c.code.as_slice());
                        self.method_done = self.current_code.is_none();
                        continue;
                    }
                }
                if let Some(next_parser) = self.class_parsers.next() {
                    let cf = next_parser.class_file();
                    let methods: Vec<_> = next_parser.methods().map(|m| (cf, m)).collect();
                    self.current_methods = Some(methods.into_iter());
                    self.current_cf = Some(cf);
                    self.current_method = None;
                    self.current_code = None;
                    continue;
                }
                return None;
            }

            let code = self.current_code.unwrap();
            let cf = self.current_cf.unwrap();
            let method = self.current_method.unwrap();

            if self.method_pc >= code.len() {
                self.method_done = true;
                continue;
            }

            match decode_flow_instruction(code, self.method_pc, cf, method) {
                Ok((info, next_pc)) => {
                    self.method_pc = next_pc;
                    if self.method_pc >= code.len() {
                        self.method_done = true;
                    }
                    return Some(Ok(info));
                }
                Err(e) => {
                    self.method_done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}
