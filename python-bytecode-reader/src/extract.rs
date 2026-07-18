//! Extraction: pest parse tree → typed records ([`crate::model`]).
//!
//! Kept separate from [`crate::parse`] so that extraction errors (overflow, bad
//! structure, unsupported version) are tested independently from syntax errors.

use pest::Parser;
use pest::iterators::Pair;

use crate::error::ParseError;
use crate::model::*;
use crate::parse::{BytecodeParser, Rule};

/// The only `bytecode_format` version this reader understands.
pub const SUPPORTED_FORMAT_VERSION: u32 = 1;

/// Parse stable bytecode text into a [`BytecodeFile`].
///
/// Syntactic errors surface as [`ParseError::Pest`]; semantic errors (overflow,
/// unsupported version) as [`ParseError::Extract`]. Never panics on any input.
pub fn parse(input: &str) -> Result<BytecodeFile, ParseError> {
    let mut pairs =
        BytecodeParser::parse(Rule::file, input).map_err(|e| ParseError::Pest(Box::new(e)))?;
    // Rule::file is infallible-present once parse() succeeds.
    let file = pairs.next().expect("file rule");
    extract_file(file)
}

fn extract_file(pair: Pair<'_, Rule>) -> Result<BytecodeFile, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::file);
    let mut format_version = None;
    let mut code_objects = Vec::new();
    for child in pair.into_inner() {
        match child.as_rule() {
            Rule::header => {
                let int = child.into_inner().next().expect("header integer");
                format_version = Some(parse_u32(&int)?);
            }
            Rule::code_object => code_objects.push(extract_code_object(child)?),
            Rule::EOI => {}
            other => return Err(ParseError::extract(&child, format!("unexpected {other:?}"))),
        }
    }
    let format_version = format_version.unwrap_or(0);
    if format_version != SUPPORTED_FORMAT_VERSION {
        return Err(ParseError::Extract {
            message: format!(
                "unsupported bytecode_format version {format_version} \
                 (this reader supports {SUPPORTED_FORMAT_VERSION})"
            ),
            line: 1,
            col: 1,
        });
    }
    Ok(BytecodeFile {
        format_version,
        code_objects,
    })
}

fn extract_code_object(pair: Pair<'_, Rule>) -> Result<CodeObject, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::code_object);
    let mut co = CodeObject::default();
    for field in pair.into_inner() {
        match field.as_rule() {
            Rule::name_field => co.name = string_of(field)?,
            Rule::qualname_field => co.qualname = string_of(field)?,
            Rule::filename_field => co.filename = string_of(field)?,
            Rule::first_line_field => co.first_line = opt_int_of(field)?,
            Rule::flags_field => co.flags = int_of(field)?,
            Rule::arg_count_field => co.arg_count = int_of(field)?,
            Rule::kwonly_count_field => co.kwonly_count = int_of(field)?,
            Rule::names_field => co.names = string_list_of(field)?,
            Rule::varnames_field => co.varnames = string_list_of(field)?,
            Rule::consts_field => co.consts = value_list_of(field)?,
            Rule::instruction => co.instructions.push(extract_instruction(field)?),
            Rule::code_object => co.nested_code_objects.push(extract_code_object(field)?),
            other => return Err(ParseError::extract(&field, format!("unexpected {other:?}"))),
        }
    }
    Ok(co)
}

fn extract_instruction(pair: Pair<'_, Rule>) -> Result<Instruction, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::instruction);
    let mut insn = Instruction {
        offset: 0,
        opname: String::new(),
        opcode: 0,
        arg: None,
        argval: ConstEntry::None,
        argrepr: None,
        starts_line: None,
        is_jump_target: false,
        jump_targets: Vec::new(),
        position: None,
    };
    for field in pair.into_inner() {
        match field.as_rule() {
            Rule::offset_field => insn.offset = int_of(field)?,
            Rule::opname_field => insn.opname = only(field).as_str().to_string(),
            Rule::opcode_field => insn.opcode = int_of(field)?,
            Rule::arg_field => insn.arg = opt_int_of(field)?,
            Rule::argval_field => insn.argval = extract_value(only(field))?,
            Rule::argrepr_field => insn.argrepr = opt_string_of(field)?,
            Rule::starts_line_field => insn.starts_line = opt_int_of(field)?,
            Rule::is_jump_target_field => insn.is_jump_target = bool_of(only(field)),
            Rule::jump_targets_field => insn.jump_targets = int_list_of(field)?,
            Rule::position_field => insn.position = extract_position(only(field))?,
            other => return Err(ParseError::extract(&field, format!("unexpected {other:?}"))),
        }
    }
    Ok(insn)
}

fn extract_position(pair: Pair<'_, Rule>) -> Result<Option<Position>, ParseError> {
    match pair.as_rule() {
        Rule::none_kw => Ok(None),
        Rule::position => {
            let mut it = pair.into_inner();
            let start_line = parse_i64(&it.next().unwrap())?;
            let start_column = parse_i64(&it.next().unwrap())?;
            let end_line = parse_i64(&it.next().unwrap())?;
            let end_column = parse_i64(&it.next().unwrap())?;
            Ok(Some(Position {
                start_line,
                start_column,
                end_line,
                end_column,
            }))
        }
        other => Err(ParseError::extract(
            &pair,
            format!("expected position, got {other:?}"),
        )),
    }
}

