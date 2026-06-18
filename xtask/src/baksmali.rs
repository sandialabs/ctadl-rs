//! Compare dex-reader's smali disassembly against baksmali as ground truth.
//!
//! Ported from the dex-reader-orig `tests/baksmali_diff.rs`, which was dropped
//! when the crate was vendored here. We run `baksmali d` (provided by the Nix
//! environment — see the `baksmali` wrapper in flake.nix) on a
//! compiled-from-source `.dex`, parse its per-method smali, apply a battery of
//! representation normalizations, and require dex-reader's own smali output to
//! match baksmali method-for-method.
//!
//! The normalizations exist because the two disassemblers make cosmetically
//! different but semantically equal choices: hex vs decimal literals, branch
//! offsets vs labels, `pN` parameter registers vs absolute `vN`, `\uXXXX` vs
//! literal Unicode in strings, etc. We canonicalize both sides before
//! comparing so only genuine disagreements fail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use dex_reader::{smali, DexParser};

use crate::exec;

/// Disassemble `dex_path` with baksmali into `out_dir`, then compare every
/// code-bearing method against dex-reader's smali. Returns the number of
/// methods compared (so the caller can assert it did real work).
pub fn compare(dex_path: &Path, dex_bytes: &[u8], out_dir: &Path) -> Result<usize> {
    exec::fresh_dir(out_dir)?;
    run_baksmali(dex_path, out_dir)?;

    let mut smali_paths = Vec::new();
    read_smali_files_recursive(out_dir, &mut smali_paths)?;
    if smali_paths.is_empty() {
        bail!(
            "baksmali produced no .smali files for {}",
            dex_path.display()
        );
    }

    let mut expected_map: HashMap<String, Vec<String>> = HashMap::new();
    for smali in smali_paths {
        for (k, v) in parse_baksmali_methods(&smali)? {
            expected_map.insert(k, v);
        }
    }

    let parser = DexParser::new(dex_bytes)
        .map_err(|e| anyhow!("DexParser for {}: {e}", dex_path.display()))?;
    let ours = smali::method_disassembly_smali(&parser);

    let mut checked = 0usize;
    for (key, raw_lines) in ours {
        let actual_lines = normalize_our_smali_lines(&raw_lines);
        if actual_lines.is_empty() {
            continue;
        }
        let Some(expected_lines) = expected_map.get(&key) else {
            bail!(
                "baksmali output missing method key {key}\n(dex: {})",
                dex_path.display()
            );
        };

        // Re-apply switch-target normalization at comparison so both sides match
        // regardless of label-numbering differences.
        let expected_norm: Vec<String> = expected_lines
            .iter()
            .map(|s| normalize_switch_targets(s))
            .collect();
        let actual_norm: Vec<String> = actual_lines
            .iter()
            .map(|s| normalize_switch_targets(s))
            .collect();
        if expected_norm != actual_norm {
            bail!(
                "smali diff for method {key}\n(dex: {})\n{}",
                dex_path.display(),
                diff_preview(&expected_norm, &actual_norm)
            );
        }
        checked += 1;
    }
    Ok(checked)
}

fn run_baksmali(dex: &Path, out_dir: &Path) -> Result<()> {
    let mut cmd = Command::new("baksmali");
    cmd.arg("d").arg(dex).arg("-o").arg(out_dir);
    exec::run_checked(cmd, "baksmali")?;
    Ok(())
}

fn read_smali_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for ent in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = ent?.path();
        if p.is_dir() {
            read_smali_files_recursive(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("smali") {
            out.push(p);
        }
    }
    Ok(())
}

// --- normalization (ported verbatim from baksmali_diff.rs) -----------------

fn keep_directive(s: &str) -> bool {
    matches!(
        s,
        ".packed-switch"
            | ".sparse-switch"
            | ".array-data"
            | ".end packed-switch"
            | ".end sparse-switch"
            | ".end array-data"
    ) || s.starts_with(".packed-switch")
        || s.starts_with(".sparse-switch")
        || s.starts_with(".array-data")
        || s.starts_with(".end packed-switch")
        || s.starts_with(".end sparse-switch")
        || s.starts_with(".end array-data")
}

