// JVM bytecode decoding and javap-style disassembly (JVMS Chapter 6).

use std::path::Path;

use crate::error::ClassFileError;
use crate::jar::JarFileParser;
use crate::parse_utils::{read_i32_be, read_u16_be, read_u8};
use crate::parser::ClassFileParser;
use crate::types::{ClassFile, CodeAttribute, CpEntry, FieldInfo, MethodInfo};

/// Decode one instruction at `pc` in `code`. Returns (pc, mnemonic, operands_line, next_pc).
/// operands_line includes "#n" and optional "// comment" for javap style.
fn decode_instruction(
    code: &[u8],
    pc: usize,
    cf: &ClassFile,
) -> Result<(usize, &'static str, String, usize), ClassFileError> {
    if pc >= code.len() {
        return Err(ClassFileError::InvalidClassFile("code past end"));
    }
    let opcode = code[pc];
    let (mnemonic, operands_len, fmt_operands): (&'static str, usize, Option<u16>) = match opcode {
        0x00 => ("nop", 0, None),
        0x01 => ("aconst_null", 0, None),
        0x02 => ("iconst_m1", 0, None),
        0x03 => ("iconst_0", 0, None),
        0x04 => ("iconst_1", 0, None),
        0x05 => ("iconst_2", 0, None),
        0x06 => ("iconst_3", 0, None),
        0x07 => ("iconst_4", 0, None),
        0x08 => ("iconst_5", 0, None),
        0x09 => ("lconst_0", 0, None),
        0x0a => ("lconst_1", 0, None),
        0x0b => ("fconst_0", 0, None),
        0x0c => ("fconst_1", 0, None),
        0x0d => ("fconst_2", 0, None),
        0x0e => ("dconst_0", 0, None),
        0x0f => ("dconst_1", 0, None),
        0x10 => ("bipush", 1, None),
        0x11 => ("sipush", 2, None),
        0x12 => ("ldc", 1, Some(1)),
        0x13 => ("ldc_w", 2, Some(2)),
        0x14 => ("ldc2_w", 2, Some(2)),
        0x15 => ("iload", 1, None),
        0x16 => ("lload", 1, None),
        0x17 => ("fload", 1, None),
        0x18 => ("dload", 1, None),
        0x19 => ("aload", 1, None),
        0x1a => ("iload_0", 0, None),
        0x1b => ("iload_1", 0, None),
        0x1c => ("iload_2", 0, None),
        0x1d => ("iload_3", 0, None),
        0x1e => ("lload_0", 0, None),
        0x1f => ("lload_1", 0, None),
        0x20 => ("lload_2", 0, None),
        0x21 => ("lload_3", 0, None),
        0x22 => ("fload_0", 0, None),
        0x23 => ("fload_1", 0, None),
        0x24 => ("fload_2", 0, None),
        0x25 => ("fload_3", 0, None),
        0x26 => ("dload_0", 0, None),
        0x27 => ("dload_1", 0, None),
        0x28 => ("dload_2", 0, None),
        0x29 => ("dload_3", 0, None),
        0x2a => ("aload_0", 0, None),
        0x2b => ("aload_1", 0, None),
        0x2c => ("aload_2", 0, None),
        0x2d => ("aload_3", 0, None),
        0x2e => ("iaload", 0, None),
        0x2f => ("laload", 0, None),
        0x30 => ("faload", 0, None),
        0x31 => ("daload", 0, None),
        0x32 => ("aaload", 0, None),
        0x33 => ("baload", 0, None),
        0x34 => ("caload", 0, None),
        0x35 => ("saload", 0, None),
        0x36 => ("istore", 1, None),
        0x37 => ("lstore", 1, None),
        0x38 => ("fstore", 1, None),
        0x39 => ("dstore", 1, None),
        0x3a => ("astore", 1, None),
        0x3b => ("istore_0", 0, None),
        0x3c => ("istore_1", 0, None),
        0x3d => ("istore_2", 0, None),
        0x3e => ("istore_3", 0, None),
        0x3f => ("lstore_0", 0, None),
        0x40 => ("lstore_1", 0, None),
        0x41 => ("lstore_2", 0, None),
        0x42 => ("lstore_3", 0, None),
        0x43 => ("fstore_0", 0, None),
        0x44 => ("fstore_1", 0, None),
        0x45 => ("fstore_2", 0, None),
        0x46 => ("fstore_3", 0, None),
        0x47 => ("dstore_0", 0, None),
        0x48 => ("dstore_1", 0, None),
        0x49 => ("dstore_2", 0, None),
        0x4a => ("dstore_3", 0, None),
        0x4b => ("astore_0", 0, None),
        0x4c => ("astore_1", 0, None),
        0x4d => ("astore_2", 0, None),
        0x4e => ("astore_3", 0, None),
        0x4f => ("iastore", 0, None),
        0x50 => ("pop2", 0, None),
        0x51 => ("fastore", 0, None),
        0x52 => ("dastore", 0, None),
        0x53 => ("aastore", 0, None),
        0x54 => ("bastore", 0, None),
        0x55 => ("castore", 0, None),
        0x56 => ("dup2_x2", 0, None),
        0x57 => ("pop", 0, None),
        0x58 => ("pop2", 0, None),
        0x59 => ("dup", 0, None),
        0x5a => ("dup_x1", 0, None),
        0x5b => ("dup_x2", 0, None),
        0x5c => ("dup2", 0, None),
        0x5d => ("dup2_x1", 0, None),
        0x5e => ("dup2_x2", 0, None),
        0x5f => ("swap", 0, None),
        0x60 => ("iadd", 0, None),
        0x61 => ("ladd", 0, None),
        0x62 => ("fadd", 0, None),
        0x63 => ("dadd", 0, None),
        0x64 => ("isub", 0, None),
        0x65 => ("lsub", 0, None),
        0x66 => ("fsub", 0, None),
        0x67 => ("dsub", 0, None),
        0x68 => ("imul", 0, None),
        0x69 => ("lmul", 0, None),
        0x6a => ("fmul", 0, None),
        0x6b => ("dmul", 0, None),
        0x6c => ("idiv", 0, None),
        0x6d => ("ldiv", 0, None),
        0x6e => ("fdiv", 0, None),
        0x6f => ("ddiv", 0, None),
        0x70 => ("irem", 0, None),
        0x71 => ("lrem", 0, None),
        0x72 => ("frem", 0, None),
        0x73 => ("drem", 0, None),
        0x74 => ("ineg", 0, None),
        0x75 => ("lneg", 0, None),
        0x76 => ("fneg", 0, None),
        0x77 => ("dneg", 0, None),
        0x78 => ("ishl", 0, None),
        0x79 => ("lshl", 0, None),
        0x7a => ("ishr", 0, None),
        0x7b => ("lshr", 0, None),
        0x7c => ("iushr", 0, None),
        0x7d => ("lushr", 0, None),
        0x7e => ("iand", 0, None),
        0x7f => ("land", 0, None),
        0x80 => ("ior", 0, None),
        0x81 => ("lor", 0, None),
        0x82 => ("ixor", 0, None),
        0x83 => ("lxor", 0, None),
        0x84 => ("iinc", 2, None),
        0x85 => ("i2l", 0, None),
        0x86 => ("i2f", 0, None),
        0x87 => ("i2d", 0, None),
        0x88 => ("l2i", 0, None),
        0x89 => ("l2f", 0, None),
        0x8a => ("l2d", 0, None),
        0x8b => ("f2i", 0, None),
        0x8c => ("f2l", 0, None),
        0x8d => ("f2d", 0, None),
        0x8e => ("d2i", 0, None),
        0x8f => ("d2l", 0, None),
        0x90 => ("d2f", 0, None),
        0x91 => ("i2b", 0, None),
        0x92 => ("i2c", 0, None),
        0x93 => ("i2s", 0, None),
        0x94 => ("lcmp", 0, None),
        0x95 => ("fcmpl", 0, None),
        0x96 => ("fcmpg", 0, None),
        0x97 => ("dcmpl", 0, None),
        0x98 => ("dcmpg", 0, None),
        0x99 => ("ifeq", 2, None),
        0x9a => ("ifne", 2, None),
        0x9b => ("iflt", 2, None),
        0x9c => ("ifge", 2, None),
        0x9d => ("ifgt", 2, None),
        0x9e => ("ifle", 2, None),
        0x9f => ("if_icmpeq", 2, None),
        0xa0 => ("if_icmpne", 2, None),
        0xa1 => ("if_icmplt", 2, None),
        0xa2 => ("if_icmpge", 2, None),
        0xa3 => ("if_icmpgt", 2, None),
        0xa4 => ("if_icmple", 2, None),
        0xa5 => ("if_acmpeq", 2, None),
        0xa6 => ("if_acmpne", 2, None),
        0xa7 => ("goto", 2, None),
        0xa8 => ("jsr", 2, None),
        0xa9 => ("ret", 1, None),
        0xaa => ("tableswitch", 0, None),  // variable
        0xab => ("lookupswitch", 0, None), // variable
        0xac => ("ireturn", 0, None),
        0xad => ("lreturn", 0, None),
        0xae => ("freturn", 0, None),
        0xaf => ("dreturn", 0, None),
        0xb0 => ("areturn", 0, None),
        0xb1 => ("return", 0, None),
        0xb2 => ("getstatic", 2, Some(2)),
        0xb3 => ("putstatic", 2, Some(2)),
        0xb4 => ("getfield", 2, Some(2)),
        0xb5 => ("putfield", 2, Some(2)),
        0xb6 => ("invokevirtual", 2, Some(2)),
        0xb7 => ("invokespecial", 2, Some(2)),
        0xb8 => ("invokestatic", 2, Some(2)),
        0xb9 => ("invokeinterface", 4, Some(2)), // index byte1 byte2, count, 0
        0xba => ("invokedynamic", 4, Some(2)),   // index byte1 byte2, 0, 0
        0xbb => ("new", 2, Some(2)),
        0xbc => ("newarray", 1, None),
        0xbd => ("anewarray", 2, Some(2)),
        0xbe => ("arraylength", 0, None),
        0xbf => ("athrow", 0, None),
        0xc0 => ("checkcast", 2, Some(2)),
        0xc1 => ("instanceof", 2, Some(2)),
        0xc2 => ("monitorenter", 0, None),
        0xc3 => ("monitorexit", 0, None),
        0xc4 => ("wide", 0, None), // variable: next byte is opcode
        0xc5 => ("multianewarray", 3, Some(2)),
        0xc6 => ("ifnull", 2, None),
        0xc7 => ("ifnonnull", 2, None),
        0xc8 => ("goto_w", 4, None),
        0xc9 => ("jsr_w", 4, None),
        _ => ("<unknown>", 0, None),
    };

    let mut next_pc = pc + 1 + operands_len;

    // Handle variable-length: wide, tableswitch, lookupswitch
    let (mnemonic, next_pc, operands_line) = if opcode == 0xc4 {
        // wide: next byte is opcode, then 0-2 bytes
        if pc + 2 > code.len() {
            return Err(ClassFileError::InvalidClassFile("wide truncated"));
        }
        let subop = code[pc + 1];
        let (sub_len, sub_fmt) = wide_operand_len(subop);
        next_pc = pc + 2 + sub_len;
        let mut line = String::new();
        if sub_fmt {
            let idx = read_u16_be(code, pc + 2)
                .map_err(|_| ClassFileError::InvalidClassFile("wide iload etc"))?;
            line.push_str(&format!("{} {}", wide_mnemonic(subop), idx));
            if subop == 0x84 {
                let const_val = code.get(pc + 4).copied().unwrap_or(0) as i8;
                line.push_str(&format!(" {}", const_val));
                next_pc = pc + 6;
            }
        }
        ("wide", next_pc, line)
    } else if opcode == 0xaa {
        // tableswitch: align to 4 bytes; default and offsets are relative to opcode pc
        let align = (pc + 1 + 3) & !3;
        if align + 12 > code.len() {
            return Err(ClassFileError::InvalidClassFile("tableswitch truncated"));
        }
        let default_offset = read_i32_be(code, align)
            .map_err(|_| ClassFileError::InvalidClassFile("tableswitch"))?;
        let low = read_i32_be(code, align + 4)
            .map_err(|_| ClassFileError::InvalidClassFile("tableswitch"))?;
        let high = read_i32_be(code, align + 8)
            .map_err(|_| ClassFileError::InvalidClassFile("tableswitch"))?;
        let n = (high - low + 1) as usize;
        next_pc = align + 12 + n * 4;
        let default_target = pc as i32 + default_offset;
        let mut body = format!(" {{ // {} to {}", low, high);
        for (i, key) in (low..=high).enumerate() {
            let off = read_i32_be(code, align + 12 + i * 4)
                .map_err(|_| ClassFileError::InvalidClassFile("tableswitch"))?;
            let target = pc as i32 + off;
            body.push_str(&format!("\n       {}: {}", key, target));
        }
        body.push_str(&format!("\n       default: {}", default_target));
        body.push_str("\n}");
        ("tableswitch", next_pc, body)
    } else if opcode == 0xab {
        let align = (pc + 1 + 3) & !3;
        if align + 8 > code.len() {
            return Err(ClassFileError::InvalidClassFile("lookupswitch truncated"));
        }
        let default_offset = read_i32_be(code, align)
            .map_err(|_| ClassFileError::InvalidClassFile("lookupswitch"))?;
        let npairs = read_i32_be(code, align + 4)
            .map_err(|_| ClassFileError::InvalidClassFile("lookupswitch"))?
            as usize;
        next_pc = align + 8 + npairs * 8;
        let default_target = pc as i32 + default_offset;
        let mut body = format!(" {{ // {}", npairs);
        for i in 0..npairs {
            let match_val = read_i32_be(code, align + 8 + i * 8)
                .map_err(|_| ClassFileError::InvalidClassFile("lookupswitch"))?;
            let offset = read_i32_be(code, align + 12 + i * 8)
                .map_err(|_| ClassFileError::InvalidClassFile("lookupswitch"))?;
            let target = pc as i32 + offset;
            body.push_str(&format!("\n       {}: {}", match_val, target));
        }
        body.push_str(&format!("\n       default: {}", default_target));
        body.push_str("\n}");
        ("lookupswitch", next_pc, body)
    } else {
        let mut line = String::new();
        if operands_len > 0 && pc + 1 + operands_len <= code.len() {
            if operands_len == 1 {
                let b = read_u8(code, pc + 1)
                    .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                if fmt_operands == Some(1) {
                    let idx = b as u16;
                    line.push_str(&format!("#{} ", idx));
                    if let Ok(comment) = format_cp_comment(cf, idx) {
                        line.push_str(&format!("// {}", comment));
                    }
                } else if opcode == 0xbc {
                    let atype = match b {
                        4 => "boolean",
                        5 => "char",
                        6 => "float",
                        7 => "double",
                        8 => "byte",
                        9 => "short",
                        10 => "int",
                        11 => "long",
                        _ => "?",
                    };
                    line.push_str(atype);
                } else {
                    line.push_str(&format!("{}", b as i8));
                }
            } else if operands_len == 2 {
                if opcode == 0x7c {
                    // iinc: index byte, const byte
                    let index = read_u8(code, pc + 1)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    let const_val = code[pc + 2] as i8;
                    line.push_str(&format!("{} {}", index, const_val));
                } else if fmt_operands == Some(2) {
                    let idx = read_u16_be(code, pc + 1)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    line.push_str(&format!("#{} ", idx));
                    if let Ok(comment) = format_cp_comment(cf, idx) {
                        line.push_str(&format!("// {}", comment));
                    }
                } else {
                    let offset = i16::from_be_bytes([code[pc + 1], code[pc + 2]]) as i32;
                    line.push_str(&format!("{}", pc as i32 + offset));
                }
            } else if operands_len == 3 {
                let idx = read_u16_be(code, pc + 1)
                    .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                line.push_str(&format!("#{} ", idx));
                if let Ok(comment) = format_cp_comment(cf, idx) {
                    line.push_str(&format!("// {}", comment));
                }
            } else if operands_len == 4 {
                if opcode == 0xb9 {
                    let idx = read_u16_be(code, pc + 1)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    let count = read_u8(code, pc + 3)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    line.push_str(&format!("#{}, {} ", idx, count));
                    if let Ok(comment) = format_cp_comment(cf, idx) {
                        line.push_str(&format!("// {}", comment));
                    }
                } else if opcode == 0xba {
                    let idx = read_u16_be(code, pc + 1)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    let zero = read_u8(code, pc + 3)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    line.push_str(&format!("#{}, {} ", idx, zero));
                    if let Ok(comment) = format_cp_comment(cf, idx) {
                        line.push_str(&format!("// {}", comment));
                    }
                } else {
                    let offset = read_i32_be(code, pc + 1)
                        .map_err(|_| ClassFileError::InvalidClassFile("operand"))?;
                    line.push_str(&format!("{}", pc as i32 + offset));
                }
            }
        }
        (mnemonic, next_pc, line.trim_end().to_string())
    };

    Ok((pc, mnemonic, operands_line, next_pc))
}

