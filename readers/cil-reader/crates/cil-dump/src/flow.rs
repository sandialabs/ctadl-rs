//! `cil-dump --flow`: the flow layer's view of every method body, for review
//! and for the flow goldens (`xtask/cil-goldens/flow`).
//!
//! Not part of the Cecil parity dump: Cecil has no such view. Each
//! instruction prints as the Formatter renders it (operand names included),
//! then its Layer 1 record: roles, pushes, effects and control. Tokens in
//! effects print as hex; the instruction line names them.

use cil_reader::Result;
use cil_reader::flow::{Control, InsFlow};
use cil_reader::model::{MethodBody, Module};
use cil_reader::raw::Table;
use std::fmt::Write as _;

/// The flow dump of every method body of `module`, in token order.
pub fn dump_flow(module: &Module<'_>) -> Result<String> {
    let mut out = String::new();
    for rid in 1..=module.metadata().row_count(Table::Method) {
        let Some(method) = module.method_def(rid) else {
            continue;
        };
        if !method.has_body()? || method.rva()? == 0 || method.is_deleted().unwrap_or(false) {
            continue;
        }
        let name = method.full_name().unwrap_or_else(|_| "!error".into());
        writeln!(out, "{:08x} {name}", method.token().raw()).unwrap();
        match method.body() {
            Ok(body) => write_body(&body, &mut out),
            Err(e) => writeln!(out, "\t!error {e}").unwrap(),
        }
    }
    Ok(out)
}

fn write_body(body: &MethodBody<'_>, out: &mut String) {
    for flow in body.flows() {
        match flow {
            Ok(flow) => write_flow(body, &flow, out),
            Err(e) => writeln!(out, "\t!error {e}").unwrap(),
        }
    }
}

/// One instruction: the formatted instruction, then its record.
pub fn write_flow(body: &MethodBody<'_>, flow: &InsFlow, out: &mut String) {
    let ins = body
        .instructions
        .iter()
        .find(|i| i.end_offset() == flow.end_offset());
    let text = ins
        .and_then(|i| body.format_instruction(i).ok())
        .unwrap_or_else(|| format!("IL_{:04x}: {} !error", flow.offset, flow.opcode.name()));
    write!(out, "\t{text}").unwrap();
    if !flow.prefixes.is_empty() {
        write!(out, " [{}] at IL_{:04x}", flow.prefixes, flow.offset).unwrap();
    }
    out.push('\n');
    if !flow.inputs.is_empty() || flow.outputs != 0 {
        let roles: Vec<String> = flow.inputs.iter().map(|r| format!("{r:?}")).collect();
        writeln!(out, "\t\tin [{}] out {}", roles.join(", "), flow.outputs).unwrap();
    }
    for e in &flow.effects {
        writeln!(out, "\t\t{e}").unwrap();
    }
    if flow.control != Control::Next {
        writeln!(out, "\t\t-> {}", flow.control).unwrap();
    }
}