fn extract_value(pair: Pair<'_, Rule>) -> Result<ConstEntry, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::value);
    let inner = only(pair);
    let entry = match inner.as_rule() {
        Rule::const_none => ConstEntry::None,
        Rule::const_bool => ConstEntry::Bool(bool_of(only(inner))),
        Rule::const_int => ConstEntry::Int(parse_i64(&only(inner))?),
        Rule::const_float => ConstEntry::Float(unescape(only(inner))?),
        Rule::const_str => ConstEntry::Str(unescape(only(inner))?),
        Rule::const_bytes => ConstEntry::Bytes(unescape(only(inner))?),
        Rule::const_code => ConstEntry::Code(parse_u32(&only(inner))?),
        Rule::const_other => ConstEntry::Other(unescape(only(inner))?),
        other => {
            return Err(ParseError::extract(
                &inner,
                format!("unexpected value {other:?}"),
            ));
        }
    };
    Ok(entry)
}

// --- Small typed accessors ------------------------------------------------

/// The single child of a one-child rule.
fn only(pair: Pair<'_, Rule>) -> Pair<'_, Rule> {
    pair.into_inner().next().expect("rule has one child")
}

/// A field wrapping one `integer`.
fn int_of(field: Pair<'_, Rule>) -> Result<i64, ParseError> {
    parse_i64(&only(field))
}

/// A field wrapping `integer | none_kw`.
fn opt_int_of(field: Pair<'_, Rule>) -> Result<Option<i64>, ParseError> {
    let inner = only(field);
    match inner.as_rule() {
        Rule::none_kw => Ok(None),
        Rule::integer => Ok(Some(parse_i64(&inner)?)),
        other => Err(ParseError::extract(
            &inner,
            format!("expected int or none, got {other:?}"),
        )),
    }
}

/// A field wrapping one `string`.
fn string_of(field: Pair<'_, Rule>) -> Result<String, ParseError> {
    unescape(only(field))
}

/// A field wrapping `string | none_kw`.
fn opt_string_of(field: Pair<'_, Rule>) -> Result<Option<String>, ParseError> {
    let inner = only(field);
    match inner.as_rule() {
        Rule::none_kw => Ok(None),
        Rule::string => Ok(Some(unescape(inner)?)),
        other => Err(ParseError::extract(
            &inner,
            format!("expected string or none, got {other:?}"),
        )),
    }
}

fn string_list_of(field: Pair<'_, Rule>) -> Result<Vec<String>, ParseError> {
    let list = only(field);
    list.into_inner().map(unescape).collect()
}

fn int_list_of(field: Pair<'_, Rule>) -> Result<Vec<i64>, ParseError> {
    let list = only(field);
    list.into_inner().map(|p| parse_i64(&p)).collect()
}

fn value_list_of(field: Pair<'_, Rule>) -> Result<Vec<ConstEntry>, ParseError> {
    let list = only(field);
    list.into_inner().map(extract_value).collect()
}

fn bool_of(pair: Pair<'_, Rule>) -> bool {
    pair.as_str() == "true"
}

fn parse_i64(pair: &Pair<'_, Rule>) -> Result<i64, ParseError> {
    pair.as_str()
        .parse::<i64>()
        .map_err(|e| ParseError::extract(pair, format!("invalid integer `{}`: {e}", pair.as_str())))
}

fn parse_u32(pair: &Pair<'_, Rule>) -> Result<u32, ParseError> {
    pair.as_str()
        .parse::<u32>()
        .map_err(|e| ParseError::extract(pair, format!("invalid u32 `{}`: {e}", pair.as_str())))
}

/// Unescape a `Rule::string` pair's JSON-escaped content.
fn unescape(pair: Pair<'_, Rule>) -> Result<String, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::string);
    let (line, col) = pair.line_col();
    let inner = pair.into_inner().next().expect("string inner"); // Rule::inner
    decode_json_escapes(inner.as_str()).map_err(|message| ParseError::Extract {
        message,
        line,
        col,
    })
}

/// Decode JSON string escapes (`\" \\ \/ \b \f \n \r \t \uXXXX`, including
/// UTF-16 surrogate pairs for astral code points) into a Rust `String`.
fn decode_json_escapes(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let esc = chars
            .next()
            .ok_or_else(|| "dangling backslash".to_string())?;
        match esc {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000C}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hi = read_hex4(&mut chars)?;
                if (0xD800..=0xDBFF).contains(&hi) {
                    // High surrogate: consume the following `\uYYYY` low surrogate.
                    if chars.next() != Some('\\') || chars.next() != Some('u') {
                        return Err("high surrogate not followed by \\u escape".to_string());
                    }
                    let lo = read_hex4(&mut chars)?;
                    if !(0xDC00..=0xDFFF).contains(&lo) {
                        return Err("invalid low surrogate".to_string());
                    }
                    let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    out.push(char::from_u32(cp).ok_or_else(|| "invalid code point".to_string())?);
                } else if (0xDC00..=0xDFFF).contains(&hi) {
                    return Err("unexpected low surrogate".to_string());
                } else {
                    out.push(char::from_u32(hi).ok_or_else(|| "invalid code point".to_string())?);
                }
            }
            other => return Err(format!("invalid escape `\\{other}`")),
        }
    }
    Ok(out)
}

fn read_hex4(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<u32, String> {
    let mut v = 0u32;
    for _ in 0..4 {
        let c = chars
            .next()
            .ok_or_else(|| "truncated \\u escape".to_string())?;
        let d = c
            .to_digit(16)
            .ok_or_else(|| format!("bad hex digit `{c}`"))?;
        v = v * 16 + d;
    }
    Ok(v)
}