fn wide_operand_len(subop: u8) -> (usize, bool) {
    match subop {
        0x15 | 0x16 | 0x17 | 0x18 | 0x19 | 0x36 | 0x37 | 0x38 | 0x39 | 0x3a | 0xa9 => (2, true),
        0x84 => (4, true),
        _ => (0, false),
    }
}

fn wide_mnemonic(subop: u8) -> &'static str {
    match subop {
        0x15 => "iload",
        0x16 => "lload",
        0x17 => "fload",
        0x18 => "dload",
        0x19 => "aload",
        0x36 => "istore",
        0x37 => "lstore",
        0x38 => "fstore",
        0x39 => "dstore",
        0x3a => "astore",
        0x84 => "iinc",
        0xa9 => "ret",
        _ => "?",
    }
}

fn format_cp_comment(cf: &ClassFile, index: u16) -> Result<String, ClassFileError> {
    let entry = cf.get_cp(index)?;
    Ok(match entry {
        CpEntry::Class { .. } => format!("class {}", cf.get_class_name(index)?),
        CpEntry::String { .. } => {
            let s = cf.get_utf8(match entry {
                CpEntry::String { string_index } => *string_index,
                _ => unreachable!(),
            })?;
            let escaped = s
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("String {}", escaped)
        }
        CpEntry::Fieldref {
            class_index,
            name_and_type_index,
        } => {
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            let field_str = match cf.this_class_name() {
                Ok(this) if this == cf.get_class_name(*class_index).unwrap_or("") => {
                    format!("Field {}:{}", name, descriptor)
                }
                _ => format!("Field {}", cf.get_field_ref(index)?),
            };
            field_str
        }
        CpEntry::Methodref {
            class_index,
            name_and_type_index,
        } => {
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            let method_str = match cf.this_class_name() {
                Ok(this) if this == cf.get_class_name(*class_index).unwrap_or("") => {
                    let name_part = if name == "<init>" || name == "<clinit>" {
                        format!("\"{}\"", name)
                    } else {
                        name.to_string()
                    };
                    format!("Method {}:{}", name_part, descriptor)
                }
                _ => format!("Method {}", cf.get_method_ref(index)?),
            };
            method_str
        }
        CpEntry::InterfaceMethodref {
            class_index,
            name_and_type_index,
        } => {
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            let method_str = match cf.this_class_name() {
                Ok(this) if this == cf.get_class_name(*class_index).unwrap_or("") => {
                    let name_part = if name == "<init>" || name == "<clinit>" {
                        format!("\"{}\"", name)
                    } else {
                        name.to_string()
                    };
                    format!("InterfaceMethod {}:{}", name_part, descriptor)
                }
                _ => format!("InterfaceMethod {}", cf.get_method_ref(index)?),
            };
            method_str
        }
        CpEntry::Integer(i) => format!("int {}", i),
        CpEntry::Float(_) => "float".to_string(),
        CpEntry::Long(l) => format!("long {}l", l),
        CpEntry::Double(d) => format!("double {}d", f64::from_bits(*d)),
        CpEntry::NameAndType { .. } => {
            let (n, d) = cf.get_name_and_type(index)?;
            format!("{}:{}", n, d)
        }
        CpEntry::MethodType { .. } => {
            let desc = cf.get_utf8(match entry {
                CpEntry::MethodType { descriptor_index } => *descriptor_index,
                _ => unreachable!(),
            })?;
            format!("MethodType {}", desc)
        }
        CpEntry::InvokeDynamic {
            bootstrap_method_attr_index,
            name_and_type_index,
        } => {
            let (name, descriptor) = cf.get_name_and_type(*name_and_type_index)?;
            format!(
                "InvokeDynamic #{}:{}:{}",
                bootstrap_method_attr_index, name, descriptor
            )
        }
        _ => "?".to_string(),
    })
}

