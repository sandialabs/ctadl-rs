// This crate is vendored, and we do not maintain it in this repo, so we don't want to alert when
// running clippy.
#![allow(clippy::all)]
pub mod error;

pub mod parse_utils;

pub mod types;

pub mod parser;

pub mod instructions;

pub mod basic_blocks;

pub mod apk;

pub mod smali;

pub mod debug_info;

pub use apk::{APKParser, ApkClass, ApkMethod};
pub use debug_info::{
    collect_line_map_entries, compute_line_map, parse_debug_info, write_line_map_json,
};
pub use parser::DexParser;
pub use types::{
    DBG_FIRST_SPECIAL, DBG_LINE_BASE, DBG_LINE_RANGE, DebugInfoItem, DebugInfoOpcode, LineMapEntry,
    NO_INDEX, PositionEntry,
};
