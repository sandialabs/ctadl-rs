//! Compares two SARIF files written by `ctadl query`, by content: their results, their
//! execution notifications and their invocations' statuses.
//!
//! The parser reads each file into `sarif_canon`'s raw runs, over atoms that one [`Atoms`]
//! table shares between both files, keeping only the fields the comparison uses.
//! `sarif_canon` turns those into canonical results, sorted by its verified order, and
//! [`multiset_diff`](multiset_diff::multiset_diff) compares the two sorted multisets under that
//! same order. Only the sampled differences are ordered by their printed form. The same parse
//! gives each notification and each status as one canonical JSON string, and those are
//! compared as sorted multisets of strings.

mod atoms;
mod driver;
mod parse;
mod render;
mod report;
#[cfg(test)]
mod tests;

use std::fmt;
use std::path::PathBuf;

pub use atoms::Atoms;
pub use diff_report::{Sample, Samples};
pub use driver::{ENDPOINTS_RULE, Options, diff_query};
pub use parse::{Error as ParseError, Parsed, read_runs};
pub use report::Report;
pub use sarif_canon::types::Error as CanonError;

/// Why two SARIF files could not be compared: one of them could not be read, or has a result
/// with no canonical form.
#[derive(Debug)]
pub struct Error {
    /// The file the error is about, as given.
    pub file: PathBuf,
    pub kind: ErrorKind,
}

#[derive(Debug)]
pub enum ErrorKind {
    /// The file is unreadable, not SARIF, or holds a property of the wrong shape.
    Parse(ParseError),
    /// A result cannot be made canonical.
    Canon(CanonError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.file.display())?;
        match &self.kind {
            ErrorKind::Parse(e) => write!(f, "{e}"),
            ErrorKind::Canon(CanonError::OutOfRange) => f.write_str(
                "a logical location index is past the end of the run's `logicalLocations`",
            ),
            ErrorKind::Canon(CanonError::NotIncreasing) => {
                f.write_str("a thread flow's `executionOrder` values do not increase")
            }
            ErrorKind::Canon(CanonError::EmptyFlow) => f.write_str("a thread flow has no steps"),
        }
    }
}

// The message already includes the parse error's own cause, so there is no `source`.
impl std::error::Error for Error {}