/// Parse a single field type from the start of descriptor; returns (Java type name, byte length consumed).
fn parse_field_type_to_java(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    match b[0] {
        b'B' => Some(("byte".to_string(), 1)),
        b'C' => Some(("char".to_string(), 1)),
        b'D' => Some(("double".to_string(), 1)),
        b'F' => Some(("float".to_string(), 1)),
        b'I' => Some(("int".to_string(), 1)),
        b'J' => Some(("long".to_string(), 1)),
        b'S' => Some(("short".to_string(), 1)),
        b'Z' => Some(("boolean".to_string(), 1)),
        b'V' => Some(("void".to_string(), 1)),
        b'L' => {
            let end = s[1..].find(';')? + 1;
            let internal = &s[1..end];
            Some((internal.replace('/', "."), end + 1))
        }
        b'[' => {
            let (elem, n) = parse_field_type_to_java(&s[1..])?;
            Some((format!("{}[]", elem), 1 + n))
        }
        _ => None,
    }
}

/// Parse method descriptor (Params)Return to a list of Java parameter type names.
fn descriptor_param_list_to_java(descriptor: &str) -> Option<Vec<String>> {
    let s = descriptor.strip_prefix('(')?;
    let mut types = Vec::new();
    let mut i = 0;
    while i < s.len() && s.as_bytes()[i] != b')' {
        let (ty, n) = parse_field_type_to_java(&s[i..])?;
        types.push(ty);
        i += n;
    }
    Some(types)
}

