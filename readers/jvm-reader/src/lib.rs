pub mod error;
pub mod flow;
pub mod instructions;
pub mod jar;
pub mod linemap;
pub mod parse_utils;
pub mod parser;
pub mod types;

pub use error::{ClassFileError, ClassFileResult, MethodContext};
pub use flow::{
    BasicBlock, CallInfo, CallKind, ConstantValue, DataflowInfo, FieldRef, InstructionFlowInfo,
    InstructionKind, Location, MethodBasicBlocks, MethodParameterInfo, MethodParameterKind,
    MethodTarget, ParameterSlotMap, compute_basic_blocks_for_method, descriptor_parameter_info,
    descriptor_returns_value, instruction_stack_effect, normalize_stack_slots_for_method,
};
pub use instructions::{disassemble_class_file, disassemble_jar_file};
pub use jar::JarFileParser;
pub use linemap::{LineMapEntry, collect_line_map_entries, write_line_map_json};
pub use parser::ClassFileParser;
pub use types::{ClassFile, CodeAttribute, FieldInfo, JvmString, MethodInfo};
