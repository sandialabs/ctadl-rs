//! Compares the output tables of two index runs as sets of rows, by content.
//!
//! Each row is rendered as `CTADL_DUMP_RELATIONS` renders it, so a function is its name and a
//! path its text, never an intern id. Both runs' functions are named from one [`IdMap`]: the
//! runs compared are of the same facts. The tables are compared one at a time, so at most one
//! table of each run is held rendered.

use std::cmp::Ordering;
use std::fmt;
use std::path::Path;

use crate::error::Error;
use crate::facts::IdMap;
use crate::index_engine::IndexResult;
use crate::index_engine::dump::{OutputTable, Outputs};

/// How many differing rows of each side a [`TableDiff`] keeps.
pub const SHOWN_ROWS: usize = 10;

/// The rows of one table that only one run has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDiff {
    pub table: OutputTable,
    /// How many distinct rows only the left run has.
    pub left_only: usize,
    /// How many distinct rows only the right run has.
    pub right_only: usize,
    /// The first [`SHOWN_ROWS`] rows only the left run has, in sorted order.
    pub left_rows: Vec<String>,
    /// The first [`SHOWN_ROWS`] rows only the right run has, in sorted order.
    pub right_rows: Vec<String>,
}

/// The tables on which two runs differ, in [`OutputTable::ALL`] order. Empty when the runs agree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputDiff {
    pub tables: Vec<TableDiff>,
}

impl OutputDiff {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }
}

/// Compares the output tables of two results.
pub fn compare_results(
    left: &IndexResult,
    right: &IndexResult,
    id_map: Option<&IdMap>,
) -> OutputDiff {
    compare(Outputs::Result(left), Outputs::Result(right), id_map)
        .expect("a result in memory reads without error")
}

/// Compares the output tables of two index directories.
pub fn compare_index_dirs(
    left: &Path,
    right: &Path,
    id_map: Option<&IdMap>,
) -> Result<OutputDiff, Error> {
    compare(Outputs::Dir(left), Outputs::Dir(right), id_map)
}

fn compare(
    left: Outputs<'_>,
    right: Outputs<'_>,
    id_map: Option<&IdMap>,
) -> Result<OutputDiff, Error> {
    let mut tables = Vec::new();
    for table in OutputTable::ALL {
        let left = rendered(table, left, id_map)?;
        let right = rendered(table, right, id_map)?;
        if let Some(diff) = diff_sorted(table, &left, &right) {
            tables.push(diff);
        }
    }
    Ok(OutputDiff { tables })
}

/// The distinct rows of `table`, rendered and sorted.
fn rendered(
    table: OutputTable,
    from: Outputs<'_>,
    id_map: Option<&IdMap>,
) -> Result<Vec<String>, Error> {
    let mut rows = Vec::new();
    table.for_each_row(from, id_map, |row| rows.push(row.to_string()))?;
    rows.sort_unstable();
    rows.dedup();
    Ok(rows)
}

/// Walks two sorted, distinct row lists in step and collects what only one side has.
fn diff_sorted(table: OutputTable, left: &[String], right: &[String]) -> Option<TableDiff> {
    let mut diff = TableDiff {
        table,
        left_only: 0,
        right_only: 0,
        left_rows: Vec::new(),
        right_rows: Vec::new(),
    };
    let keep = |count: &mut usize, rows: &mut Vec<String>, row: &String| {
        *count += 1;
        if rows.len() < SHOWN_ROWS {
            rows.push(row.clone());
        }
    };
    let (mut l, mut r) = (left.iter().peekable(), right.iter().peekable());
    loop {
        let left_first = match (l.peek(), r.peek()) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => match a.cmp(b) {
                Ordering::Equal => {
                    l.next();
                    r.next();
                    continue;
                }
                order => order == Ordering::Less,
            },
        };
        if left_first {
            keep(&mut diff.left_only, &mut diff.left_rows, l.next().unwrap());
        } else {
            keep(
                &mut diff.right_only,
                &mut diff.right_rows,
                r.next().unwrap(),
            );
        }
    }
    (diff.left_only + diff.right_only > 0).then_some(diff)
}

impl fmt::Display for OutputDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.tables.is_empty() {
            return writeln!(f, "the output tables agree");
        }
        for t in &self.tables {
            writeln!(
                f,
                "{}: left only {}, right only {}",
                t.table.name(),
                t.left_only,
                t.right_only
            )?;
            for row in &t.left_rows {
                writeln!(f, "  < {row}")?;
            }
            if t.left_only > t.left_rows.len() {
                writeln!(f, "  < ...")?;
            }
            for row in &t.right_rows {
                writeln!(f, "  > {row}")?;
            }
            if t.right_only > t.right_rows.len() {
                writeln!(f, "  > ...")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_engine::{IndexConfig, taint_index_with_config};
    use crate::index_engine_tests::{SRC, index_program, program_from_string};

    fn two_runs() -> (IndexResult, IndexResult, IdMap) {
        let (program, _) = program_from_string(SRC);
        let (facts, source_info) = index_program(program);
        let id_map = source_info.sites;
        let config = IndexConfig::default();
        let a = taint_index_with_config(facts.clone(), config.clone(), Some(&id_map));
        let b = taint_index_with_config(facts, config, Some(&id_map));
        (a, b, id_map)
    }

    #[test]
    fn two_monolithic_runs_agree() {
        let (a, b, id_map) = two_runs();
        assert!(!a.summary.is_empty() && !a.assign_like.is_empty());
        let diff = compare_results(&a, &b, Some(&id_map));
        assert!(diff.is_empty(), "{diff}");

        let (left, right) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        a.try_save(left.path()).unwrap();
        b.try_save(right.path()).unwrap();
        let diff = compare_index_dirs(left.path(), right.path(), Some(&id_map)).unwrap();
        assert!(diff.is_empty(), "{diff}");
    }

    #[test]
    fn a_dropped_row_names_its_table() {
        let (a, mut b, id_map) = two_runs();
        let dropped = b.summary.pop().unwrap();
        // Order and duplicates do not count.
        b.assign_like.reverse();
        b.assign_like.push(b.assign_like[0]);

        let diff = compare_results(&a, &b, Some(&id_map));
        assert_eq!(diff.tables.len(), 1, "{diff}");
        let t = &diff.tables[0];
        assert_eq!(t.table, OutputTable::Summary);
        assert_eq!((t.left_only, t.right_only), (1, 0));
        let name = crate::index_engine::func_name(Some(&id_map), dropped.0);
        assert!(t.left_rows[0].starts_with(&format!("{name}\t")), "{diff}");
        let shown = diff.to_string();
        assert!(
            shown.starts_with("summary: left only 1, right only 0\n  < "),
            "{shown}"
        );

        let (left, right) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        a.try_save(left.path()).unwrap();
        b.try_save(right.path()).unwrap();
        let from_dirs = compare_index_dirs(left.path(), right.path(), Some(&id_map)).unwrap();
        assert_eq!(from_dirs, diff);
    }
}