/// Return type from method descriptor (e.g. "(I)V" -> "void", "()Ljava/lang/String;" -> "java.lang.String").
fn descriptor_return_to_java(descriptor: &str) -> Option<String> {
    let rest = descriptor.strip_prefix('(')?;
    let close = rest.find(')')?;
    let return_part = &rest[close + 1..];
    if return_part.is_empty() {
        return None;
    }
    parse_field_type_to_java(return_part).map(|(ty, _)| ty)
}

fn format_method_signature(cf: &ClassFile, m: &MethodInfo) -> Result<String, ClassFileError> {
    let name = cf.get_utf8(m.name_index)?;
    let descriptor = cf.get_utf8(m.descriptor_index)?;
    let mut mods = Vec::<&str>::new();
    let flags = m.access_flags;
    if flags & 0x0001 != 0 {
        mods.push("public");
    }
    if flags & 0x0002 != 0 {
        mods.push("private");
    }
    if flags & 0x0004 != 0 {
        mods.push("protected");
    }
    if flags & 0x0008 != 0 {
        mods.push("static");
    }
    if flags & 0x0010 != 0 {
        mods.push("final");
    }
    if flags & 0x0020 != 0 {
        mods.push("synchronized");
    }
    if flags & 0x0080 != 0 {
        mods.push("varargs");
    }
    if flags & 0x0100 != 0 {
        mods.push("native");
    }
    if flags & 0x0400 != 0 {
        mods.push("abstract");
    }
    if flags & 0x1000 != 0 {
        mods.push("synthetic");
    }
    let mod_str = mods.join(" ");
    let display_name = if name == "<init>" {
        cf.this_class_name()?.replace('/', ".")
    } else {
        name.to_string()
    };
    let params_str = descriptor_param_list_to_java(descriptor)
        .map(|ps| ps.join(", "))
        .unwrap_or_else(|| descriptor.to_string());
    let return_type_str = descriptor_return_to_java(descriptor).unwrap_or_else(|| "?".to_string());
    Ok(format!(
        "{} {}({});",
        mod_str,
        if name == "<init>" {
            display_name
        } else {
            format!("{} {}", return_type_str, display_name)
        },
        params_str
    ))
}

