/*! Pcode constant propagation.

# Constant Propagation

The constant propagation uses a simple domain where each varnode `x` maps to `option(y) + c`
meaning that "varnode `x` is equal to `y` plus the constant `c`. If `option(y)` is none it means
that `x = c`. As a special case, the initial value for each varnode is `x = x + 0`. 

Constant propagation is implemented with relatively simple datalog rules and through lattice-aware
arithmetic operations on [`SymbolicProp`].

# Assumptions

- The stack pointer register is presumed to be top of stack. I think this will cause issues if it
  gets modified in a "non-standard" way in the function.
*/
use crate::{PcodeFacts, PcodeVarnode};
use ascent::ascent;
use ascent::lattice::Lattice;
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

ascent! {
    pub struct ConstantPropagationProgram;

    // Inputs
    relation copy_op(PcodeVarnode, PcodeVarnode);
    relation add_op(PcodeVarnode, PcodeVarnode, PcodeVarnode);
    relation sub_op(PcodeVarnode, PcodeVarnode, PcodeVarnode);
    relation ptradd_op(PcodeVarnode, PcodeVarnode, PcodeVarnode, i64); // out, base, index, size
    relation ptrsub_op(PcodeVarnode, PcodeVarnode, PcodeVarnode); // out, base, offset

    relation initial_value(PcodeVarnode, SymbolicProp);

    // Lattice relation for propagation
    lattice variable_prop(PcodeVarnode, SymbolicProp);

    // Initial values (constants or self-references)
    variable_prop(v, val.clone()) <-- initial_value(v, val);

    // Rules
    variable_prop(out, val.clone()) <--
        copy_op(out, inp),
        variable_prop(inp, val);

    // INT_ADD
    variable_prop(out, val1.add(val2)) <--
        add_op(out, in1, in2),
        variable_prop(in1, val1),
        variable_prop(in2, val2);

    // INT_SUB
    variable_prop(out, val1.sub(val2)) <--
        sub_op(out, in1, in2),
        variable_prop(in1, val1),
        variable_prop(in2, val2);

    // PTRADD: out = base + index * size
    variable_prop(out, val_base.ptradd(val_idx, *sz)) <--
        ptradd_op(out, base, index, sz),
        variable_prop(base, val_base),
        variable_prop(index, val_idx);

    // PTRSUB: out = base + offset (ptrsub is logically an add)
    variable_prop(out, val_base.add(val_off)) <--
        ptrsub_op(out, base, offset),
        variable_prop(base, val_base),
        variable_prop(offset, val_off);
}

