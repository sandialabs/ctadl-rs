//! The pest-generated parser for the stable bytecode text format.

#[derive(pest_derive::Parser)]
#[grammar = "grammar.pest"]
pub struct BytecodeParser;