fn format_field_signature(cf: &ClassFile, f: &FieldInfo) -> Result<String, ClassFileError> {
    let name = cf.get_utf8(f.name_index)?;
    let descriptor = cf.get_utf8(f.descriptor_index)?;
    let mut mods = Vec::<&str>::new();
    let flags = f.access_flags;
    if flags & 0x0001 != 0 {
        mods.push("public");
    }
    if flags & 0x0002 != 0 {
        mods.push("private");
    }
    if flags & 0x0004 != 0 {
        mods.push("protected");
    }
    if flags & 0x0008 != 0 {
        mods.push("static");
    }
    if flags & 0x0010 != 0 {
        mods.push("final");
    }
    if flags & 0x0040 != 0 {
        mods.push("volatile");
    }
    if flags & 0x0080 != 0 {
        mods.push("transient");
    }
    if flags & 0x1000 != 0 {
        mods.push("synthetic");
    }
    if flags & 0x4000 != 0 {
        mods.push("enum");
    }
    let mod_str = mods.join(" ");
    let type_str = parse_field_type_to_java(descriptor)
        .map(|(ty, _)| ty)
        .unwrap_or_else(|| descriptor.to_string());
    Ok(format!("{} {} {};", mod_str, type_str, name))
}

fn format_class_header(cf: &ClassFile) -> Result<String, ClassFileError> {
    let mut out = String::new();
    if let Some(idx) = cf.source_file {
        let name = cf.get_utf8(idx)?;
        out.push_str(&format!("Compiled from \"{}\"\n", name));
    }
    let name = cf.this_class_name()?;
    let name_dotted = name.replace('/', ".");
    let flags = cf.access_flags;
    let is_interface = (flags & 0x0200) != 0;
    let mut mods = Vec::<&str>::new();
    if flags & 0x0001 != 0 {
        mods.push("public");
    }
    if flags & 0x0010 != 0 {
        mods.push("final");
    }
    if !is_interface {
        if flags & 0x0400 != 0 {
            mods.push("abstract");
        }
        if flags & 0x1000 != 0 {
            mods.push("synthetic");
        }
        if flags & 0x2000 != 0 {
            mods.push("annotation");
        }
        if flags & 0x4000 != 0 {
            mods.push("enum");
        }
    } else {
        if flags & 0x1000 != 0 {
            mods.push("synthetic");
        }
    }
    let mod_str = if mods.is_empty() {
        "".to_string()
    } else {
        mods.join(" ") + " "
    };
    let kind = if is_interface { "interface" } else { "class" };
    out.push_str(&format!("{}{} {}", mod_str, kind, name_dotted));
    if !cf.interfaces.is_empty() {
        let ifaces: Vec<String> = cf
            .interfaces
            .iter()
            .filter_map(|&idx| cf.get_class_name(idx).ok())
            .map(|s| s.replace('/', "."))
            .collect();
        let clause = if is_interface {
            " extends "
        } else {
            " implements "
        };
        out.push_str(&format!("{}{}", clause, ifaces.join(",")));
    }
    out.push_str(" {");
    Ok(out)
}

