//! Compares the results of two SARIF files written by `ctadl query`, by content.
//!
//! The parser reads each file into `sarif_canon`'s raw runs, over atoms that one [`Atoms`]
//! table shares between both files, keeping only the fields the comparison uses.
//! `sarif_canon` turns those into canonical results, which are what is compared.

mod atoms;
mod parse;

pub use atoms::Atoms;
pub use parse::{Error, read_runs};
