//! `cil-dump --flow`: the flow layer's view of every method body, for review
//! and for the flow goldens (`xtask/cil-goldens/flow`).
//!
//! Not part of the Cecil parity dump: Cecil has no such view. For each
//! method, first Layer 1: each instruction as the Formatter renders it
//! (operand names included), then its record: roles, pushes, effects and
//! control. Tokens in effects print as hex; the instruction line names them.
//! Then Layer 2: the handlers, each block with its entry height and kinds,
//! successors and bound effects (with the inputs' kinds and resolved
//! addresses), and the warnings.

use cil_reader::Result;
use cil_reader::flow::{Control, InsFlow, InsId, MethodFlow, StackKind};
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
            Ok(body) => {
                write_body(&body, &mut out);
                write_method_flow(&MethodFlow::build(&body), &mut out);
            }
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

fn kind(k: &StackKind) -> String {
    match k {
        StackKind::Int32 => "i4".into(),
        StackKind::Int64 => "i8".into(),
        StackKind::NativeInt => "i".into(),
        StackKind::Float => "f".into(),
        StackKind::ObjRef => "o".into(),
        StackKind::ManagedPtr => "&".into(),
        StackKind::ValueType(t) => format!("vt({t})"),
        StackKind::Unknown => "?".into(),
    }
}

fn kinds(ks: &[StackKind]) -> String {
    ks.iter().map(kind).collect::<Vec<_>>().join(" ")
}

/// Layer 2 of one method.
pub fn write_method_flow(mf: &MethodFlow, out: &mut String) {
    out.push_str("\t-- layer 2\n");
    for (i, h) in mf.handlers.iter().enumerate() {
        let entry = |b: Option<usize>| b.map_or("-".to_string(), |b| format!("B{b}"));
        writeln!(
            out,
            "\thandler {i} {:?} try IL_{:04x}..IL_{:04x} entry {} handler {}",
            h.kind,
            h.try_start,
            h.try_end,
            entry(h.entry),
            entry(h.handler_block)
        )
        .unwrap();
    }
    for (b, block) in mf.blocks.iter().enumerate() {
        let ids = |v: &[usize]| {
            v.iter()
                .map(|b| format!("B{b}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        write!(
            out,
            "\tB{b} IL_{:04x}..IL_{:04x} entry {} [{}] exit {}",
            block.start,
            block.end,
            block.entry_height,
            kinds(&block.entry_kinds),
            block.exit_height
        )
        .unwrap();
        if !block.succs.is_empty() {
            write!(out, " -> {}", ids(&block.succs)).unwrap();
        }
        if !block.exc_succs.is_empty() {
            write!(out, " exc {}", ids(&block.exc_succs)).unwrap();
        }
        out.push('\n');
        for (index, ins) in block.instrs.iter().enumerate() {
            let at = InsId { block: b, index };
            write!(out, "\t\tIL_{:04x}", ins.flow.offset).unwrap();
            if !ins.inputs.is_empty() {
                write!(out, " in [{}]", kinds(&ins.input_kinds)).unwrap();
            }
            for (i, place) in ins.input_places.iter().enumerate() {
                if let Some(p) = place {
                    write!(out, " i{i}=&{p}").unwrap();
                }
            }
            if mf.address_escapes_resolution(at) {
                out.push_str(" escapes");
            }
            out.push('\n');
            for e in &ins.effects {
                writeln!(out, "\t\t\t{e}").unwrap();
            }
        }
    }
    let taken: Vec<String> = mf.address_taken().iter().map(|v| v.to_string()).collect();
    if !taken.is_empty() {
        writeln!(out, "\taddress-taken {}", taken.join(" ")).unwrap();
    }
    for w in &mf.warnings {
        writeln!(out, "\twarning {w}").unwrap();
    }
}
