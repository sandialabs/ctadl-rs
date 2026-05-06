// This crate is vendored, and we do not maintain it in this repo, so we don't want to alert when
// running clippy.
#![allow(clippy::all)]
pub mod error;
pub mod flow;
pub mod instructions;
pub mod jar;
pub mod linemap;
pub mod parse_utils;
pub mod parser;
pub mod types;

pub use error::{ClassFileError, ClassFileResult};
pub use flow::{
    compute_basic_blocks_for_method, normalize_stack_slots_for_method, BasicBlock, CallInfo,
    CallKind, ConstantValue, DataflowInfo, FieldRef, InstructionFlowInfo, InstructionKind,
    Location, MethodBasicBlocks, MethodTarget,
};
pub use instructions::{disassemble_class_file, disassemble_jar_file};
pub use jar::JarFileParser;
pub use linemap::{collect_line_map_entries, write_line_map_json, LineMapEntry};
pub use parser::ClassFileParser;
pub use types::{ClassFile, CodeAttribute, FieldInfo, MethodInfo};