fn normalize_numeric_literals(line: &str) -> String {
    // Canonicalize hex literals to decimal so "0x1" vs "1" don't fail. Handles
    // an optional leading '-': -0x1.
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];

        let prev_is_ident = i > 0 && chars[i - 1].is_ascii_alphanumeric();
        if !prev_is_ident {
            // -0x...
            if c == '-'
                && i + 3 < chars.len()
                && chars[i + 1] == '0'
                && (chars[i + 2] == 'x' || chars[i + 2] == 'X')
                && chars[i + 3].is_ascii_hexdigit()
            {
                let mut j = i + 3;
                while j < chars.len() && chars[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let hex: String = chars[i + 3..j].iter().collect();
                if let Ok(v) = i64::from_str_radix(&hex, 16) {
                    out.push_str(&(-v).to_string());
                    i = j;
                    continue;
                }
            }

            // 0x...
            if c == '0'
                && i + 2 < chars.len()
                && (chars[i + 1] == 'x' || chars[i + 1] == 'X')
                && chars[i + 2].is_ascii_hexdigit()
            {
                let mut j = i + 2;
                while j < chars.len() && chars[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let hex: String = chars[i + 2..j].iter().collect();
                if let Ok(v) = i64::from_str_radix(&hex, 16) {
                    out.push_str(&v.to_string());
                    i = j;
                    continue;
                }
            }
        }

        out.push(c);
        i += 1;
    }
    out
}

fn normalize_const_string(line: &str) -> String {
    // const-string: baksmali uses \uXXXX for non-ASCII; we may print literal
    // chars. Normalize both to \u form (lowercase hex).
    if let Some(rest) = line.strip_prefix("const-string ") {
        if let Some((reg_part, after_comma)) = rest.split_once(',') {
            let after_comma = after_comma.trim();
            if let Some(quoted) = after_comma
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
            {
                // Normalize \t / \\t and \n / \\n so both sides match.
                let quoted = quoted
                    .replace("\\t", "\t")
                    .replace("\\n", "\n")
                    .replace("\\r", "\r");
                // Normalize Rust \u{XXXX} to Java \uXXXX (4 hex digits).
                let quoted = {
                    let mut result = String::with_capacity(quoted.len());
                    let mut cur = quoted.as_str();
                    while let Some(start) = cur.find("\\u{") {
                        result.push_str(&cur[..start]);
                        let rest = &cur[start + 3..];
                        if let Some(end) = rest.find('}') {
                            let hex = &rest[..end];
                            if !hex.is_empty()
                                && hex.len() <= 8
                                && hex.chars().all(|c| c.is_ascii_hexdigit())
                            {
                                let hex_lower = hex.to_lowercase();
                                let hex_4 = if hex_lower.len() >= 4 {
                                    hex_lower[..4].to_string()
                                } else {
                                    format!("{:0>4}", hex_lower)
                                };
                                result.push_str(&format!("\\u{}", hex_4));
                                cur = &rest[end + 1..];
                            } else {
                                result.push_str("\\u{");
                                cur = rest;
                            }
                        } else {
                            result.push_str("\\u{");
                            cur = rest;
                        }
                    }
                    result.push_str(cur);
                    result
                };
                // If already \u-escaped (baksmali), just lowercase the hex.
                if quoted.contains("\\u") {
                    let mut out = String::from("const-string ");
                    out.push_str(reg_part.trim());
                    out.push_str(", \"");
                    let mut i = 0;
                    let chars: Vec<char> = quoted.chars().collect();
                    while i < chars.len() {
                        if i + 5 <= chars.len()
                            && chars[i] == '\\'
                            && chars[i + 1] == 'u'
                            && chars[i + 2].is_ascii_hexdigit()
                            && chars[i + 3].is_ascii_hexdigit()
                            && chars[i + 4].is_ascii_hexdigit()
                            && chars[i + 5].is_ascii_hexdigit()
                        {
                            let hex: String = chars[i + 2..i + 6]
                                .iter()
                                .collect::<String>()
                                .to_lowercase();
                            out.push_str(&format!("\\u{}", hex));
                            i += 6;
                            continue;
                        }
                        out.push(chars[i]);
                        i += 1;
                    }
                    out.push('"');
                    return out;
                }
                // Our output: literal Unicode -> \uXXXX.
                let mut escaped = String::from('"');
                for c in quoted.chars() {
                    if c.is_ascii() && c != '\\' && c != '"' {
                        escaped.push(c);
                    } else if c == '\\' {
                        escaped.push_str("\\\\");
                    } else if c == '"' {
                        escaped.push_str("\\\"");
                    } else {
                        escaped.push_str(&format!("\\u{:04x}", c as u32));
                    }
                }
                escaped.push('"');
                return format!("const-string {}, {}", reg_part.trim(), escaped);
            }
        }
    }
    line.to_string()
}

