/*! Pcode constant propagation.

# Constant Propagation

The constant propagation uses a simple domain where each varnode `x` maps to `option(y) + c`
meaning that "varnode `x` is equal to `y` plus the constant `c`". If `option(y)` is none it means
that `x = c`. As a special case, the initial value for each varnode is `x = x + 0`.

Constant propagation is one rule per instruction, from its inputs' values to its output's, using
lattice-aware arithmetic operations on [`SymbolicProp`], solved to a fixpoint by
[`compute_constant_propagation`].

# Assumptions

- The stack pointer register is presumed to be top of stack. I think this will cause issues if it
  gets modified in a "non-standard" way in the function.
*/
use crate::{PcodeFacts, PcodeVarnode};
use ascent_base::lattice::Lattice;
use hashbrown::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub enum SymbolicProp {
    #[default]
    Bottom,
    Value(Option<PcodeVarnode>, i64),
    Top,
}

impl Lattice for SymbolicProp {
    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (SymbolicProp::Top, x) | (x, SymbolicProp::Top) => x,
            (SymbolicProp::Bottom, _) | (_, SymbolicProp::Bottom) => SymbolicProp::Bottom,
            (SymbolicProp::Value(v1, o1), SymbolicProp::Value(v2, o2)) => {
                if v1 == v2 && o1 == o2 {
                    SymbolicProp::Value(v1, o1)
                } else {
                    SymbolicProp::Bottom
                }
            }
        }
    }

    fn join(self, other: Self) -> Self {
        match (self, other) {
            (SymbolicProp::Bottom, x) | (x, SymbolicProp::Bottom) => x,
            (SymbolicProp::Top, _) | (_, SymbolicProp::Top) => SymbolicProp::Top,
            (SymbolicProp::Value(v1, o1), SymbolicProp::Value(v2, o2)) => {
                if v1 == v2 && o1 == o2 {
                    SymbolicProp::Value(v1, o1)
                } else {
                    SymbolicProp::Top
                }
            }
        }
    }

    fn meet_mut(&mut self, other: Self) -> bool {
        let met = self.clone().meet(other);
        if met != *self {
            *self = met;
            true
        } else {
            false
        }
    }

    fn join_mut(&mut self, other: Self) -> bool {
        let joined = self.clone().join(other);
        if joined != *self {
            *self = joined;
            true
        } else {
            false
        }
    }
}

impl SymbolicProp {
    pub fn add(&self, other: &Self) -> Self {
        match (self, other) {
            (SymbolicProp::Top, _) | (_, SymbolicProp::Top) => SymbolicProp::Top,
            (SymbolicProp::Bottom, _) | (_, SymbolicProp::Bottom) => SymbolicProp::Bottom,
            (SymbolicProp::Value(v1, o1), SymbolicProp::Value(v2, o2)) => {
                if v1.is_none() {
                    SymbolicProp::Value(v2.clone(), o1.wrapping_add(*o2))
                } else if v2.is_none() {
                    SymbolicProp::Value(v1.clone(), o1.wrapping_add(*o2))
                } else {
                    SymbolicProp::Top
                }
            }
        }
    }

    pub fn sub(&self, other: &Self) -> Self {
        match (self, other) {
            (SymbolicProp::Top, _) | (_, SymbolicProp::Top) => SymbolicProp::Top,
            (SymbolicProp::Bottom, _) | (_, SymbolicProp::Bottom) => SymbolicProp::Bottom,
            (SymbolicProp::Value(v1, o1), SymbolicProp::Value(v2, o2)) => {
                if v2.is_none() {
                    SymbolicProp::Value(v1.clone(), o1.wrapping_sub(*o2))
                } else if v1 == v2 {
                    SymbolicProp::Value(None, o1.wrapping_sub(*o2))
                } else {
                    SymbolicProp::Top
                }
            }
        }
    }

    pub fn ptradd(&self, index: &Self, size: i64) -> Self {
        match (self, index) {
            (SymbolicProp::Top, _) | (_, SymbolicProp::Top) => SymbolicProp::Top,
            (SymbolicProp::Bottom, _) | (_, SymbolicProp::Bottom) => SymbolicProp::Bottom,
            (SymbolicProp::Value(v1, o1), SymbolicProp::Value(v2, o2)) => {
                if v2.is_none() {
                    SymbolicProp::Value(v1.clone(), o1.wrapping_add(o2.wrapping_mul(size)))
                } else {
                    SymbolicProp::Top
                }
            }
        }
    }
}

