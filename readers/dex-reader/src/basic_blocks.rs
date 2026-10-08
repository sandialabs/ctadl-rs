//! Basic block view of a decoded instruction stream.
//!
//! Uses control-flow instructions and their targets to partition
//! `Vec<DecodedCodeItem>` into basic blocks (index ranges).

use std::collections::{BTreeSet, HashMap};

use crate::instructions::Instruction;
use crate::parser::DecodedCodeItem;
use crate::types::CodeItem;

/// Collects all offsets that start a basic block: entry (0), branch targets, and
/// fall-through successors of conditional/switch instructions.
pub fn control_flow_targets(code: &CodeItem, items: &[DecodedCodeItem]) -> BTreeSet<usize> {
    let mut starts = BTreeSet::new();
    starts.insert(0);

    for (i, item) in items.iter().enumerate() {
        let DecodedCodeItem::Instruction { offset: pc, inst } = item else {
            continue;
        };
        let fall_through = (i + 1 < items.len()).then(|| items[i + 1].offset());

        if protected_handler_targets(code, *pc).next().is_some() {
            for handler in protected_handler_targets(code, *pc) {
                starts.insert(handler);
            }
            if inst.can_throw() {
                if let Some(next) = fall_through {
                    starts.insert(next);
                }
            }
        }

        if !inst.is_control_flow() {
            continue;
        }

        match inst {
            Instruction::Goto(f) => {
                let off = f.off as i32;
                add_target(&mut starts, *pc, off);
            }
            Instruction::Goto16(f) => {
                let off = f.off as i32;
                add_target(&mut starts, *pc, off);
            }
            Instruction::Goto32(f) => {
                add_target(&mut starts, *pc, f.off);
            }
            Instruction::IfEq(f)
            | Instruction::IfNe(f)
            | Instruction::IfLt(f)
            | Instruction::IfGe(f)
            | Instruction::IfGt(f)
            | Instruction::IfLe(f) => {
                let tgt = f.tgt as i32;
                add_target(&mut starts, *pc, tgt);
                if let Some(next) = fall_through {
                    starts.insert(next);
                }
            }
            Instruction::IfEqz(f)
            | Instruction::IfNez(f)
            | Instruction::IfLtz(f)
            | Instruction::IfGez(f)
            | Instruction::IfGtz(f)
            | Instruction::IfLez(f) => {
                let tgt = f.tgt as i32;
                add_target(&mut starts, *pc, tgt);
                if let Some(next) = fall_through {
                    starts.insert(next);
                }
            }
            Instruction::PackedSwitch(f) | Instruction::SparseSwitch(f) => {
                if let Some(next) = fall_through {
                    starts.insert(next);
                }
                let payload_offset = (*pc as i32 + f.tgt) as usize;
                if let Some(payload) = items.iter().find(|it| it.offset() == payload_offset) {
                    if let DecodedCodeItem::Payload {
                        offset: _,
                        payload: p,
                    } = payload
                    {
                        for &rel in p.targets() {
                            add_target(&mut starts, *pc, rel);
                        }
                    }
                }
            }
            Instruction::Throw(_)
            | Instruction::Return(_)
            | Instruction::ReturnVoid(_)
            | Instruction::ReturnWide(_)
            | Instruction::ReturnObject(_) => {}
            _ => {}
        }
    }

    starts
}

fn add_target(starts: &mut BTreeSet<usize>, pc: usize, offset: i32) {
    if let Ok(delta) = i32::try_from(pc) {
        if let Some(t) = delta.checked_add(offset) {
            if t >= 0 {
                starts.insert(t as usize);
            }
        }
    }
}

fn resolve_target(pc: usize, offset: i32) -> Option<usize> {
    let delta = i32::try_from(pc).ok()?;
    let t = delta.checked_add(offset)?;
    if t >= 0 { Some(t as usize) } else { None }
}

fn protected_handler_targets(code: &CodeItem, pc: usize) -> impl Iterator<Item = usize> + '_ {
    let mut targets = Vec::new();
    let Some(handlers) = &code.handlers else {
        return targets.into_iter();
    };
    let pc = pc as u32;

    for try_item in &code.tries {
        let start = try_item.start_addr;
        let end = start + u32::from(try_item.insn_count);
        if !(start..end).contains(&pc) {
            continue;
        }

        if let Some(handler) = handlers.get_by_off(try_item.handler_off) {
            for pair in &handler.pairs {
                targets.push(pair.addr as usize);
            }
            if let Some(addr) = handler.catch_all_addr {
                targets.push(addr as usize);
            }
        }
    }

    targets.sort_unstable();
    targets.dedup();
    targets.into_iter()
}

/// One basic block: a contiguous range of indices into the decoded instruction list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicBlock {
    /// Start index (inclusive) into the `DecodedCodeItem` slice.
    pub start: usize,
    /// End index (exclusive).
    pub end: usize,
}