fn normalize_wide_literal(line: &str) -> String {
    // const-wide/high16 and const/high16 literals may be shown in different
    // forms; reduce to opcode + reg + placeholder so we compare structure.
    if let Some(rest) = line.strip_prefix("const-wide/high16 ") {
        if let Some((reg_part, _lit)) = rest.split_once(',') {
            let reg_part = reg_part.trim();
            if !reg_part.is_empty() {
                return format!("const-wide/high16 {}, <lit>L", reg_part);
            }
        }
    }
    if let Some(rest) = line.strip_prefix("const/high16 ") {
        if let Some((reg_part, _lit)) = rest.split_once(',') {
            let reg_part = reg_part.trim();
            if !reg_part.is_empty() {
                return format!("const/high16 {}, <lit>", reg_part);
            }
        }
    }
    line.to_string()
}

/// Normalize unsigned decimal that fits in u32 but represents a negative i32
/// (array-data, etc.) to signed form so both sides match.
fn normalize_signed_u32_line(line: &str) -> String {
    let s = line.trim();
    if s.is_empty() {
        return line.to_string();
    }
    if s.starts_with('-') {
        return line.to_string();
    }
    if !s.chars().all(|c| c.is_ascii_digit()) {
        return line.to_string();
    }
    if let Ok(n) = s.parse::<u32>() {
        if n >= 2147483648 {
            return (n as i32).to_string();
        }
    }
    line.to_string()
}

fn normalize_switch_targets(line: &str) -> String {
    // Sparse-switch entries: "65 -> :sswitch_47" vs ":sswitch_0" -> "65 -> <tgt>".
    let arrow_pos = line.find("->").or_else(|| line.find('\u{2192}'));
    if let Some(pos) = arrow_pos {
        let arrow_len = if line.get(pos..).is_some_and(|s| s.starts_with("->")) {
            2
        } else {
            1
        };
        let key_part = line[..pos].trim();
        let after_arrow = line[pos + arrow_len..].trim_start();
        if after_arrow.starts_with(':')
            && !key_part.is_empty()
            && key_part
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == ' ')
        {
            return format!("{} -> <tgt>", key_part);
        }
    }
    line.to_string()
}

/// Normalize obfuscated field names that look like register names (p0, v3) to a
/// placeholder so both sides match regardless of naming.
fn normalize_register_like_field_name(line: &str) -> String {
    if let Some(arrow) = line.find("->") {
        let after_arrow = &line[arrow + 2..];
        if let Some(colon) = after_arrow.find(':') {
            let name = &after_arrow[..colon];
            if (name.starts_with('p') || name.starts_with('v'))
                && name.len() > 1
                && name.chars().skip(1).all(|c| c.is_ascii_digit())
            {
                return format!("{}-><f>{}", &line[..arrow + 2], &after_arrow[colon..]);
            }
        }
    }
    line.to_string()
}