fn disassemble_method(
    cf: &ClassFile,
    m: &MethodInfo,
    code: &CodeAttribute,
) -> Result<String, ClassFileError> {
    let mut out = String::new();
    let sig = format_method_signature(cf, m)?;
    out.push_str(&format!("  {}\n", sig));
    out.push_str("    Code:\n");
    let mut pc = 0usize;
    let code_bytes = &code.code;
    while pc < code_bytes.len() {
        let (cur_pc, mnemonic, operands_line, next_pc) = decode_instruction(code_bytes, pc, cf)?;
        let line = if operands_line.is_empty() {
            format!("       {}: {}", cur_pc, mnemonic)
        } else {
            format!("       {}: {} {}", cur_pc, mnemonic, operands_line)
        };
        out.push_str(&line);
        out.push('\n');
        pc = next_pc;
    }
    Ok(out)
}

/// Disassemble a .class file to a javap -c style string.
pub fn disassemble_class_file(path: &Path) -> String {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };
    let parser = match ClassFileParser::parse(&data) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let cf = parser.class_file();
    let mut out = String::new();
    if let Ok(header) = format_class_header(cf) {
        out.push_str(&header);
        out.push('\n');
    }
    // javap default: show all except private (public, protected, package-private)
    const ACC_PRIVATE: u16 = 0x0002;
    let visible = |flags: u16| (flags & ACC_PRIVATE) == 0;

    let mut first = true;
    for f in parser.fields() {
        if !visible(f.access_flags) {
            continue;
        }
        if !first {
            out.push_str("\n");
        }
        first = false;
        if let Ok(sig) = format_field_signature(cf, f) {
            out.push_str(&format!("  {}\n", sig));
        }
    }
    for m in parser.methods() {
        if !visible(m.access_flags) {
            continue;
        }
        if !first {
            out.push_str("\n");
        }
        first = false;
        if let Some(ref code) = m.code {
            if let Ok(s) = disassemble_method(cf, m, code) {
                out.push_str(&s);
            }
        } else if let Ok(sig) = format_method_signature(cf, m) {
            out.push_str(&format!("  {}\n", sig));
        }
    }
    out.push_str("}\n");
    out
}

