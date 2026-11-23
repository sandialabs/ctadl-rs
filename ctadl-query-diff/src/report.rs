//! The result of a diff, and its two printers. Both print only what the [`Report`] holds, so the
//! text and the JSON cannot disagree.

use std::io::{self, Write};

use diff_report::{Samples, write_samples};
use serde::Serialize;

/// What two SARIF files differ in: results, execution notifications and invocation statuses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The two files, as given.
    pub a: String,
    pub b: String,
    /// Whether results of the endpoints rule were compared by their endpoints only.
    pub endpoints_only: bool,
    /// The most rows printed per side, for each of results, notifications and invocations.
    pub limit: usize,
    /// The results of every run of each file.
    pub results_a: usize,
    pub results_b: usize,
    /// Results A has more of than B, the first by their printed form.
    pub more_in_a: Samples,
    /// Results B has more of than A, the first by their printed form.
    pub more_in_b: Samples,
    /// The execution notifications of every invocation of each file.
    pub notifications_a: usize,
    pub notifications_b: usize,
    /// Notifications A has more of than B, the first by their printed form.
    pub notifications_more_in_a: Samples,
    /// Notifications B has more of than A, the first by their printed form.
    pub notifications_more_in_b: Samples,
    /// The invocations of every run of each file.
    pub invocations_a: usize,
    pub invocations_b: usize,
    /// Invocation statuses A has more of than B, the first by their printed form.
    pub invocations_more_in_a: Samples,
    /// Invocation statuses B has more of than A, the first by their printed form.
    pub invocations_more_in_b: Samples,
}

impl Report {
    /// Whether the two files' canonical results, notifications and statuses are each equal as
    /// multisets. Independent of `limit`, since a side's total counts the rows not shown.
    pub fn identical(&self) -> bool {
        self.results_differ() == 0
            && self.notifications_differ() == 0
            && self.invocations_differ() == 0
    }

    /// How many distinct canonical results the two files hold a different number of times.
    pub fn results_differ(&self) -> usize {
        self.more_in_a.total() + self.more_in_b.total()
    }

    /// How many distinct notifications the two files hold a different number of times.
    pub fn notifications_differ(&self) -> usize {
        self.notifications_more_in_a.total() + self.notifications_more_in_b.total()
    }

    /// How many distinct invocation statuses the two files hold a different number of times.
    pub fn invocations_differ(&self) -> usize {
        self.invocations_more_in_a.total() + self.invocations_more_in_b.total()
    }

    fn first_line(&self) -> &'static str {
        if self.identical() {
            "IDENTICAL"
        } else {
            "DIFFERENT"
        }
    }

    /// The report as text: the first line, the sides, the flag in force and the result
    /// counts, then the results each side has more of; then the same for notifications and
    /// for invocation statuses.
    pub fn write_text(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{}", self.first_line())?;
        writeln!(w, "A: {}", self.a)?;
        writeln!(w, "B: {}", self.b)?;
        if self.endpoints_only {
            writeln!(w, "flag: --endpoints-only")?;
        }
        writeln!(w, "results: A {}, B {}", self.results_a, self.results_b)?;
        write_samples(w, "A", &self.more_in_a, "results", "")?;
        write_samples(w, "B", &self.more_in_b, "results", "")?;
        writeln!(
            w,
            "notifications: A {}, B {}",
            self.notifications_a, self.notifications_b
        )?;
        write_samples(w, "A", &self.notifications_more_in_a, "notifications", "")?;
        write_samples(w, "B", &self.notifications_more_in_b, "notifications", "")?;
        writeln!(
            w,
            "invocations: A {}, B {}",
            self.invocations_a, self.invocations_b
        )?;
        write_samples(w, "A", &self.invocations_more_in_a, "invocations", "")?;
        write_samples(w, "B", &self.invocations_more_in_b, "invocations", "")
    }

    /// The report as one JSON document: its fields, after `result` (`IDENTICAL` or
    /// `DIFFERENT`), `results_differ`, `notifications_differ` and `invocations_differ`.
    pub fn write_json(&self, w: &mut impl Write) -> serde_json::Result<()> {
        #[derive(Serialize)]
        struct Json<'a> {
            result: &'a str,
            results_differ: usize,
            notifications_differ: usize,
            invocations_differ: usize,
            #[serde(flatten)]
            report: &'a Report,
        }
        serde_json::to_writer(
            &mut *w,
            &Json {
                result: self.first_line(),
                results_differ: self.results_differ(),
                notifications_differ: self.notifications_differ(),
                invocations_differ: self.invocations_differ(),
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
