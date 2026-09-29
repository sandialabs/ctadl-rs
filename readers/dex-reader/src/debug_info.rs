use std::io;

use crate::error::{DexError, DexResult};
use crate::parse_utils::{read_sleb128, read_u8, read_uleb128};
use crate::parser::DexParser;
use crate::types::{
    CodeItem, DBG_FIRST_SPECIAL, DBG_LINE_BASE, DBG_LINE_RANGE, DebugInfoItem, DebugInfoOpcode,
    LineMapEntry, NO_INDEX, PositionEntry, StringTable,
};

// ---------------------------------------------------------------------------
// Layer 1 — raw parse
// ---------------------------------------------------------------------------

/// Decode a raw ULEB128 "index+1" field: 0 means absent (`NO_INDEX`), otherwise `v - 1`.
#[inline]
fn decode_index(v: u32) -> u32 {
    if v == 0 { NO_INDEX } else { v - 1 }
}

/// Parse a `debug_info_item` at the given absolute byte offset in `data`.
///
/// Returns `DexError::InvalidDex` when `offset` is 0 (absent debug info).
pub fn parse_debug_info(data: &[u8], offset: u32) -> DexResult<DebugInfoItem> {
    if offset == 0 {
        return Err(DexError::InvalidDex("debug_info_off is zero"));
    }
    let mut pos = offset as usize;

    let (line_start, p) = read_uleb128(data, pos)?;
    pos = p;

    let (parameters_size, p) = read_uleb128(data, pos)?;
    pos = p;

    let mut parameter_names = Vec::with_capacity(parameters_size as usize);
    for _ in 0..parameters_size {
        let (v, p) = read_uleb128(data, pos)?;
        pos = p;
        parameter_names.push(decode_index(v));
    }

    let mut opcodes = Vec::new();
    loop {
        let byte = read_u8(data, pos)?;
        pos += 1;
        match byte {
            0x00 => {
                opcodes.push(DebugInfoOpcode::EndSequence);
                break;
            }
            0x01 => {
                let (addr_diff, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::AdvancePc { addr_diff });
            }
            0x02 => {
                let (line_diff, p) = read_sleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::AdvanceLine { line_diff });
            }
            0x03 => {
                let (register_num, p) = read_uleb128(data, pos)?;
                pos = p;
                let (raw_name, p) = read_uleb128(data, pos)?;
                pos = p;
                let (raw_type, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::StartLocal {
                    register_num,
                    name_idx: decode_index(raw_name),
                    type_idx: decode_index(raw_type),
                });
            }
            0x04 => {
                let (register_num, p) = read_uleb128(data, pos)?;
                pos = p;
                let (raw_name, p) = read_uleb128(data, pos)?;
                pos = p;
                let (raw_type, p) = read_uleb128(data, pos)?;
                pos = p;
                let (raw_sig, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::StartLocalExtended {
                    register_num,
                    name_idx: decode_index(raw_name),
                    type_idx: decode_index(raw_type),
                    sig_idx: decode_index(raw_sig),
                });
            }
            0x05 => {
                let (register_num, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::EndLocal { register_num });
            }
            0x06 => {
                let (register_num, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::RestartLocal { register_num });
            }
            0x07 => {
                opcodes.push(DebugInfoOpcode::SetPrologueEnd);
            }
            0x08 => {
                opcodes.push(DebugInfoOpcode::SetEpilogueBegin);
            }
            0x09 => {
                let (raw_name, p) = read_uleb128(data, pos)?;
                pos = p;
                opcodes.push(DebugInfoOpcode::SetFile {
                    name_idx: decode_index(raw_name),
                });
            }
            special => {
                // Special opcodes 0x0a–0xff: simultaneously advance address and line.
                let adj = (special - DBG_FIRST_SPECIAL) as u32;
                let addr_delta = adj / DBG_LINE_RANGE;
                let line_delta = DBG_LINE_BASE + (adj % DBG_LINE_RANGE) as i32;
                opcodes.push(DebugInfoOpcode::Special {
                    addr_delta,
                    line_delta,
                });
            }
        }
    }

    Ok(DebugInfoItem {
        offset,
        line_start,
        parameters_size,
        parameter_names,
        opcodes,
    })
}