impl BasicBlock {
    /// View this block’s instructions as a slice of the full stream.
    #[inline]
    pub fn instructions<'a>(&self, items: &'a [DecodedCodeItem]) -> &'a [DecodedCodeItem] {
        &items[self.start..self.end]
    }
}

/// Partitions the instruction stream into basic blocks using control-flow targets.
pub fn basic_blocks(code: &CodeItem, items: &[DecodedCodeItem]) -> Vec<BasicBlock> {
    let starts = control_flow_targets(code, items);
    if starts.is_empty() {
        return Vec::new();
    }

    let mut blocks = Vec::with_capacity(starts.len());
    let start_offsets: Vec<usize> = starts.into_iter().collect();

    for (idx, &block_start_offset) in start_offsets.iter().enumerate() {
        let start_index = items
            .iter()
            .position(|it| it.offset() >= block_start_offset)
            .unwrap_or(items.len());
        let end_offset = start_offsets.get(idx + 1).copied().unwrap_or(usize::MAX);
        let end_index = items
            .iter()
            .position(|it| it.offset() >= end_offset)
            .unwrap_or(items.len());
        if start_index < end_index {
            blocks.push(BasicBlock {
                start: start_index,
                end: end_index,
            });
        }
    }

    blocks
}

/// For each basic block (in the order of `basic_blocks(items)`), returns the list of
/// successor block indices implied by the block's terminator.
pub fn block_successors(code: &CodeItem, items: &[DecodedCodeItem]) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return Vec::new();
    }
    let blocks = basic_blocks(code, items);
    if blocks.is_empty() {
        return Vec::new();
    }

    let offset_to_block: HashMap<usize, usize> = blocks
        .iter()
        .enumerate()
        .map(|(b, block)| (items[block.start].offset(), b))
        .collect();

    let mut result = Vec::with_capacity(blocks.len());
    for (block_idx, block) in blocks.iter().enumerate() {
        let terminator_index = (block.start..block.end)
            .rfind(|&j| matches!(items[j], DecodedCodeItem::Instruction { .. }));

        let succs = match terminator_index.and_then(|i| {
            let DecodedCodeItem::Instruction { offset: pc, inst } = &items[i] else {
                return None;
            };
            Some((i, *pc, inst))
        }) {
            None => {
                // No instruction or fall-through only
                if block_idx + 1 < blocks.len() {
                    vec![block_idx + 1]
                } else {
                    vec![]
                }
            }
            Some((i, pc, inst)) if !inst.is_control_flow() => {
                let fall_through_block = (i + 1 < items.len())
                    .then(|| items[i + 1].offset())
                    .and_then(|off| offset_to_block.get(&off).copied());
                let mut s = Vec::new();
                if let Some(b) = fall_through_block {
                    s.push(b);
                }
                if inst.can_throw() {
                    for target in protected_handler_targets(code, pc) {
                        if let Some(&b) = offset_to_block.get(&target) {
                            if !s.contains(&b) {
                                s.push(b);
                            }
                        }
                    }
                }
                s
            }
            Some((i, pc, inst)) => {
                let fall_through_block = (i + 1 < items.len())
                    .then(|| items[i + 1].offset())
                    .and_then(|off| offset_to_block.get(&off).copied());

                let mut succs = match inst {
                    Instruction::Throw(_)
                    | Instruction::Return(_)
                    | Instruction::ReturnVoid(_)
                    | Instruction::ReturnWide(_)
                    | Instruction::ReturnObject(_) => vec![],
                    Instruction::Goto(f) => {
                        let off = f.off as i32;
                        resolve_target(pc, off)
                            .and_then(|tgt| offset_to_block.get(&tgt).copied())
                            .map(|b| vec![b])
                            .unwrap_or_default()
                    }
                    Instruction::Goto16(f) => {
                        let off = f.off as i32;
                        resolve_target(pc, off)
                            .and_then(|tgt| offset_to_block.get(&tgt).copied())
                            .map(|b| vec![b])
                            .unwrap_or_default()
                    }
                    Instruction::Goto32(f) => resolve_target(pc, f.off)
                        .and_then(|tgt| offset_to_block.get(&tgt).copied())
                        .map(|b| vec![b])
                        .unwrap_or_default(),
                    Instruction::IfEq(f)
                    | Instruction::IfNe(f)
                    | Instruction::IfLt(f)
                    | Instruction::IfGe(f)
                    | Instruction::IfGt(f)
                    | Instruction::IfLe(f) => {
                        let tgt = f.tgt as i32;
                        let mut s = Vec::with_capacity(2);
                        if let Some(t) = resolve_target(pc, tgt) {
                            if let Some(&b) = offset_to_block.get(&t) {
                                s.push(b);
                            }
                        }
                        if let Some(b) = fall_through_block {
                            s.push(b);
                        }
                        s
                    }
                    Instruction::IfEqz(f)
                    | Instruction::IfNez(f)
                    | Instruction::IfLtz(f)
                    | Instruction::IfGez(f)
                    | Instruction::IfGtz(f)
                    | Instruction::IfLez(f) => {
                        let tgt = f.tgt as i32;
                        let mut s = Vec::with_capacity(2);
                        if let Some(t) = resolve_target(pc, tgt) {
                            if let Some(&b) = offset_to_block.get(&t) {
                                s.push(b);
                            }
                        }
                        if let Some(b) = fall_through_block {
                            s.push(b);
                        }
                        s
                    }
                    Instruction::PackedSwitch(f) | Instruction::SparseSwitch(f) => {
                        let mut s = Vec::new();
                        if let Some(b) = fall_through_block {
                            s.push(b);
                        }
                        let payload_offset = (pc as i32 + f.tgt) as usize;
                        if let Some(payload) = items.iter().find(|it| it.offset() == payload_offset)
                        {
                            if let DecodedCodeItem::Payload {
                                offset: _,
                                payload: p,
                            } = payload
                            {
                                for &rel in p.targets() {
                                    if let Some(t) = resolve_target(pc, rel) {
                                        if let Some(&b) = offset_to_block.get(&t) {
                                            if !s.contains(&b) {
                                                s.push(b);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        s
                    }
                    _ => {
                        if block_idx + 1 < blocks.len() {
                            vec![block_idx + 1]
                        } else {
                            vec![]
                        }
                    }
                };

                if inst.can_throw() {
                    for target in protected_handler_targets(code, pc) {
                        if let Some(&b) = offset_to_block.get(&target) {
                            if !succs.contains(&b) {
                                succs.push(b);
                            }
                        }
                    }
                }

                succs
            }
        };
        result.push(succs);
    }
    result
}

#[cfg(test)]
mod tests {
    use crate::instructions::{Format10t, Format10x, Instruction};
    use crate::parser::DecodedCodeItem;
    use crate::types::{CatchHandlerList, CodeItem, EncodedCatchHandler, TryItem, TypeAddrPair};

    fn empty_code_item() -> CodeItem {
        CodeItem {
            registers_size: 0,
            ins_size: 0,
            outs_size: 0,
            tries_size: 0,
            debug_info_off: 0,
            insns: Vec::new(),
            tries: Vec::new(),
            handlers: None,
            code_off: 0,
        }
    }

    #[test]
    fn block_successors_goto_and_return() {
        // Block 0: Goto +2 (target offset 2). Block 1: ReturnVoid (no successors).
        let items: Vec<DecodedCodeItem> = vec![
            DecodedCodeItem::Instruction {
                offset: 0,
                inst: Instruction::Goto(Format10t { off: 2 }),
            },
            DecodedCodeItem::Instruction {
                offset: 2,
                inst: Instruction::ReturnVoid(Format10x),
            },
        ];
        let code = empty_code_item();
        let blocks = super::basic_blocks(&code, &items);
        assert_eq!(blocks.len(), 2);
        let succs = super::block_successors(&code, &items);
        assert_eq!(succs.len(), 2);
        assert_eq!(succs[0], vec![1]); // goto targets block 1
        assert_eq!(succs[1], vec![]); // return has no successors
    }

    #[test]
    fn protected_throwing_instruction_jumps_to_handler() {
        let items: Vec<DecodedCodeItem> = vec![
            DecodedCodeItem::Instruction {
                offset: 0,
                inst: Instruction::ConstString(crate::instructions::Format21c {
                    a: 0u32.into(),
                    idx: 0u32.into(),
                }),
            },
            DecodedCodeItem::Instruction {
                offset: 2,
                inst: Instruction::ReturnVoid(Format10x),
            },
            DecodedCodeItem::Instruction {
                offset: 3,
                inst: Instruction::MoveException(crate::instructions::Format11x { a: 0u32.into() }),
            },
            DecodedCodeItem::Instruction {
                offset: 4,
                inst: Instruction::ReturnVoid(Format10x),
            },
        ];
        let code = CodeItem {
            tries_size: 1,
            tries: vec![TryItem {
                start_addr: 0,
                insn_count: 2,
                handler_off: 0,
            }],
            handlers: Some(CatchHandlerList {
                size: 1,
                handlers: vec![EncodedCatchHandler {
                    raw_size: 1,
                    pairs: vec![TypeAddrPair {
                        type_idx: 0,
                        addr: 3,
                    }],
                    catch_all_addr: None,
                    start_off: 0,
                }],
            }),
            ..empty_code_item()
        };

        let starts = super::control_flow_targets(&code, &items);
        assert!(starts.contains(&3));

        let blocks = super::basic_blocks(&code, &items);
        assert_eq!(blocks.len(), 3);

        let succs = super::block_successors(&code, &items);
        assert_eq!(succs[0], vec![1, 2]);
        assert_eq!(succs[1], vec![]);
        assert_eq!(succs[2], vec![]);
    }
}
