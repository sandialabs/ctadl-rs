//! Line map: bytecode file offsets to source lines (from `LineNumberTable`), JSON compatible with dex-reader.

use std::io;

use crate::error::{ClassFileError, ClassFileResult};
use crate::parse_utils::read_u16_be;
use crate::parser::ClassFileParser;
use crate::types::{ClassFile, MethodInfo};

/// Flat record used for JSON line-map serialization (same shape as dex-reader).
#[derive(Debug, Clone)]
pub struct LineMapEntry {
    /// Fully-qualified method reference, e.g. `"Lcom/example/Foo;->bar(I)V"`.
    pub method: String,
    /// Absolute byte offset of the bytecode from the start of the raw `.class` buffer.
    pub dex_offset: u64,
    /// Source file name (empty string if not available).
    pub source_file: String,
    /// Source line number.
    pub line: u32,
}

/// Parse `LineNumberTable` attribute `info` bytes (JVMS §4.7.8).
fn parse_line_number_table(info: &[u8]) -> ClassFileResult<Vec<(u16, u16)>> {
    if info.len() < 2 {
        return Err(ClassFileError::InvalidClassFile(
            "LineNumberTable too short",
        ));
    }
    let count = read_u16_be(info, 0)? as usize;
    let need = 2usize.saturating_add(count.saturating_mul(4));
    if info.len() < need {
        return Err(ClassFileError::InvalidClassFile(
            "LineNumberTable truncated",
        ));
    }
    let mut out = Vec::with_capacity(count);
    let mut pos = 2usize;
    for _ in 0..count {
        let start_pc = read_u16_be(info, pos)?;
        pos += 2;
        let line_number = read_u16_be(info, pos)?;
        pos += 2;
        out.push((start_pc, line_number));
    }
    Ok(out)
}

fn method_dex_style(cf: &ClassFile, m: &MethodInfo) -> ClassFileResult<String> {
    let class = cf.this_class_name()?;
    let name = cf.get_utf8(m.name_index)?;
    let descriptor = cf.get_utf8(m.descriptor_index)?;
    Ok(format!("L{};->{}{}", class, name, descriptor))
}

/// Collect line-map entries from every method in a `ClassFileParser` that has a `LineNumberTable`.
///
/// Methods without code, without a line table, or with parse errors are skipped (same resilience style as dex-reader).
/// Rows whose `start_pc` is out of range for the method bytecode are skipped.
pub fn collect_line_map_entries(parser: &ClassFileParser) -> Vec<LineMapEntry> {
    let cf = parser.class_file();
    let source_file = cf
        .source_file
        .and_then(|idx| cf.get_utf8(idx).ok())
        .unwrap_or("")
        .to_string();

    let mut result = Vec::new();

    for method in &cf.methods {
        let Some(code) = &method.code else {
            continue;
        };

        let method_label = match method_dex_style(cf, method) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut rows: Vec<(u16, u16)> = Vec::new();
        for attr in &code.attributes {
            let name = match cf.get_utf8(attr.name_index) {
                Ok(n) => n,
                Err(_) => continue,
            };
            if name != "LineNumberTable" {
                continue;
            }
            match parse_line_number_table(&attr.info) {
                Ok(r) => rows.extend(r),
                Err(_) => continue,
            }
        }

        if rows.is_empty() {
            continue;
        }

        rows.sort_by_key(|(pc, _)| *pc);

        let code_len = code.code.len();
        for (start_pc, line_number) in rows {
            if (start_pc as usize) >= code_len {
                continue;
            }
            result.push(LineMapEntry {
                method: method_label.clone(),
                dex_offset: code.code_byte_offset_in_classfile as u64 + u64::from(start_pc),
                source_file: source_file.clone(),
                line: u32::from(line_number),
            });
        }
    }

    result
}

/// Escape a string for use in a JSON value (handles `\`, `"`, and control chars).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Serialize a slice of `LineMapEntry` values as a JSON array.
///
/// Each element is a JSON object with keys `method`, `source_file`, `dex_offset`, and `line`.
pub fn write_line_map_json<W: io::Write>(
    writer: &mut W,
    entries: &[LineMapEntry],
) -> io::Result<()> {
    writer.write_all(b"[\n")?;
    for (i, entry) in entries.iter().enumerate() {
        let comma = if i + 1 < entries.len() { "," } else { "" };
        writeln!(
            writer,
            "  {{\"method\":\"{}\",\"source_file\":\"{}\",\"dex_offset\":{},\"line\":{}}}{}",
            json_escape(&entry.method),
            json_escape(&entry.source_file),
            entry.dex_offset,
            entry.line,
            comma,
        )?;
    }
    writer.write_all(b"]\n")?;
    Ok(())
}
