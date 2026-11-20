//! The result of a diff, and its two printers. Both print only what the [`Report`] holds, so the
//! text and the JSON cannot disagree.

use std::io::{self, Write};

use diff_report::{Samples, write_samples};
use serde::Serialize;

/// What the results of two SARIF files differ in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The two files, as given.
    pub a: String,
    pub b: String,
    /// Whether results of the endpoints rule were compared by their endpoints only.
    pub endpoints_only: bool,
    /// The most results printed per side.
    pub limit: usize,
    /// The results of every run of each file.
    pub results_a: usize,
    pub results_b: usize,
    /// Results A has more of than B, the first by their printed form.
    pub more_in_a: Samples,
    /// Results B has more of than A, the first by their printed form.
    pub more_in_b: Samples,
}

impl Report {
    /// Whether the two multisets of canonical results are equal. Independent of `limit`,
    /// since a side's total counts the results not shown.
    pub fn identical(&self) -> bool {
        self.results_differ() == 0
    }

    /// How many distinct canonical results the two files hold a different number of times.
    pub fn results_differ(&self) -> usize {
        self.more_in_a.total() + self.more_in_b.total()
    }

    fn first_line(&self) -> &'static str {
        if self.identical() {
            "IDENTICAL"
        } else {
            "DIFFERENT"
        }
    }

    /// The report as text: the first line, the sides, the flag in force and the result
    /// counts, then the results each side has more of.
    pub fn write_text(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{}", self.first_line())?;
        writeln!(w, "A: {}", self.a)?;
        writeln!(w, "B: {}", self.b)?;
        if self.endpoints_only {
            writeln!(w, "flag: --endpoints-only")?;
        }
        writeln!(w, "results: A {}, B {}", self.results_a, self.results_b)?;
        write_samples(w, "A", &self.more_in_a, "results", "")?;
        write_samples(w, "B", &self.more_in_b, "results", "")
    }

    /// The report as one JSON document: its fields, after `result` (`IDENTICAL` or
    /// `DIFFERENT`) and `results_differ`.
    pub fn write_json(&self, w: &mut impl Write) -> serde_json::Result<()> {
        #[derive(Serialize)]
        struct Json<'a> {
            result: &'a str,
            results_differ: usize,
            #[serde(flatten)]
            report: &'a Report,
        }
        serde_json::to_writer(
            &mut *w,
            &Json {
                result: self.first_line(),
                results_differ: self.results_differ(),
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
