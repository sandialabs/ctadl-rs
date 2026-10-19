//! Cross-checks dex-reader's per-opcode facts against dexlib2's opcode table.
//!
//! `tests/data/dexlib2-opcodes.tsv` is a dump of dexlib2's `Opcode` enum (the library inside
//! baksmali), made by `scripts/DumpDexlib2Opcodes.java`. For each of the 256 opcode bytes this test
//! decodes a representative instruction and compares it with the dump's row: mnemonic, format,
//! size, reference kinds, which registers it writes, whether it can throw, whether it produces a
//! result for `move-result`, and whether it can fall through.
//!
//! dexlib2's flags describe writes only, so this says nothing about the registers an instruction
//! reads (`DataFlow::source`). `cargo xtask dex-opcodes` checks those against ART.

use dex_reader::instructions::{DALVIK_OPCODES, Format, Instruction};

const DUMP: &str = include_str!("data/dexlib2-opcodes.tsv");

/// Opcodes where dex-reader deliberately disagrees with dexlib2, with the field and the reason.
const KNOWN_DIVERGENCES: &[(&str, &str, &str)] = &[(
    "fill-array-data",
    "can_throw",
    "dexlib2 says it cannot throw, but it throws NullPointerException on a null array and \
     ArrayIndexOutOfBoundsException when the payload is longer than the array. ART's own table \
     (dex_instruction_list.h) marks it kThrow.",
)];

struct Row {
    value: u8,
    name: String,
    format: String,
    size: usize,
    refs: (String, String),
    can_throw: bool,
    can_continue: bool,
    sets_result: bool,
    sets_register: bool,
    sets_wide_register: bool,
}

fn parse_dump() -> Vec<Option<Row>> {
    let mut lines = DUMP.lines().filter(|l| !l.starts_with('#'));
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    assert_eq!(
        header,
        [
            "value",
            "name",
            "format",
            "size",
            "ref",
            "ref2",
            "can_throw",
            "can_continue",
            "sets_result",
            "sets_register",
            "sets_wide_register"
        ]
    );
    let rows: Vec<Option<Row>> = lines
        .map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            assert_eq!(c.len(), header.len(), "malformed dump line: {line}");
            let value = u8::from_str_radix(c[0].trim_start_matches("0x"), 16).unwrap();
            if c[1] == "unused" {
                return None;
            }
            let flag = |s: &str| match s {
                "0" => false,
                "1" => true,
                _ => panic!("bad flag {s:?} in {line}"),
            };
            Some(Row {
                value,
                name: c[1].to_string(),
                format: c[2].to_string(),
                size: c[3].parse().unwrap(),
                refs: (c[4].to_string(), c[5].to_string()),
                can_throw: flag(c[6]),
                can_continue: flag(c[7]),
                sets_result: flag(c[8]),
                sets_register: flag(c[9]),
                sets_wide_register: flag(c[10]),
            })
        })
        .collect();
    assert_eq!(
        rows.len(),
        256,
        "the dump must have one row per opcode byte"
    );
    for (i, row) in rows.iter().enumerate() {
        if let Some(row) = row {
            assert_eq!(
                usize::from(row.value),
                i,
                "dump rows must be in opcode order"
            );
        }
    }
    rows
}

/// Code units for opcode `op` with every operand field nonzero and distinct, long enough for the
/// widest format. Register A is 2 (or 0x52 in 8-bit fields); 35c/45cc use all five registers.
fn representative(op: u8) -> [u16; 5] {
    [u16::from(op) | 0x5200, 0x0007, 0x4310, 0x0009, 0x000b]
}

/// dexlib2's name for this instruction's format (without the `Format` prefix), and the kinds of
/// its first and second references.
fn format_info(inst: &Instruction) -> (&'static str, &'static str, &'static str) {
    use Format::*;
    match inst.format() {
        Format10t(_) => ("10t", "none", "none"),
        Format10x(_) => ("10x", "none", "none"),
        Format11n(_) => ("11n", "none", "none"),
        Format11x(_) => ("11x", "none", "none"),
        Format12x(_) => ("12x", "none", "none"),
        Format20t(_) => ("20t", "none", "none"),
        Format21cF(_) => ("21c", "field", "none"),
        Format21cM(_) => ("21c", "method", "none"),
        Format21cH(_) => ("21c", "method_handle", "none"),
        Format21cP(_) => ("21c", "proto", "none"),
        Format21cS(_) => ("21c", "string", "none"),
        Format21cT(_) => ("21c", "type", "none"),
        // dexlib2 splits 21h by the width of the destination; dex-reader does not.
        Format21h(_) => match inst {
            Instruction::ConstWideHigh16(_) => ("21lh", "none", "none"),
            _ => ("21ih", "none", "none"),
        },
        Format21s(_) => ("21s", "none", "none"),
        Format21t(_) => ("21t", "none", "none"),
        Format22b(_) => ("22b", "none", "none"),
        Format22cF(_) => ("22c", "field", "none"),
        Format22cT(_) => ("22c", "type", "none"),
        Format22s(_) => ("22s", "none", "none"),
        Format22t(_) => ("22t", "none", "none"),
        Format22x(_) => ("22x", "none", "none"),
        Format23x(_) => ("23x", "none", "none"),
        Format30t(_) => ("30t", "none", "none"),
        Format31c(_) => ("31c", "string", "none"),
        Format31i(_) => ("31i", "none", "none"),
        Format31t(_) => ("31t", "none", "none"),
        Format32x(_) => ("32x", "none", "none"),
        Format35cC(_) => ("35c", "call_site", "none"),
        Format35cM(_) => ("35c", "method", "none"),
        Format35cT(_) => ("35c", "type", "none"),
        Format3rcC(_) => ("3rc", "call_site", "none"),
        Format3rcM(_) => ("3rc", "method", "none"),
        Format3rcT(_) => ("3rc", "type", "none"),
        Format45cc(_) => ("45cc", "method", "proto"),
        Format4rcc(_) => ("4rcc", "method", "proto"),
        Format51l(_) => ("51l", "none", "none"),
        FormatUnused(_) => ("unused", "none", "none"),
    }
}