fn normalize_branch_targets(line: &str) -> String {
    // Our disassembler prints numeric branch targets; baksmali prints labels.
    // Treat the target as non-semantic and normalize it away, keeping opcode +
    // regs.
    let mut it = line.splitn(2, char::is_whitespace);
    let mnemonic = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("").trim();
    if rest.is_empty() {
        return line.to_string();
    }

    let is_goto = matches!(mnemonic, "goto" | "goto/16" | "goto/32");
    let is_if = mnemonic.starts_with("if-");
    let is_switch_or_fad = matches!(
        mnemonic,
        "packed-switch" | "sparse-switch" | "fill-array-data"
    );
    if !(is_goto || is_if || is_switch_or_fad) {
        return line.to_string();
    }

    if is_goto {
        return format!("{mnemonic} <tgt>");
    }

    let mut ops: Vec<String> = rest
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !ops.is_empty() {
        *ops.last_mut().unwrap() = "<tgt>".to_string();
    } else {
        ops.push("<tgt>".to_string());
    }
    format!("{mnemonic} {}", ops.join(", "))
}

fn normalize_line_common(line: &str) -> Option<String> {
    let mut s = line.trim();
    if s.is_empty() {
        return None;
    }
    // Drop full-line comments and labels.
    if s.starts_with('#') {
        return None;
    }
    if s.starts_with(':') {
        return None;
    }
    // Strip trailing comments.
    if let Some((before, _)) = s.split_once('#') {
        s = before.trim_end();
        if s.is_empty() {
            return None;
        }
    }
    // Ignore most directives (and drop .end payload directives).
    if s == ".end array-data" || s == ".end sparse-switch" || s == ".end packed-switch" {
        return None;
    }
    if s.starts_with('.') && !keep_directive(s) {
        return None;
    }

    let s = normalize_const_string(s);
    let s = normalize_wide_literal(&s);
    let s = normalize_switch_targets(&s);
    let s = normalize_numeric_literals(&s);
    let s = normalize_branch_targets(&s);
    let s = normalize_signed_u32_line(&s);
    let s = normalize_register_like_field_name(&s);
    Some(s)
}

fn normalize_baksmali_line(line: &str) -> Option<String> {
    normalize_line_common(line)
}

