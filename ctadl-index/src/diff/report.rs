//! The result of a diff, and its two printers. Both print only what the [`Report`] holds, so the
//! text and the JSON cannot disagree.

use std::io::{self, Write};

use serde::Serialize;

/// What two indexes differ in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The two directories, as given.
    pub a: String,
    pub b: String,
    /// The most rows printed per table and side.
    pub limit: usize,
    pub filters: Filters,
    /// `.parquet` files that no table is stored in, by name.
    pub not_compared: Vec<UnknownFile>,
    /// One per table present on either side, in name order.
    pub tables: Vec<TableReport>,
}

/// The filters the comparison was limited by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Filters {
    /// The tables compared, by name, sorted; empty for every table.
    pub tables: Vec<String>,
    /// Rows were limited to those of functions whose name contains this.
    pub function: Option<String>,
}

/// A `.parquet` file that is not compared, and the sides it is on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnknownFile {
    pub file: String,
    pub in_a: bool,
    pub in_b: bool,
}

/// One table's comparison. Counts are of canonical rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TableReport {
    pub table: String,
    /// Whether each side has the table at all.
    pub in_a: bool,
    pub in_b: bool,
    /// The rows compared on each side.
    pub rows_a: usize,
    pub rows_b: usize,
    /// Distinct rows on one side only.
    pub only_in_a: usize,
    pub only_in_b: usize,
    /// Distinct rows on both sides, a different number of times.
    pub count_differs: usize,
    /// Rows A has more of than B, the first by content.
    pub more_in_a: Samples,
    /// Rows B has more of than A, the first by content.
    pub more_in_b: Samples,
}

/// The first differing rows by content, and how many more there are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Samples {
    pub rows: Vec<Sample>,
    pub not_shown: usize,
}

/// A row, printed by content with its columns separated by tabs, and how many times each side
/// holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub row: String,
    pub count_a: usize,
    pub count_b: usize,
}

impl Samples {
    /// How many rows differ this way, shown or not.
    pub fn total(&self) -> usize {
        self.rows.len() + self.not_shown
    }
}

impl TableReport {
    /// Whether the table differs: present on one side only, or holding different rows.
    pub fn differs(&self) -> bool {
        self.in_a != self.in_b
            || self.only_in_a != 0
            || self.only_in_b != 0
            || self.count_differs != 0
    }
}

impl Report {
    /// Whether no table differs. A file that was not compared is not a difference.
    pub fn identical(&self) -> bool {
        !self.tables.iter().any(TableReport::differs)
    }

    /// The tables that differ, in name order.
    pub fn differing(&self) -> impl Iterator<Item = &TableReport> {
        self.tables.iter().filter(|t| t.differs())
    }

    /// `IDENTICAL`, or `DIFFERENT: ...` naming how many tables differ and which.
    pub fn first_line(&self) -> String {
        if self.identical() {
            return "IDENTICAL".to_string();
        }
        let names: Vec<&str> = self.differing().map(|t| t.table.as_str()).collect();
        format!(
            "DIFFERENT: {} of {} tables differ: {}",
            names.len(),
            self.tables.len(),
            names.join(", ")
        )
    }

    /// The report as text: the first line, the sides and filters, the files not compared,
    /// then a line per table, each differing one followed by its samples.
    pub fn write_text(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{}", self.first_line())?;
        writeln!(w, "A: {}", self.a)?;
        writeln!(w, "B: {}", self.b)?;
        if !self.filters.tables.is_empty() {
            writeln!(w, "filter: tables {}", self.filters.tables.join(", "))?;
        }
        if let Some(f) = &self.filters.function {
            writeln!(w, "filter: functions whose name contains {f:?}")?;
        }
        for u in &self.not_compared {
            writeln!(
                w,
                "not compared, no table is stored in it: {} in {}",
                u.file,
                sides(u.in_a, u.in_b)
            )?;
        }
        for t in &self.tables {
            if !t.differs() {
                writeln!(w, "{}: same, {} rows", t.table, t.rows_a)?;
                continue;
            }
            let status = match (t.in_a, t.in_b) {
                (true, false) => "only in A",
                (false, true) => "only in B",
                _ => "DIFFERENT",
            };
            writeln!(
                w,
                "{}: {status}, A {}, B {}; {} only in A, {} only in B, {} in both with different counts",
                t.table,
                rows(t.in_a, t.rows_a),
                rows(t.in_b, t.rows_b),
                t.only_in_a,
                t.only_in_b,
                t.count_differs
            )?;
            for (side, samples) in [("A", &t.more_in_a), ("B", &t.more_in_b)] {
                if samples.total() == 0 {
                    continue;
                }
                writeln!(
                    w,
                    "  more in {side}: {} rows, {} not shown",
                    samples.total(),
                    samples.not_shown
                )?;
                for s in &samples.rows {
                    writeln!(w, "    [A {}, B {}] {}", s.count_a, s.count_b, s.row)?;
                }
            }
        }
        Ok(())
    }

    /// The report as one JSON document: its fields, after `result` (`IDENTICAL` or
    /// `DIFFERENT`) and `tables_differ`.
    pub fn write_json(&self, w: &mut impl Write) -> serde_json::Result<()> {
        #[derive(Serialize)]
        struct Json<'a> {
            result: &'a str,
            tables_differ: usize,
            #[serde(flatten)]
            report: &'a Report,
        }
        serde_json::to_writer(
            &mut *w,
            &Json {
                result: if self.identical() {
                    "IDENTICAL"
                } else {
                    "DIFFERENT"
                },
                tables_differ: self.differing().count(),
                report: self,
            },
        )?;
        writeln!(w).map_err(serde_json::Error::io)
    }

    /// [`Report::write_text`] into a string.
    pub fn to_text(&self) -> String {
        let mut out = Vec::new();
        self.write_text(&mut out).expect("writing to a Vec");
        String::from_utf8(out).expect("the report is UTF-8")
    }

    /// [`Report::write_json`] into a string.
    pub fn to_json(&self) -> String {
        let mut out = Vec::new();
        self.write_json(&mut out).expect("writing to a Vec");
        String::from_utf8(out).expect("the report is UTF-8")
    }
}

fn sides(a: bool, b: bool) -> &'static str {
    match (a, b) {
        (true, true) => "A and B",
        (true, false) => "A",
        _ => "B",
    }
}

fn rows(present: bool, n: usize) -> String {
    if present {
        format!("{n} rows")
    } else {
        "no table".to_string()
    }
}
