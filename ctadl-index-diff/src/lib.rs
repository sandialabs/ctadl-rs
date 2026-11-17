//! Compares two index directories table by table, as multisets of canonical rows.
//!
//! A canonical row is a table's row with everything that numbers things per run replaced by
//! what it names: a function id by the function's name, an import id by the import's name, an
//! instruction id by its canonical instruction ([`CanonInsn`]), and a span id dropped. Two
//! tables are equal when they hold the same canonical rows, each the same number of times, in
//! any order.
//!
//! An instruction is named by its function and its input facts ([`CanonInsn`]); instructions
//! of one function with the same facts share a name, and the diff counts how many rows
//! collapse onto it. Which instructions share a signature is computed by
//! [`name_by_facts::Names`], proved with Verus to give two instructions, on either side, one
//! signature exactly when they have one set of facts.
//!
//! Each side is sorted by intern id, which is consistent across the two sides because both are
//! loaded into this one process, and the two are compared with
//! [`multiset_diff`](multiset_diff::multiset_diff). Only the sampled differences are ordered by
//! content, for printing. One table pair is loaded at a time and freed before the next.

mod canon;
mod driver;
mod insn;
mod names;
mod render;
mod report;
mod table;
#[cfg(test)]
mod tests;

use std::fmt;

pub use canon::{CanonEdge, CanonEndpoint, CanonTarget, CanonVar, FnName};
pub use driver::{Options, diff_index};
pub use insn::{CanonInsn, SigRow, SigVar, SignatureId, Signatures};
pub use report::{Filters, Report, Sample, Samples, TableReport, UnknownFile};

/// One of the two index directories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::A => "A",
            Side::B => "B",
        })
    }
}

/// Why two indexes could not be compared: a table that does not load, or a row that cannot be
/// made canonical. The message names the table and the side.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// `e` and its sources, joined by `: `.
pub(crate) fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    msg
}