fn is_known_divergence(name: &str, field: &str) -> bool {
    KNOWN_DIVERGENCES
        .iter()
        .any(|(n, f, _)| *n == name && *f == field)
}

#[test]
fn opcode_table_agrees_with_dexlib2() {
    let rows = parse_dump();
    let mut mismatches = Vec::new();
    let mut divergences_seen = Vec::new();

    for (value, row) in rows.iter().enumerate() {
        let op = value as u8;
        let units = representative(op);
        let (size, inst) = Instruction::new(&units);
        let ours_name = DALVIK_OPCODES[value];
        let (format, ref1, ref2) = format_info(&inst);
        let mut check = |field: &str, ours: String, theirs: String| {
            if ours == theirs {
                return;
            }
            let name = row.as_ref().map_or(ours_name, |r| r.name.as_str());
            if is_known_divergence(name, field) {
                divergences_seen.push((name.to_string(), field.to_string()));
            } else {
                mismatches.push(format!(
                    "0x{op:02x} {name}: {field}: dex-reader {ours}, dexlib2 {theirs}"
                ));
            }
        };

        let Some(row) = row else {
            check("format", format.to_string(), "unused".to_string());
            continue;
        };

        let df = inst.data_flow();
        let first_reg = match inst.format() {
            Format::Format32x(f) => f.a.0,
            _ if matches!(format, "12x" | "11n" | "22c" | "22s" | "22t") => 2,
            _ => 0x52,
        };

        check("name", ours_name.to_string(), row.name.clone());
        check("format", format.to_string(), row.format.clone());
        check("size", size.to_string(), row.size.to_string());
        check("ref", ref1.to_string(), row.refs.0.clone());
        check("ref2", ref2.to_string(), row.refs.1.clone());
        check(
            "can_throw",
            inst.can_throw().to_string(),
            row.can_throw.to_string(),
        );
        check(
            "sets_register",
            (!df.dest.is_empty()).to_string(),
            row.sets_register.to_string(),
        );
        check(
            "sets_wide_register",
            (df.dest.len() == 2).to_string(),
            row.sets_wide_register.to_string(),
        );
        if row.sets_register {
            // The written register is always vA, and a wide write takes vA and vA+1.
            let want: Vec<u32> = if row.sets_wide_register {
                vec![first_reg, first_reg + 1]
            } else {
                vec![first_reg]
            };
            check(
                "dest",
                format!("{:?}", df.dest.iter().map(|r| r.0).collect::<Vec<_>>()),
                format!("{want:?}"),
            );
        }
        // dexlib2's SETS_RESULT marks the instructions a move-result may follow: every invoke,
        // plus filled-new-array.
        let filled_new_array = matches!(
            inst,
            Instruction::FilledNewArray(_) | Instruction::FilledNewArrayRange(_)
        );
        check(
            "sets_result",
            (inst.is_invoke() || filled_new_array).to_string(),
            row.sets_result.to_string(),
        );
        check(
            "call_args",
            inst.call_args().is_some().to_string(),
            inst.is_invoke().to_string(),
        );
        // An instruction that cannot fall through must end its block. Among the rest,
        // is_control_flow() holds exactly for branches and switches.
        let branch_or_switch = matches!(format, "21t" | "22t")
            || matches!(
                inst,
                Instruction::PackedSwitch(_) | Instruction::SparseSwitch(_)
            );
        check(
            "is_control_flow",
            inst.is_control_flow().to_string(),
            (!row.can_continue || branch_or_switch).to_string(),
        );
    }

    for (name, field, _) in KNOWN_DIVERGENCES {
        if !divergences_seen.contains(&(name.to_string(), field.to_string())) {
            mismatches.push(format!(
                "{name}: {field}: listed in KNOWN_DIVERGENCES but agrees with dexlib2; remove it"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "dex-reader disagrees with dexlib2 on {} facts:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
