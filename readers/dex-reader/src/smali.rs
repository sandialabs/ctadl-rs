//! Smali-format disassembly: format DEX method code as smali-style lines (opcode + operands, no offset prefix).

use crate::instructions::DispArg;
use crate::parser::{DecodedCodeItem, DexParser, decode_code_item, parse_type_list};
use crate::types::{CodeItem, DexConstantPool};

/// Format a code item as smali-style lines (opcode and operands only, no offset prefix).
/// Payloads (e.g. .sparse-switch, .array-data) emit multiple logical lines; each is a separate string.
pub fn format_code_item_smali(code: &CodeItem, cp: &DexConstantPool) -> Vec<String> {
    let instructions = decode_code_item(code);
    let mut result = Vec::new();

    for item in instructions {
        let display_str = match &item {
            DecodedCodeItem::Instruction { inst, .. } => format!("{}", DispArg(inst, cp)),
            DecodedCodeItem::Payload { payload, .. } => format!("{}", DispArg(payload, cp)),
        };
        for line in display_str.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                result.push(trimmed.to_string());
            }
        }
    }

    result
}

fn method_descriptor(parser: &DexParser, proto_idx: usize) -> String {
    let proto = &parser.proto_ids[proto_idx];
    let ret = parser
        .type_ids
        .get(proto.return_type_idx as usize)
        .and_then(|t| t.descriptor(&parser.strings).ok())
        .unwrap_or("<invalid_ret>".into());

    let mut out = String::from("(");
    if proto.parameters_off != 0 {
        let list = parse_type_list(parser.data, proto.parameters_off).unwrap();
        for type_idx in list.types {
            let d = parser
                .type_ids
                .get(type_idx as usize)
                .and_then(|t| t.descriptor(&parser.strings).ok())
                .unwrap_or("<invalid_param>".into());
            out.push_str(&d);
        }
    }
    out.push(')');
    out.push_str(&ret);
    out
}

/// Iterate over all methods with code in a DEX and return (method_key, smali_lines) for each.
/// Method key format: `class_descriptor->method_name(proto_descriptor)`.
pub fn method_disassembly_smali(parser: &DexParser) -> Vec<(String, Vec<String>)> {
    let cp = parser.constant_pool();
    let mut out = Vec::new();

    for class_def in parser.classes() {
        let class_data = parser.class_data(class_def).unwrap();
        for enc in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            let mi = &parser.method_ids[enc.method_idx as usize];

            let class_desc = parser
                .type_ids
                .get(mi.class_idx as usize)
                .and_then(|t| t.descriptor(&parser.strings).ok())
                .unwrap_or("<invalid_class>".into());

            let name = parser
                .strings
                .get(mi.name_idx as usize)
                .unwrap_or("<invalid_name>".into());

            let desc = method_descriptor(parser, mi.proto_idx as usize);
            let key = format!("{class_desc}->{name}{desc}");

            let Some(code_item) = enc.code(parser.data).unwrap() else {
                continue;
            };

            let lines = format_code_item_smali(&code_item, cp);
            out.push((key, lines));
        }
    }

    out
}

/// APK variant: iterate over all methods with code across all DEXes and return (method_key, smali_lines).
pub fn method_disassembly_smali_apk(apk: &crate::apk::APKParser) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let parsers = apk.dex_parsers();

    for apk_class in apk.classes() {
        let p = &parsers[apk_class.dex_index];
        let cp = p.constant_pool();
        let class_data = apk.class_data(&apk_class).expect("class_data");

        for enc in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            let mi = &p.method_ids[enc.method_idx as usize];
            let class_desc = p
                .type_ids
                .get(mi.class_idx as usize)
                .and_then(|t| t.descriptor(&p.strings).ok())
                .unwrap_or("<invalid_class>".into());
            let name = p
                .strings
                .get(mi.name_idx as usize)
                .unwrap_or("<invalid_name>".into());
            let desc = method_descriptor(p, mi.proto_idx as usize);
            let key = format!("{class_desc}->{name}{desc}");

            let Some(code_item) = enc.code(p.data).expect("code") else {
                continue;
            };

            let lines = format_code_item_smali(&code_item, cp);
            out.push((key, lines));
        }
    }

    out
}
