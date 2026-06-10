#![allow(unused)] //TODO_JDB REMOVE THIS
use crate::error::Error;
use crate::facts as fx;

use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{FunctionSummary, IndexFacts, taint_index};
use crate::{
    codegen::{CallResolutionStrategy, RETURN_INDEX, codegen_program},
    languages::tree_sitter,
};
use anyhow::{Context, Result};
use ctadl_ir::graph::DirectedGraph;
use ctadl_ir::mir::{BasicBlockIdx, TerminatorKind};
use ctadl_ir::{Exp, Idx, StatementKind, VariableRef, ssa};
use ctadl_ir::{Program, ProgramInfo};
use std::path::{Path, PathBuf};

pub(crate) fn init_test_logging() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug) // This forces it to Debug
        .is_test(true)
        .try_init();
}

pub(crate) fn get_full_path(filename: &str) -> Result<PathBuf> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Now just append the folders from the crate root
    path.push("tests");
    path.push("c");
    path.push(filename);
    Ok(path)
}

/* Compile a program from a string. */
pub(crate) fn program_from_string(src: &str) -> (Program, String) {
    let result = tree_sitter::parse_c_program(src).expect("Failed to parse C program.");
    assert!(
        !result.1,
        "Input Program failed to parse without error from Tree-sitter"
    );
    // A block with no terminator is always a CFG defect, so fail loudly here rather
    // than let a test silently pass on a malformed control-flow graph.
    assert!(
        !result.2.contains("<no terminator>"),
        "Parsed IR contains a block with no terminator:\n{}",
        result.2
    );
    (result.0, result.2)
}

/* Compile a program from a string. */
pub(crate) fn program_from_string_no_check(src: &str) -> Program {
    program_from_string(src).0
}

/* Compile a program from a file. */
pub(crate) fn program_from_file<P: AsRef<Path>>(filename: P) -> Result<Program> {
    let path = filename.as_ref();

    // Read the file, and if it fails, attach a helpful message before returning
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to load source file: {}", path.display()))?;
    let program = tree_sitter::parse_c_program(&contents)?;
    Ok(program.0)
}

/* Common output for when tests fail. */
pub(crate) fn check_fail_str(prog_str: &str, msg: &str) {
    log::error!("TEST FAIL: {msg}");
    log::error!("\t{prog_str}");
}

pub(crate) fn check_fail(prog: &Program, msg: &str) {
    let prog_str = prog.to_string();
    check_fail_str(&prog_str, msg);
}

/* A test to check a particular program parsed N functions. */
pub(crate) fn check_function_count(prog: &Program, count: usize) -> bool {
    let len = prog.functions.len();
    if len != count {
        let err = format!("{} functions in parsed program, expected {}.", len, count);
        check_fail(prog, &err);
        return false;
    }
    true
}

/* Checks that count number of blocks exist in the program.
  Implicitly checks for the existance of only 1 functions.
*/
pub(crate) fn check_block_count(prog: &Program, count: usize) -> bool {
    if !check_function_count(prog, 1) {
        return false;
    }
    let Some(fun) = prog.functions.functions.raw.first() else {
        return false;
    };
    let len = fun.blocks.num_nodes();

    if len != count {
        let err = format!("{} blocks in parsed function, expected {}.", len, count);
        check_fail(prog, &err);
        return false;
    }
    true
}

pub(crate) fn debug_output_blocks(prog: &Program) {
    let Some(fun) = prog.functions.functions.raw.first() else {
        log::warn!("No functions in program");
        return;
    };
    //let mut idx = 0;
    for (idx, block) in fun.blocks.as_slice().into_iter().enumerate() {
        log::info!("BLOCK {}: {}", idx, block);
        //  idx += 1;
    }
}

