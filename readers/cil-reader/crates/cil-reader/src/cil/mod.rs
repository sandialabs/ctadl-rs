//! CIL bytecode: opcode table, instruction decoding, method bodies.

pub mod decoder;
pub mod opcodes;

pub use decoder::{Ins, InstructionDecoder, RawOperand, SwitchTargets};
pub use opcodes::{Code, FlowControl, OpCode, OpCodeType, OperandType, StackBehaviour};
