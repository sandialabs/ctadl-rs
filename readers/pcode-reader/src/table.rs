//! Reading one fact table: decompressing it whole, then parsing its rows, in parallel for a big
//! table.
//!
//! A table is Ghidra's tab-separated text with no header. Rows are parsed the way the `csv`
//! crate parses them (empty lines are skipped, a field may be quoted), and every row must have
//! the table's number of fields. Rows come back in file order, which callers rely on wherever a
//! later row overrides an earlier one or rows are collected into a list.

use std::io::Read;
use std::path::Path;

use rayon::prelude::*;

use crate::{PcodeError, Result};

/// How many bytes of a table one parallel task parses. Big enough that a task's overhead is
/// noise, small enough that the largest tables split across every thread.
const CHUNK: usize = 1 << 20;

/// The whole of `path`, inflated when `compressed`.
pub(crate) fn load(path: &Path, compressed: bool) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata().map_or(0, |m| m.len() as usize);
    let mut bytes = Vec::new();
    if compressed {
        // Fact text compresses about tenfold; reserving for that saves most of the regrowth.
        bytes.reserve(len.saturating_mul(10));
        flate2::read::MultiGzDecoder::new(std::io::BufReader::new(file)).read_to_end(&mut bytes)?;
    } else {
        bytes.reserve(len);
        file.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}

/// Each row of `table` (named `filename`, for errors), which must have `N` fields, mapped
/// through `f`, in file order.
pub(crate) fn parse<const N: usize, T, F>(filename: &str, table: &[u8], f: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn([&str; N]) -> Result<T> + Sync,
{
    // A quote may start a quoted field, which can hold a tab or a newline, and a carriage return
    // ends a row too. Fact tables have neither, but when one does, the `csv` crate reads it.
    if memchr::memchr2(b'"', b'\r', table).is_some() {
        return parse_csv(filename, table, f);
    }
    if table.len() <= CHUNK {
        return parse_lines(filename, table, &f);
    }
    let chunks = split_lines(table, CHUNK);
    let parsed: Vec<Vec<T>> = chunks
        .par_iter()
        .map(|chunk| parse_lines(filename, chunk, &f))
        .collect::<Result<_>>()?;
    Ok(parsed.into_iter().flatten().collect())
}

/// `table` cut into pieces of about `size` bytes that each end at the end of a line.
fn split_lines(table: &[u8], size: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::with_capacity(table.len() / size + 1);
    let mut rest = table;
    while rest.len() > size {
        let end = match memchr::memchr(b'\n', &rest[size..]) {
            Some(i) => size + i + 1,
            None => rest.len(),
        };
        let (chunk, tail) = rest.split_at(end);
        chunks.push(chunk);
        rest = tail;
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

/// The rows of `text`, which has no quotes or carriage returns: lines split at tabs.
fn parse_lines<const N: usize, T>(
    filename: &str,
    text: &[u8],
    f: &impl Fn([&str; N]) -> Result<T>,
) -> Result<Vec<T>> {
    let text = std::str::from_utf8(text)
        .map_err(|e| PcodeError::invalid_fact_format(filename, e.to_string()))?;
    let mut rows = Vec::with_capacity(text.len() / 32);
    for line in text.split('\n') {
        if line.is_empty() {
            continue;
        }
        let mut fields = [""; N];
        let mut parts = line.split('\t');
        for field in &mut fields {
            *field = parts.next().ok_or_else(|| wrong_width(filename, N, line))?;
        }
        if parts.next().is_some() {
            return Err(wrong_width(filename, N, line));
        }
        rows.push(f(fields)?);
    }
    Ok(rows)
}

/// The rows of `table` as the `csv` crate reads them.
fn parse_csv<const N: usize, T>(
    filename: &str,
    table: &[u8],
    f: impl Fn([&str; N]) -> Result<T>,
) -> Result<Vec<T>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .delimiter(b'\t')
        .from_reader(table);
    let mut record = csv::StringRecord::new();
    let mut rows = Vec::new();
    while reader
        .read_record(&mut record)
        .map_err(|e| PcodeError::csv_parse_error(filename, e))?
    {
        if record.len() != N {
            return Err(wrong_width(filename, N, &format!("{record:?}")));
        }
        let mut fields = [""; N];
        for (field, value) in fields.iter_mut().zip(record.iter()) {
            *field = value;
        }
        rows.push(f(fields)?);
    }
    Ok(rows)
}

fn wrong_width(filename: &str, n: usize, row: &str) -> PcodeError {
    PcodeError::invalid_fact_format(filename, format!("expected {n} fields in row: {row:?}"))
}

/// An integer field. An empty field is 0, as it was when tables were read as Arrow columns,
/// where an empty field is null and a null integer reads as 0.
pub(crate) fn int(filename: &str, field: &str) -> Result<i64> {
    if field.is_empty() {
        return Ok(0);
    }
    field.parse().map_err(|e| {
        PcodeError::invalid_fact_format(filename, format!("bad integer {field:?}: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(table: &str) -> Vec<(String, i64)> {
        parse("T.facts", table.as_bytes(), |[a, b]| {
            Ok((a.to_string(), int("T.facts", b)?))
        })
        .unwrap()
    }

    #[test]
    fn rows_skip_empty_lines_and_need_no_final_newline() {
        assert_eq!(
            pairs("a\t1\n\nb\t2"),
            [("a".to_string(), 1), ("b".to_string(), 2)]
        );
    }

    #[test]
    fn quoted_fields_are_read_as_csv() {
        assert_eq!(
            pairs("\"a\tb\"\t1\r\nc\t2\n"),
            [("a\tb".to_string(), 1), ("c".to_string(), 2)]
        );
    }

    #[test]
    fn a_row_of_the_wrong_width_is_an_error() {
        assert!(parse("T.facts", b"a\t1\t2\n", |[a, _]: [&str; 2]| Ok(a.len())).is_err());
        assert!(parse("T.facts", b"a\n", |[a, _]: [&str; 2]| Ok(a.len())).is_err());
    }

    #[test]
    fn a_big_table_parses_in_file_order() {
        let table: String = (0..300_000).map(|i| format!("k{i}\t{i}\n")).collect();
        assert!(table.len() > 2 * CHUNK);
        let rows = pairs(&table);
        assert_eq!(rows.len(), 300_000);
        assert!(
            rows.iter()
                .enumerate()
                .all(|(i, (k, v))| *v == i as i64 && *k == format!("k{i}"))
        );
    }

    #[test]
    fn chunks_end_at_line_ends() {
        let chunks = split_lines(b"aaaa\nbb\ncccccc\nd", 3);
        assert_eq!(chunks, [&b"aaaa\n"[..], &b"bb\ncccccc\n"[..], &b"d"[..]]);
    }
}