/// A single basic block parsed out of the marked-up IR dump.
struct FlowBlock {
    id: usize,
    /// Inline flags such as "[start]" that follow the block id.
    flags: String,
    /// The `// ...` annotation comment attached to the block header.
    label: String,
    /// Non-terminator statements, in order.
    statements: Vec<String>,
    /// Block ids reached by the terminating `goto`.
    successors: Vec<usize>,
    /// The raw terminator text (e.g. "return %x", "<no terminator>").
    terminator: String,
}

/// Master switch for the test IR/CFG dumps. Flip to `false` to silence every
/// `dump_ir` call at once when the tests are settled.
const DUMP_IR: bool = true;

/// Test-only helper: log the raw IR dump and its ASCII block-flow diagram,
/// gated by [`DUMP_IR`]. Call this in every test that parses a C program so the
/// resulting control-flow graph can be eyeballed, then turn `DUMP_IR` off.
pub(crate) fn dump_ir(dump: &str) {
    if DUMP_IR {
        log::info!("IR dump:\n{dump}");
        log::info!("BCFG:{}", ascii_block_flow(dump));
    }
}

/// Parses a marked-up `ctadl-ir` dump (as produced by the test logging) and
/// renders an ASCII block-flow diagram: one box per basic block followed by
/// arrows to its `goto` successors.
pub(crate) fn ascii_block_flow(dump: &str) -> String {
    let blocks = parse_flow_blocks(dump);
    if blocks.is_empty() {
        return "\n(no basic blocks found in dump)\n".to_string();
    }
    render_flow_blocks(&blocks)
}

fn parse_flow_blocks(dump: &str) -> Vec<FlowBlock> {
    let mut blocks: Vec<FlowBlock> = Vec::new();
    let mut current: Option<FlowBlock> = None;

    for raw in dump.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("begin block_") {
            // rest looks like: `0 [start]: // initial_block` or `1: // **MISSING**`
            let (head, tail) = rest.split_once(':').unwrap_or((rest, ""));
            let mut head_parts = head.trim().splitn(2, char::is_whitespace);
            let id = head_parts
                .next()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let flags = head_parts.next().unwrap_or("").trim().to_string();
            let label = tail
                .trim()
                .strip_prefix("//")
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            current = Some(FlowBlock {
                id,
                flags,
                label,
                statements: Vec::new(),
                successors: Vec::new(),
                terminator: "<no terminator>".to_string(),
            });
        } else if line.starts_with("end block_") {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
        } else if let Some(b) = current.as_mut() {
            if let Some(targets) = line.strip_prefix("goto ") {
                b.successors = targets
                    .split(',')
                    .filter_map(|t| t.trim().parse::<usize>().ok())
                    .collect();
                b.terminator = line.to_string();
            } else if line.starts_with("return") || line == "<no terminator>" {
                b.terminator = line.to_string();
            } else if !line.is_empty() {
                b.statements.push(line.to_string());
            }
        }
    }

    blocks
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

/// A routed control-flow edge between two blocks.
struct FlowEdge {
    src: usize, // position index into `blocks`
    dst: usize,
    side: Side,
    src_row: usize, // canvas row where the edge leaves the source box
    dst_row: usize, // canvas row where the edge enters the destination box
    lane: usize,    // gutter lane (0 = nearest the boxes)
    succ_idx: usize, // which successor of the source block (== its `goto` line index)
}

fn block_header(b: &FlowBlock) -> String {
    if b.flags.is_empty() {
        format!("block_{}", b.id)
    } else {
        format!("block_{} {}", b.id, b.flags)
    }
}

/// The terminator section: one `goto block_<id>.<label>` line per successor
/// (named via `labels`), or the raw terminator text when there are none.
fn block_terminator_lines(
    b: &FlowBlock,
    labels: &std::collections::HashMap<usize, String>,
) -> Vec<String> {
    if b.successors.is_empty() {
        return vec![b.terminator.clone()];
    }
    b.successors
        .iter()
        .map(|s| match labels.get(s) {
            Some(label) if !label.is_empty() => format!("goto block_{s}.{label}"),
            _ => format!("goto block_{s}"),
        })
        .collect()
}