fn method_ins_size_from_descriptor(desc: &str, is_static: bool) -> usize {
    // desc is the dex method descriptor "(...)R". Count parameter registers:
    // each param is 1 reg, except J/D which are 2. Add 1 for `this` if
    // non-static.
    let mut i = 0usize;
    let bytes = desc.as_bytes();
    if bytes.first() != Some(&b'(') {
        return if is_static { 0 } else { 1 };
    }
    i += 1;

    let mut regs = if is_static { 0usize } else { 1usize };
    while i < bytes.len() {
        match bytes[i] as char {
            ')' => break,
            '[' => {
                while i < bytes.len() && bytes[i] == b'[' {
                    i += 1;
                }
                if i >= bytes.len() {
                    break;
                }
                match bytes[i] as char {
                    'L' => {
                        while i < bytes.len() && bytes[i] != b';' {
                            i += 1;
                        }
                        if i < bytes.len() {
                            i += 1;
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
                regs += 1;
            }
            'L' => {
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                regs += 1;
            }
            'J' | 'D' => {
                i += 1;
                regs += 2;
            }
            'B' | 'C' | 'F' | 'I' | 'S' | 'Z' => {
                i += 1;
                regs += 1;
            }
            _ => {
                i += 1;
                regs += 1;
            }
        }
    }
    regs
}

fn rewrite_p_registers_to_v(line: &str, p_base: usize) -> String {
    // Replace pN with v(p_base + N). Handles tokens like p0, {p0}, {p0 .. p3}.
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == 'p'
            && i + 1 < chars.len()
            && chars[i + 1].is_ascii_digit()
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
        {
            let mut j = i + 1;
            let mut n: usize = 0;
            while j < chars.len() && chars[j].is_ascii_digit() {
                n = n * 10 + (chars[j] as u8 - b'0') as usize;
                j += 1;
            }
            out.push('v');
            out.push_str(&(p_base + n).to_string());
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn parse_baksmali_methods(smali_path: &Path) -> Result<HashMap<String, Vec<String>>> {
    let buf = std::fs::read_to_string(smali_path)
        .with_context(|| format!("reading {}", smali_path.display()))?;

    let mut class_desc: Option<String> = None;
    let mut current_key: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_p_base: Option<usize> = None;
    let mut current_ins_size: usize = 0;
    let mut in_annotation: bool = false;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();

    for raw in buf.lines() {
        let line = raw.trim();
        if line.starts_with(".class") {
            if let Some(desc) = line
                .split_whitespace()
                .rev()
                .find(|tok| tok.starts_with('L') && tok.ends_with(';'))
            {
                class_desc = Some(desc.to_string());
            }
            continue;
        }

        if line.starts_with(".method") {
            in_annotation = false;
            let cls = class_desc.as_deref().unwrap_or("<unknown_class>");
            let token = line
                .split_whitespace()
                .rev()
                .find(|tok| tok.contains('('))
                .context("failed to find method token in .method line")?;
            let paren = token.find('(').unwrap();
            let name = &token[..paren];
            let desc = &token[paren..];
            let is_static = line.split_whitespace().any(|t| t == "static");
            current_ins_size = method_ins_size_from_descriptor(desc, is_static);
            current_p_base = None;
            current_key = Some(format!("{cls}->{name}{desc}"));
            current_lines.clear();
            continue;
        }

        if line.starts_with(".annotation") {
            in_annotation = true;
            continue;
        }
        if line.starts_with(".end annotation") {
            in_annotation = false;
            continue;
        }

        if line.starts_with(".end method") {
            if let Some(k) = current_key.take() {
                out.insert(k, current_lines.clone());
            }
            current_lines.clear();
            current_p_base = None;
            current_ins_size = 0;
            continue;
        }

        if current_key.is_some() && !in_annotation {
            // Capture register allocation so we can map pX -> vN.
            if let Some(rest) = line.strip_prefix(".registers") {
                if let Ok(total_regs) = rest.trim().parse::<usize>() {
                    if total_regs >= current_ins_size {
                        current_p_base = Some(total_regs - current_ins_size);
                    }
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix(".locals") {
                if let Ok(locals) = rest.trim().parse::<usize>() {
                    let total_regs = locals + current_ins_size;
                    if total_regs >= current_ins_size {
                        current_p_base = Some(total_regs - current_ins_size);
                    }
                }
                continue;
            }

            if let Some(mut n) = normalize_baksmali_line(raw) {
                if let Some(p_base) = current_p_base {
                    n = rewrite_p_registers_to_v(&n, p_base);
                }
                current_lines.push(n);
            }
        }
    }

    Ok(out)
}

/// Normalize "our" smali lines (no offset prefix) for comparison with baksmali.
fn normalize_our_smali_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|s| normalize_line_common(s))
        .collect()
}

fn diff_preview(expected: &[String], actual: &[String]) -> String {
    let mut i = 0usize;
    while i < expected.len() && i < actual.len() && expected[i] == actual[i] {
        i += 1;
    }

    let start = i.saturating_sub(3);
    let end = (i + 3).min(expected.len().max(actual.len()));

    let mut out = String::new();
    out.push_str(&format!(
        "first difference at line {i} (expected_len={}, actual_len={})\n",
        expected.len(),
        actual.len()
    ));
    out.push_str("expected:\n");
    for (j, line) in expected.iter().enumerate().take(end).skip(start) {
        out.push_str(&format!("{j:04}: {line}\n"));
    }
    out.push_str("actual:\n");
    for (j, line) in actual.iter().enumerate().take(end).skip(start) {
        out.push_str(&format!("{j:04}: {line}\n"));
    }
    out
}