// ---------------------------------------------------------------------------
// Layer 2 — state machine interpreter
// ---------------------------------------------------------------------------

/// Run the `debug_info_item` state machine and return a line-number map.
///
/// `initial_source_file` is the source file name from the enclosing `ClassDef`
/// (empty string when absent). `strings` is needed to resolve `SetFile` opcodes
/// that change the source file mid-method.
///
/// Only `Special` opcodes emit `PositionEntry` records. The entries are ordered
/// by ascending `address`.
pub fn compute_line_map(
    debug_info: &DebugInfoItem,
    code_item: &CodeItem,
    initial_source_file: String,
    strings: &StringTable<'_>,
) -> Vec<PositionEntry> {
    let mut address: u32 = 0;
    let mut line: u32 = debug_info.line_start;
    let mut source_file = initial_source_file;
    let mut entries = Vec::new();

    for op in &debug_info.opcodes {
        match op {
            DebugInfoOpcode::EndSequence => break,
            DebugInfoOpcode::AdvancePc { addr_diff } => {
                address += addr_diff;
            }
            DebugInfoOpcode::AdvanceLine { line_diff } => {
                line = (line as i64 + *line_diff as i64) as u32;
            }
            DebugInfoOpcode::SetFile { name_idx } => {
                if *name_idx != NO_INDEX {
                    if let Ok(name) = strings.get(*name_idx as usize) {
                        source_file = name;
                    }
                }
                // NO_INDEX means "leave source file unchanged"
            }
            DebugInfoOpcode::Special {
                addr_delta,
                line_delta,
            } => {
                address += addr_delta;
                line = (line as i64 + *line_delta as i64) as u32;
                // absolute offset = code_item header (16 bytes) + address * 2 bytes per code unit
                let absolute_offset = code_item.code_off as u64 + 16 + address as u64 * 2;
                entries.push(PositionEntry {
                    address,
                    absolute_offset,
                    source_file: source_file.clone(),
                    line,
                });
            }
            // StartLocal, StartLocalExtended, EndLocal, RestartLocal,
            // SetPrologueEnd, SetEpilogueBegin do not emit position entries.
            _ => {}
        }
    }

    entries
}

// ---------------------------------------------------------------------------
// Layer 3 — collection and JSON serialization
// ---------------------------------------------------------------------------

/// Collect all line-map entries from every method in a `DexParser`.
///
/// Methods with no debug info are silently skipped. Errors from individual
/// methods are also silently skipped to be resilient against malformed data.
pub fn collect_line_map_entries(parser: &DexParser<'_>) -> Vec<LineMapEntry> {
    let mut result = Vec::new();

    for class_def in parser.classes() {
        // Resolve the source file for this class.
        let class_source_file = class_def
            .source_file(&parser.strings)
            .and_then(|r| r.ok())
            .unwrap_or_default();

        let class_data = match parser.class_data(class_def) {
            Ok(cd) => cd,
            Err(_) => continue,
        };

        let all_methods = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter());

        for enc_method in all_methods {
            if enc_method.code_off == 0 {
                continue;
            }

            let code_item = match parser.method_code(enc_method) {
                Ok(Some(ci)) => ci,
                _ => continue,
            };

            if code_item.debug_info_off == 0 {
                continue;
            }

            let debug_info = match parser.debug_info(&code_item) {
                Ok(Some(di)) => di,
                _ => continue,
            };

            let position_entries = compute_line_map(
                &debug_info,
                &code_item,
                class_source_file.clone(),
                &parser.strings,
            );

            if position_entries.is_empty() {
                continue;
            }

            // Build the fully-qualified method label.
            let method_label = parser
                .get_method(enc_method.method_idx as usize)
                .and_then(|mid| parser.method_signature(mid).ok())
                .unwrap_or_else(|| format!("<method {}>", enc_method.method_idx));

            for pe in position_entries {
                result.push(LineMapEntry {
                    method: method_label.clone(),
                    dex_offset: pe.absolute_offset,
                    source_file: pe.source_file,
                    line: pe.line,
                });
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// JSON serialization
// ---------------------------------------------------------------------------

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
/// Each element is a JSON object with keys `method`, `source_file`,
/// `dex_offset`, and `line`. The output is written to `writer`.
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