/// One instruction's rule: a varnode index for the output, then for each input, and a
/// `PTRADD`'s element size.
#[derive(Clone, Copy)]
enum Op {
    /// `out ⊒ a`
    Copy(u32, u32),
    /// `out ⊒ a + b`
    Add(u32, u32, u32),
    /// `out ⊒ a - b`
    Sub(u32, u32, u32),
    /// `out ⊒ a + b * size`
    PtrAdd(u32, u32, u32, i64),
    /// `out ⊒ a + b`, a `PTRSUB`
    PtrSub(u32, u32, u32),
}

impl Op {
    fn output(self) -> u32 {
        match self {
            Op::Copy(out, ..)
            | Op::Add(out, ..)
            | Op::Sub(out, ..)
            | Op::PtrAdd(out, ..)
            | Op::PtrSub(out, ..) => out,
        }
    }

    fn inputs(self) -> impl Iterator<Item = u32> {
        let (a, b) = match self {
            Op::Copy(_, a) => (a, None),
            Op::Add(_, a, b) | Op::Sub(_, a, b) | Op::PtrAdd(_, a, b, _) | Op::PtrSub(_, a, b) => {
                (a, Some(b))
            }
        };
        std::iter::once(a).chain(b)
    }

    /// The value the rule derives for its output from `values`, once its inputs have one.
    fn eval(self, values: &[Option<SymbolicProp>]) -> Option<SymbolicProp> {
        let value = |i: u32| values[i as usize].as_ref();
        Some(match self {
            Op::Copy(_, a) => value(a)?.clone(),
            Op::Add(_, a, b) | Op::PtrSub(_, a, b) => value(a)?.add(value(b)?),
            Op::Sub(_, a, b) => value(a)?.sub(value(b)?),
            Op::PtrAdd(_, a, b, size) => value(a)?.ptradd(value(b)?, size),
        })
    }
}

/// Varnodes numbered densely, for [`compute_constant_propagation`].
#[derive(Default)]
struct Varnodes<'f> {
    ids: HashMap<&'f PcodeVarnode, u32>,
    names: Vec<&'f PcodeVarnode>,
}

impl<'f> Varnodes<'f> {
    fn id(&mut self, vn: &'f PcodeVarnode) -> u32 {
        *self.ids.entry(vn).or_insert_with(|| {
            self.names.push(vn);
            (self.names.len() - 1) as u32
        })
    }
}