pub fn compute_constant_propagation(facts: &PcodeFacts) -> BTreeMap<PcodeVarnode, SymbolicProp> {
    let mut prog = ConstantPropagationProgram::default();

    let mut defined_vars = BTreeSet::new();
    for pcode in facts.pcode_facts.values() {
        for out in &pcode.outputs {
            defined_vars.insert(out.clone());
        }
    }

    let stack_top_vn = PcodeVarnode::from("__stack_top");
    prog.initial_value.push((
        stack_top_vn.clone(),
        SymbolicProp::Value(Some(stack_top_vn.clone()), 0),
    ));

    let sp_offsets: BTreeSet<i64> = facts
        .register_facts
        .iter()
        .filter(|r| r.is_stack_pointer)
        .map(|r| r.offset)
        .collect();

    // Fill initial values and operations
    for (vnode, data) in &facts.vnode_facts {
        let is_const_space = data.space.as_deref() == Some("const");
        let is_stack_space = data.space.as_deref() == Some("stack");
        let is_register_space = data.space.as_deref() == Some("register");

        if is_const_space {
            if let Some(offset) = data.constant_offset {
                prog.initial_value
                    .push((vnode.clone(), SymbolicProp::Value(None, offset)));
            } else if let Some(addr) = &data.address {
                prog.initial_value
                    .push((vnode.clone(), SymbolicProp::Value(None, addr.0)));
            }
        } else if is_stack_space {
            if let Some(offset) = data.constant_offset {
                prog.initial_value.push((
                    vnode.clone(),
                    SymbolicProp::Value(Some(stack_top_vn.clone()), offset),
                ));
            } else if let Some(addr) = &data.address {
                prog.initial_value.push((
                    vnode.clone(),
                    SymbolicProp::Value(Some(stack_top_vn.clone()), addr.0),
                ));
            }
        } else if !defined_vars.contains(vnode) {
            let mut is_sp = false;
            if is_register_space
                && let Some(addr) = &data.address
                && sp_offsets.contains(&addr.0)
            {
                is_sp = true;
            }

            if is_sp {
                prog.initial_value.push((
                    vnode.clone(),
                    SymbolicProp::Value(Some(stack_top_vn.clone()), 0),
                ));
            } else {
                // Variables that are NOT defined in this function (parameters, globals, etc.)
                // start as "self + 0"
                prog.initial_value
                    .push((vnode.clone(), SymbolicProp::Value(Some(vnode.clone()), 0)));
            }
        }
    }

    for pcode in facts.pcode_facts.values() {
        if pcode.outputs.is_empty() {
            continue;
        }
        let out = pcode.outputs[0].clone();

        let mnemonic = pcode.mnemonic.deref().deref();
        match mnemonic {
            "COPY" | "MULTIEQUAL" | "INDIRECT" | "CAST" | "TRUNC" | "INT_SEXT" | "INT_ZEXT" => {
                for input in &pcode.inputs {
                    prog.copy_op.push((out.clone(), input.clone()));
                }
            }
            "INT_ADD" if pcode.inputs.len() >= 2 => {
                prog.add_op
                    .push((out, pcode.inputs[0].clone(), pcode.inputs[1].clone()));
            }
            "INT_SUB" if pcode.inputs.len() >= 2 => {
                prog.sub_op
                    .push((out, pcode.inputs[0].clone(), pcode.inputs[1].clone()));
            }
            "PTRADD" if pcode.inputs.len() >= 3 => {
                let base = pcode.inputs[0].clone();
                let index = pcode.inputs[1].clone();
                let size_vn = &pcode.inputs[2];
                if let Some(addr) = facts
                    .vnode_facts
                    .get(size_vn)
                    .and_then(|data| data.address.as_ref())
                {
                    prog.ptradd_op.push((out, base, index, addr.0));
                }
            }
            "PTRSUB" if pcode.inputs.len() >= 2 => {
                prog.ptrsub_op
                    .push((out, pcode.inputs[0].clone(), pcode.inputs[1].clone()));
            }
            _ => {}
        }
    }

    prog.run();

    prog.variable_prop.into_iter().collect()
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
                name: "v1".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            c5.clone(),
            VnodeData {
                name: "5".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("const".to_string()),
                address: Some(PcodeAddress(5)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v2.clone(),
            VnodeData {
                name: "v2".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v3.clone(),
            VnodeData {
                name: "v3".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
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
                opcode: None,
                inputs: smallvec::smallvec![v1.clone(), c5.clone()],
                outputs: smallvec::smallvec![v2.clone()],
                bb_id: None,
                index: 0,
                target: None,
            },
        );

        // v3 = v2 + 5
        let inst2 = PcodeInstruction::from("inst2");
        facts.pcode_facts.insert(
            inst2.clone(),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                opcode: None,
                inputs: smallvec::smallvec![v2.clone(), c5.clone()],
                outputs: smallvec::smallvec![v3.clone()],
                bb_id: None,
                index: 1,
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
                name: "v1".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
                address: None,
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            k10.clone(),
            VnodeData {
                name: "known10".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("const".to_string()),
                address: None,
                constant_offset: Some(10),
            },
        );

        facts.vnode_facts.insert(
            v2.clone(),
            VnodeData {
                name: "v2".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
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
                opcode: None,
                inputs: smallvec::smallvec![v1.clone(), k10.clone()],
                outputs: smallvec::smallvec![v2.clone()],
                bb_id: None,
                index: 0,
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
        for (vn, name, space, offset) in [
            (&t, "t", "register", None),
            (&t1, "t1", "register", None),
            (&t2, "t2", "register", None),
            (&c2, "2", "const", Some(2)),
            (&c4, "4", "const", Some(4)),
        ] {
            facts.vnode_facts.insert(
                vn.clone(),
                VnodeData {
                    name: name.to_string(),
                    size: Some(8),
                    is_address: false,
                    space: Some(space.to_string()),
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
                opcode: None,
                inputs: smallvec::smallvec![t.clone(), c2.clone()],
                outputs: smallvec::smallvec![t1.clone()],
                bb_id: None,
                index: 0,
                target: None,
            },
        );

        // t2 = t1 + 4
        facts.pcode_facts.insert(
            PcodeInstruction::from("i2"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                opcode: None,
                inputs: smallvec::smallvec![t1.clone(), c4.clone()],
                outputs: smallvec::smallvec![t2.clone()],
                bb_id: None,
                index: 1,
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
                name: "stack_vn".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("stack".to_string()),
                address: Some(PcodeAddress(-16)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            c8.clone(),
            VnodeData {
                name: "8".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("const".to_string()),
                address: Some(PcodeAddress(8)),
                constant_offset: None,
            },
        );

        facts.vnode_facts.insert(
            v1.clone(),
            VnodeData {
                name: "v1".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
                address: None,
                constant_offset: None,
            },
        );

        // v1 = stack_vn + 8
        facts.pcode_facts.insert(
            PcodeInstruction::from("i1"),
            PcodeData {
                mnemonic: PcodeMnemonic::from("INT_ADD"),
                opcode: None,
                inputs: smallvec::smallvec![stack_vn.clone(), c8.clone()],
                outputs: smallvec::smallvec![v1.clone()],
                bb_id: None,
                index: 0,
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
                name: "RSP".to_string(),
                size: Some(8),
                is_address: false,
                space: Some("register".to_string()),
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
}