/// Renders one block as its box, a vector of equal-width lines (borders included).
fn block_box_lines(
    b: &FlowBlock,
    width: usize,
    labels: &std::collections::HashMap<usize, String>,
) -> Vec<String> {
    let border = format!("+{}+", "-".repeat(width + 2));
    let row = |content: &str| format!("| {content:<width$} |");
    let mut lines = vec![border.clone(), row(&block_header(b))];
    if !b.label.is_empty() {
        lines.push(row(&format!("// {}", b.label)));
    }
    lines.push(border.clone());
    if b.statements.is_empty() {
        lines.push(row("(no statements)"));
    } else {
        for s in &b.statements {
            lines.push(row(s));
        }
    }
    lines.push(border.clone());
    for term in block_terminator_lines(b, labels) {
        lines.push(row(&term));
    }
    lines.push(border);
    lines
}

/// Assigns gutter lanes on one side so that no two vertical segments overlap.
/// Uses interval partitioning: edges whose row spans are disjoint share a lane.
/// Returns the number of lanes used on that side.
fn assign_lanes(edges: &mut [FlowEdge], side: Side) -> usize {
    let mut order: Vec<usize> = (0..edges.len()).filter(|&i| edges[i].side == side).collect();
    order.sort_by_key(|&i| edges[i].src_row.min(edges[i].dst_row));

    let mut lane_end: Vec<usize> = Vec::new(); // last occupied row per lane
    for i in order {
        let lo = edges[i].src_row.min(edges[i].dst_row);
        let hi = edges[i].src_row.max(edges[i].dst_row);
        match lane_end.iter().position(|&end| end < lo) {
            Some(l) => {
                edges[i].lane = l;
                lane_end[l] = hi;
            }
            None => {
                edges[i].lane = lane_end.len();
                lane_end.push(hi);
            }
        }
    }
    lane_end.len()
}

/// Glyph drawn where a horizontal segment crosses a vertical one (distinct from
/// the '+' used for line corners/turns).
const CROSS: char = ')';

/// Draws `ch` at (r, c). A horizontal meeting a perpendicular vertical becomes a
/// crossing (`CROSS`); corners are plotted explicitly as '+' by the caller.
fn plot(grid: &mut [Vec<char>], r: usize, c: usize, ch: char) {
    let cur = grid[r][c];
    grid[r][c] = match (cur, ch) {
        (' ', _) => ch,
        ('|', '-') | ('-', '|') => CROSS,
        // A line drawn back through an existing corner or crossing keeps it.
        ('+', '-') | ('+', '|') | (CROSS, '-') | (CROSS, '|') => cur,
        _ => ch,
    };
}