/// Runs constant propagation over every instruction in `facts`: each varnode's value is the
/// least one that is at least its initial value and what every instruction writing it derives
/// from its inputs' values.
///
/// The rules are monotone in the lattice, so the fixpoint does not depend on the order they
/// are applied in. A worklist applies them: an instruction is re-evaluated when one of its
/// inputs' value changes, which, with a lattice this short, is at most twice per input.
pub fn compute_constant_propagation(facts: &PcodeFacts) -> BTreeMap<PcodeVarnode, SymbolicProp> {
    let mut defined_vars: HashSet<&PcodeVarnode> = HashSet::new();
    for pcode in facts.pcode_facts.values() {
        defined_vars.extend(pcode.outputs.iter());
    }

    let stack_top_vn = PcodeVarnode::from("__stack_top");
    // Numbering the varnodes in `vnode_facts` first, in its order, keeps the result in key
    // order but for varnodes that only instructions mention.
    let mut varnodes = Varnodes::default();
    varnodes.ids.reserve(facts.vnode_facts.len());
    for vn in facts.vnode_facts.keys() {
        varnodes.id(vn);
    }

    let mut initial_value: Vec<(u32, SymbolicProp)> = Vec::with_capacity(facts.vnode_facts.len());
    initial_value.push((
        varnodes.id(&stack_top_vn),
        SymbolicProp::Value(Some(stack_top_vn.clone()), 0),
    ));

    let sp_offsets: BTreeSet<i64> = facts
        .register_facts
        .iter()
        .filter(|r| r.is_stack_pointer)
        .map(|r| r.offset)
        .collect();

    // Fill initial values and operations
    for (id, (vnode, data)) in facts.vnode_facts.iter().enumerate() {
        let id = id as u32;
        let is_const_space = data.space() == Some("const");
        let is_stack_space = data.space() == Some("stack");
        let is_register_space = data.space() == Some("register");

        if is_const_space {
            if let Some(offset) = data.constant_offset {
                initial_value.push((id, SymbolicProp::Value(None, offset)));
            } else if let Some(addr) = &data.address {
                initial_value.push((id, SymbolicProp::Value(None, addr.0)));
            }
        } else if is_stack_space {
            if let Some(offset) = data.constant_offset {
                initial_value.push((id, SymbolicProp::Value(Some(stack_top_vn.clone()), offset)));
            } else if let Some(addr) = &data.address {
                initial_value.push((id, SymbolicProp::Value(Some(stack_top_vn.clone()), addr.0)));
            }
        } else if !defined_vars.contains(vnode) {
            let is_sp = is_register_space
                && data
                    .address
                    .as_ref()
                    .is_some_and(|addr| sp_offsets.contains(&addr.0));
            if is_sp {
                initial_value.push((id, SymbolicProp::Value(Some(stack_top_vn.clone()), 0)));
            } else {
                // Variables that are NOT defined in this function (parameters, globals, etc.)
                // start as "self + 0"
                initial_value.push((id, SymbolicProp::Value(Some(vnode.clone()), 0)));
            }
        }
    }

    let mut ops = Vec::new();
    for pcode in facts.pcode_facts.values() {
        if pcode.outputs.is_empty() {
            continue;
        }
        let out = varnodes.id(&pcode.outputs[0]);
        let mut input = |i: usize| varnodes.id(&pcode.inputs[i]);

        let mnemonic = pcode.mnemonic.deref().deref();
        match mnemonic {
            "COPY" | "MULTIEQUAL" | "INDIRECT" | "CAST" | "TRUNC" | "INT_SEXT" | "INT_ZEXT" => {
                for i in 0..pcode.inputs.len() {
                    ops.push(Op::Copy(out, input(i)));
                }
            }
            "INT_ADD" if pcode.inputs.len() >= 2 => ops.push(Op::Add(out, input(0), input(1))),
            "INT_SUB" if pcode.inputs.len() >= 2 => ops.push(Op::Sub(out, input(0), input(1))),
            "PTRADD" if pcode.inputs.len() >= 3 => {
                let size_vn = &pcode.inputs[2];
                if let Some(addr) = facts
                    .vnode_facts
                    .get(size_vn)
                    .and_then(|data| data.address.as_ref())
                {
                    ops.push(Op::PtrAdd(out, input(0), input(1), addr.0));
                }
            }
            "PTRSUB" if pcode.inputs.len() >= 2 => ops.push(Op::PtrSub(out, input(0), input(1))),
            _ => {}
        }
    }

    // The instructions reading each varnode, `readers[starts[v]..starts[v + 1]]`.
    let n = varnodes.names.len();
    let mut starts = vec![0u32; n + 1];
    for op in &ops {
        for input in op.inputs() {
            starts[input as usize + 1] += 1;
        }
    }
    for v in 0..n {
        starts[v + 1] += starts[v];
    }
    let mut readers = vec![0u32; starts[n] as usize];
    let mut next = starts.clone();
    for (i, op) in ops.iter().enumerate() {
        for input in op.inputs() {
            readers[next[input as usize] as usize] = i as u32;
            next[input as usize] += 1;
        }
    }

    let mut values: Vec<Option<SymbolicProp>> = vec![None; n];
    let mut work = Vec::new();
    let join = |values: &mut [Option<SymbolicProp>], work: &mut Vec<u32>, v: u32, value| {
        let changed = match &mut values[v as usize] {
            Some(old) => old.join_mut(value),
            slot @ None => {
                *slot = Some(value);
                true
            }
        };
        if changed {
            work.push(v);
        }
    };
    for (v, value) in initial_value {
        join(&mut values, &mut work, v, value);
    }
    while let Some(v) = work.pop() {
        let (start, end) = (starts[v as usize] as usize, starts[v as usize + 1] as usize);
        for &op in &readers[start..end] {
            let op = ops[op as usize];
            if let Some(value) = op.eval(&values) {
                join(&mut values, &mut work, op.output(), value);
            }
        }
    }

    varnodes
        .names
        .into_iter()
        .zip(values)
        .filter_map(|(vn, value)| Some((vn.clone(), value?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn test_constant_propagation() {
        let mut facts = PcodeFacts::default();

        let v1 = PcodeVarnode::from("v1");
        let v2 = PcodeVarnode::from("v2");
        let c5 = PcodeVarnode::from("c5");
        let v3 = PcodeVarnode::from("v3");

        facts.vnode_facts.insert(
            v1.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            c5.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(5)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v2.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v3.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        // v2 = v1 + 5
        let inst1 = PcodeInstruction::from("inst1");
        facts.pcode_facts.insert(
            inst1.clone(),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![v1.clone(), c5.clone()],
                outputs: smallvec::smallvec![v2.clone()],
                bb_id: None,
                target: None,
            },
        );

        // v3 = v2 + 5
        let inst2 = PcodeInstruction::from("inst2");
        facts.pcode_facts.insert(
            inst2.clone(),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![v2.clone(), c5.clone()],
                outputs: smallvec::smallvec![v3.clone()],
                bb_id: None,
                target: None,
            },
        );

        let results = compute_constant_propagation(&facts);

        assert_eq!(
            results.get(&v1),
            Some(&SymbolicProp::Value(Some(v1.clone()), 0))
        );
        assert_eq!(results.get(&c5), Some(&SymbolicProp::Value(None, 5)));
        assert_eq!(
            results.get(&v2),
            Some(&SymbolicProp::Value(Some(v1.clone()), 5))
        );
        assert_eq!(
            results.get(&v3),
            Some(&SymbolicProp::Value(Some(v1.clone()), 10))
        );
    }

    #[test]
    fn test_constant_propagation_with_offset() {
        let mut facts = PcodeFacts::default();

        let v1 = PcodeVarnode::from("v1");
        let v2 = PcodeVarnode::from("v2");
        let k10 = PcodeVarnode::from("k10");

        facts.vnode_facts.insert(
            v1.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            k10.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: None,
                constant_offset: Some(10),
            },
        );

        facts.vnode_facts.insert(
            v2.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        // v2 = v1 + k10
        let inst1 = PcodeInstruction::from("inst1");
        facts.pcode_facts.insert(
            inst1.clone(),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![v1.clone(), k10.clone()],
                outputs: smallvec::smallvec![v2.clone()],
                bb_id: None,
                target: None,
            },
        );

        let results = compute_constant_propagation(&facts);

        assert_eq!(
            results.get(&v1),
            Some(&SymbolicProp::Value(Some(v1.clone()), 0))
        );
        assert_eq!(results.get(&k10), Some(&SymbolicProp::Value(None, 10)));
        assert_eq!(
            results.get(&v2),
            Some(&SymbolicProp::Value(Some(v1.clone()), 10))
        );
    }

    #[test]
    fn test_transitive_addition() {
        let mut facts = PcodeFacts::default();
        let t = PcodeVarnode::from("t");
        let t1 = PcodeVarnode::from("t1");
        let t2 = PcodeVarnode::from("t2");
        let c2 = PcodeVarnode::from("c2");
        let c4 = PcodeVarnode::from("c4");

        // Define varnodes
        for (vn, _name, space, offset) in [
            (&t, "t", "register", None),
            (&t1, "t1", "register", None),
            (&t2, "t2", "register", None),
            (&c2, "2", "const", Some(2)),
            (&c4, "4", "const", Some(4)),
        ] {
            facts.vnode_facts.insert(
                vn.clone(),
                VnodeData {
                    size: Some(8),
                    space: Some(space.into()),
                    address: offset.map(PcodeAddress),
                    constant_offset: None,
                },
            );
        }

        // t1 = t + 2
        facts.pcode_facts.insert(
            PcodeInstruction::from("i1"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![t.clone(), c2.clone()],
                outputs: smallvec::smallvec![t1.clone()],
                bb_id: None,
                target: None,
            },
        );

        // t2 = t1 + 4
        facts.pcode_facts.insert(
            PcodeInstruction::from("i2"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![t1.clone(), c4.clone()],
                outputs: smallvec::smallvec![t2.clone()],
                bb_id: None,
                target: None,
            },
        );

        let results = compute_constant_propagation(&facts);
        assert_eq!(
            results.get(&t2),
            Some(&SymbolicProp::Value(Some(t.clone()), 6))
        );
    }

    #[test]
    fn test_stack_relative_propagation() {
        let mut facts = PcodeFacts::default();
        let stack_vn = PcodeVarnode::from("stack_vn");
        let v1 = PcodeVarnode::from("v1");
        let c8 = PcodeVarnode::from("c8");

        facts.vnode_facts.insert(
            stack_vn.clone(),
            VnodeData {
                size: Some(8),
                space: Some("stack".into()),
                address: Some(PcodeAddress(-16)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            c8.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(8)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v1.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: None,
                constant_offset: None,
            },
        );

        // v1 = stack_vn + 8
        facts.pcode_facts.insert(
            PcodeInstruction::from("i1"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                inputs: smallvec::smallvec![stack_vn.clone(), c8.clone()],
                outputs: smallvec::smallvec![v1.clone()],
                bb_id: None,
                target: None,
            },
        );

        let results = compute_constant_propagation(&facts);
        let stack_top = PcodeVarnode::from("__stack_top");

        assert_eq!(
            results.get(&stack_vn),
            Some(&SymbolicProp::Value(Some(stack_top.clone()), -16))
        );
        assert_eq!(
            results.get(&v1),
            Some(&SymbolicProp::Value(Some(stack_top), -8))
        );
    }

    #[test]
    fn test_register_sp_propagation() {
        let mut facts = PcodeFacts::default();
        let rsp = PcodeVarnode::from("rsp_vn");

        facts.register_facts.push(RegisterData {
            offset: 32,
            size: 8,
            name: "RSP".to_string(),
            is_stack_pointer: true,
        });

        facts.vnode_facts.insert(
            rsp.clone(),
            VnodeData {
                size: Some(8),
                space: Some("register".into()),
                address: Some(PcodeAddress(32)),
                constant_offset: None,
            },
        );

        let results = compute_constant_propagation(&facts);
        let stack_top = PcodeVarnode::from("__stack_top");

        assert_eq!(
            results.get(&rsp),
            Some(&SymbolicProp::Value(Some(stack_top), 0))
        );
    }

    #[test]
    fn test_ptradd_ptrsub_constants() {
        let mut facts = PcodeFacts::default();
        let base = PcodeVarnode::from("base");
        let index = PcodeVarnode::from("index");
        let size_vn = PcodeVarnode::from("size");
        let out_ptradd = PcodeVarnode::from("out_ptradd");
        let out_ptrsub = PcodeVarnode::from("out_ptrsub");
        let offset = PcodeVarnode::from("offset");

        // base = 1000
        facts.vnode_facts.insert(
            base.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(1000)),
                constant_offset: None,
            },
        );

        // index = 2
        facts.vnode_facts.insert(
            index.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(2)),
                constant_offset: None,
            },
        );

        // size = 4
        facts.vnode_facts.insert(
            size_vn.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(4)),
                constant_offset: None,
            },
        );

        // offset = 50
        facts.vnode_facts.insert(
            offset.clone(),
            VnodeData {
                size: Some(8),
                space: Some("const".into()),
                address: Some(PcodeAddress(50)),
                constant_offset: None,
            },
        );

        // out_ptradd = PTRADD(base, index, size) -> 1000 + 2 * 4 = 1008
        facts.pcode_facts.insert(
            PcodeInstruction::from("i1"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("PTRADD"),
                inputs: smallvec::smallvec![base.clone(), index.clone(), size_vn.clone()],
                outputs: smallvec::smallvec![out_ptradd.clone()],
                bb_id: None,
                target: None,
            },
        );

        // out_ptrsub = PTRSUB(base, offset) -> 1000 + 50 = 1050
        facts.pcode_facts.insert(
            PcodeInstruction::from("i2"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("PTRSUB"),
                inputs: smallvec::smallvec![base.clone(), offset.clone()],
                outputs: smallvec::smallvec![out_ptrsub.clone()],
                bb_id: None,
                target: None,
            },
        );

        let results = compute_constant_propagation(&facts);

        assert_eq!(
            results.get(&out_ptradd),
            Some(&SymbolicProp::Value(None, 1008))
        );
        assert_eq!(
            results.get(&out_ptrsub),
            Some(&SymbolicProp::Value(None, 1050))
        );
    }
}