/// Disassemble all .class files in a JAR to a javap -c style string (one class block after another).
pub fn disassemble_jar_file(path: &Path) -> String {
    let jar = match JarFileParser::open(path) {
        Ok(j) => j,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    for parser in jar.class_parsers() {
        let cf = parser.class_file();
        if let Ok(header) = format_class_header(cf) {
            if !out.is_empty() {
                out.push_str("\n");
            }
            out.push_str(&header);
            out.push('\n');
        }
        const ACC_PRIVATE: u16 = 0x0002;
        let visible = |flags: u16| (flags & ACC_PRIVATE) == 0;

        let mut first = true;
        for f in parser.fields() {
            if !visible(f.access_flags) {
                continue;
            }
            if !first {
                out.push_str("\n");
            }
            first = false;
            if let Ok(sig) = format_field_signature(cf, f) {
                out.push_str(&format!("  {}\n", sig));
            }
        }
        for m in parser.methods() {
            if !visible(m.access_flags) {
                continue;
            }
            if !first {
                out.push_str("\n");
            }
            first = false;
            if let Some(ref code) = m.code {
                if let Ok(s) = disassemble_method(cf, m, code) {
                    out.push_str(&s);
                }
            } else if let Ok(sig) = format_method_signature(cf, m) {
                out.push_str(&format!("  {}\n", sig));
            }
        }
        out.push_str("}\n");
    }
    out
}