fn render_flow_blocks(blocks: &[FlowBlock]) -> String {
    let pos_of: std::collections::HashMap<usize, usize> =
        blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
    let labels: std::collections::HashMap<usize, String> =
        blocks.iter().map(|b| (b.id, b.label.clone())).collect();

    // Uniform inner width so all boxes line up.
    let mut width = 0usize;
    for b in blocks {
        width = width.max(block_header(b).chars().count());
        if !b.label.is_empty() {
            width = width.max(b.label.chars().count() + 3); // "// "
        }
        for s in &b.statements {
            width = width.max(s.chars().count());
        }
        for term in block_terminator_lines(b, &labels) {
            width = width.max(term.chars().count());
        }
    }

    let box_lines: Vec<Vec<String>> = blocks
        .iter()
        .map(|b| block_box_lines(b, width, &labels))
        .collect();
    let box_w = width + 4; // "| " + content + " |"

    // Stack boxes vertically, one blank spacer row between them.
    const GAP: usize = 1;
    let mut box_top = vec![0usize; blocks.len()];
    let mut next = 0usize;
    for i in 0..blocks.len() {
        box_top[i] = next;
        next += box_lines[i].len() + GAP;
    }
    let total_rows = next.saturating_sub(GAP);

    // Build edges; forward edges route right, back-edges / self-loops route left.
    let mut edges: Vec<FlowEdge> = Vec::new();
    for (si, b) in blocks.iter().enumerate() {
        for (k, s) in b.successors.iter().enumerate() {
            if let Some(&di) = pos_of.get(s) {
                let side = if di > si { Side::Right } else { Side::Left };
                edges.push(FlowEdge {
                    src: si,
                    dst: di,
                    side,
                    src_row: 0,
                    dst_row: 0,
                    lane: 0,
                    succ_idx: k,
                });
            }
        }
    }

    // Assign port rows inside each block. Outgoing edges originate from their matching
    // `goto block_X` line; incoming edges land on the remaining content rows.
    for i in 0..blocks.len() {
        let top = box_top[i];
        let len = box_lines[i].len();
        // Terminator lines are the last `n_term` content rows before the bottom border,
        // one per successor and in successor order.
        let n_term = blocks[i].successors.len();
        let goto_lo = top + len - 1 - n_term; // row of `goto` line for successor 0
        let goto_hi = top + len - 2; // row of the last `goto` line

        // Each outgoing edge leaves from the `goto` line that names its target.
        for e in edges.iter_mut() {
            if e.src == i {
                e.src_row = goto_lo + e.succ_idx;
            }
        }

        // Incoming arrows land on content rows that aren't `goto` lines (so they don't
        // collide with the outgoing stubs); fall back to all content rows if needed.
        let mut dst_pool: Vec<usize> = Vec::new();
        for (li, line) in box_lines[i].iter().enumerate() {
            if !line.starts_with('|') {
                continue;
            }
            let r = top + li;
            if n_term == 0 || r < goto_lo || r > goto_hi {
                dst_pool.push(r);
            }
        }
        if dst_pool.is_empty() {
            for (li, line) in box_lines[i].iter().enumerate() {
                if line.starts_with('|') {
                    dst_pool.push(top + li);
                }
            }
        }
        for want_right in [false, true] {
            let mut k = 0usize;
            for e in edges.iter_mut() {
                if (e.side == Side::Right) != want_right {
                    continue;
                }
                if e.dst == i {
                    e.dst_row = dst_pool[k.min(dst_pool.len() - 1)];
                    k += 1;
                }
            }
        }
    }

    let max_left = assign_lanes(&mut edges, Side::Left);
    let max_right = assign_lanes(&mut edges, Side::Right);

    // Column layout: left gutter | boxes | right gutter.
    let box_left = if max_left > 0 { 2 * max_left + 1 } else { 0 };
    let box_right = box_left + box_w - 1;
    let canvas_w = box_right + 2 * max_right + 3;

    let mut grid = vec![vec![' '; canvas_w]; total_rows];

    // Stamp the boxes.
    for i in 0..blocks.len() {
        for (li, line) in box_lines[i].iter().enumerate() {
            let r = box_top[i] + li;
            for (ci, ch) in line.chars().enumerate() {
                grid[r][box_left + ci] = ch;
            }
        }
    }

    // Route the edges through the gutters.
    for e in &edges {
        let (sr, dr) = (e.src_row, e.dst_row);
        let (lo, hi) = (sr.min(dr), sr.max(dr));
        match e.side {
            Side::Right => {
                let lane = box_right + 2 + 2 * e.lane;
                for c in (box_right + 1)..lane {
                    plot(&mut grid, sr, c, '-');
                    plot(&mut grid, dr, c, '-');
                }
                plot(&mut grid, sr, lane, '+');
                plot(&mut grid, dr, lane, '+');
                for r in (lo + 1)..hi {
                    plot(&mut grid, r, lane, '|');
                }
                grid[dr][box_right + 1] = '<'; // arrowhead into the destination
            }
            Side::Left => {
                let lane = box_left - 2 - 2 * e.lane;
                for c in (lane + 1)..box_left {
                    plot(&mut grid, sr, c, '-');
                    plot(&mut grid, dr, c, '-');
                }
                plot(&mut grid, sr, lane, '+');
                plot(&mut grid, dr, lane, '+');
                for r in (lo + 1)..hi {
                    plot(&mut grid, r, lane, '|');
                }
                grid[dr][box_left - 1] = '>'; // arrowhead into the destination
            }
        }
    }

    let mut out = String::from("\n");
    for row in &grid {
        let line: String = row.iter().collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
/* This function is intended to run on Program objects generated by
parsing a single function and will search all blocks for a
particular assignment. */
pub(crate) fn check_assign<I>(
    prog: &Program,
    dst: &str,
    src_strs: I,
    block_id: Option<usize>,
) -> bool
where
    I: IntoIterator<Item = &'static str>,
{
    let srcs: Vec<Exp> = src_strs
        .into_iter()
        .map(|s| Exp::from(VariableRef::new_local(String::from(s))))
        .collect();
    let var = VariableRef::new_local(String::from(dst));
    let assign = StatementKind::assign(var, srcs);

    if !check_function_count(prog, 1) {
        return false;
    }
    let Some(fun) = prog.functions.functions.raw.first() else {
        return false;
    };
    //for block in fun.blocks.iter() {
    for (i, block) in fun.blocks.iter().enumerate() {
        // You can now check 'i' before proceeding
        if let Some(req_block) = block_id
            && i != req_block
        {
            continue;
        }
        for stmt in block.statements.iter() {
            if let StatementKind::Assign { .. } = &stmt.kind
                && stmt.kind == assign
            {
                return true;
            }
        }
    }
    let err = format!("Could not find '{}' in function {}.", assign, fun.name);
    check_fail(prog, &err);
    false
}

pub(crate) fn check_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        return true;
    }
    check_fail_str(prog_str, &format!("expected {}", needle));
    false
}

/// Inverse of [`check_match`]: passes (returns true) when `needle` is ABSENT, and
/// only logs a failure when it is unexpectedly present. Use this for negative
/// assertions so a passing test doesn't emit a misleading "expected ..." line.
pub(crate) fn check_no_match(prog_str: &str, needle: &str) -> bool {
    if prog_str.contains(needle) {
        check_fail_str(prog_str, &format!("did not expect {}", needle));
        return false;
    }
    true
}

pub(crate) fn get_summary(
    mut program_info: ProgramInfo,
) -> Result<(Vec<FunctionSummary>, IndexSourceInfo), Error> {
    program_info.program.verify()?;
    let mut facts = IndexFacts::default();
    ssa::transform_program(&mut program_info.program, true);
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
    ); //why is that mutable in codegen?
    let result = taint_index(facts);
    Ok((result.summary, source_info))
}

// Isn't it simpler to just assume we will only pass a summary of 1 function?
pub(crate) fn summary_returns_param(
    summary: &[FunctionSummary],
    source_info: &IndexSourceInfo,
    func_name: &str,
    param_num: i16,
) -> bool {
    let id = source_info
        .sites
        .get_function_id(fx::Function(func_name.into()))
        .unwrap();
    summary.iter().any(|r| {
        r.0 == id
            && r.1 == fx::FormalIndex::new(RETURN_INDEX)
            && r.3 == fx::FormalIndex::new(param_num)
    })
}

pub(crate) fn summary_count(summary: &[FunctionSummary], count: usize) -> bool {
    summary.len() == count
}

pub(crate) fn summary_search(
    summary: &[FunctionSummary],
    from_index: i16,
    from_path: &str,
    to_index: i16,
    to_path: &str,
) -> bool {
    summary.iter().any(|r| {
        r.1 == fx::FormalIndex::new(to_index)
            && r.2.to_string() == to_path
            && r.3 == fx::FormalIndex::new(from_index)
            && r.4.to_string() == from_path
    })
}
