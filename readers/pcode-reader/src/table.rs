//! Reading one fact table: a window of its text at a time, each window's rows parsed in
//! parallel.
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

/// How many bytes of a table's text are read at a time. Every table is read at once, so this,
/// not the size of the largest table, bounds the text in memory.
const WINDOW: usize = 8 << 20;

/// Each row of the table at `path` (named `filename`, for errors; inflated when
/// `compressed`), as [`parse`] reads them, in file order.
pub(crate) fn read<const N: usize, T, F>(
    path: &Path,
    compressed: bool,
    filename: &str,
    f: F,
) -> Result<Vec<T>>
where
    T: Send,
    F: Fn([&str; N]) -> Result<T> + Sync,
{
    let file = std::fs::File::open(path)?;
    let mut reader: Box<dyn Read + Send> = if compressed {
        Box::new(flate2::read::MultiGzDecoder::new(std::io::BufReader::new(
            file,
        )))
    } else {
        Box::new(file)
    };
    // Appends up to a window of text to `buf`; true at the end of the table.
    let fill = |reader: &mut Box<dyn Read + Send>, buf: &mut Vec<u8>| -> Result<bool> {
        Ok(reader.take(WINDOW as u64).read_to_end(buf)? < WINDOW)
    };
    let mut rows = Vec::new();
    // The text read and not yet parsed: whole lines but for the last.
    let mut window = Vec::with_capacity(WINDOW);
    let mut at_end = fill(&mut reader, &mut window)?;
    loop {
        if memchr::memchr2(b'"', b'\r', &window).is_some() {
            // A quoted field may span lines, so the rest is read as one.
            reader.read_to_end(&mut window)?;
            rows.extend(parse_csv(filename, &window, &f)?);
            return Ok(rows);
        }
        if at_end {
            rows.extend(parse(filename, &window, &f)?);
            return Ok(rows);
        }
        let end = memchr::memrchr(b'\n', &window).map_or(0, |i| i + 1);
        let mut next = Vec::with_capacity(WINDOW + window.len() - end);
        next.extend_from_slice(&window[end..]);
        // Inflating a window is serial, so the next one is read while this one is parsed.
        let (parsed, filled) = rayon::join(
            || parse(filename, &window[..end], &f),
            || fill(&mut reader, &mut next),
        );
        rows.extend(parsed?);
        at_end = filled?;
        window = next;
    }
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

    /// A table bigger than a window reads the same plain, gzipped, and with a quoted field
    /// past the first window.
    #[test]
    fn a_table_reads_the_same_across_windows() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mut text: String = (0..1_000_000).map(|i| format!("key{i}\t{i}\n")).collect();
        assert!(text.len() > WINDOW);
        let expected = pairs(&text);
        let read_pairs = |path: &Path, compressed| {
            read(path, compressed, "T.facts", |[a, b]| {
                Ok((a.to_string(), int("T.facts", b)?))
            })
            .unwrap()
        };

        let plain = dir.path().join("T.facts");
        std::fs::write(&plain, &text).unwrap();
        assert!(read_pairs(&plain, false) == expected);

        let gz = dir.path().join("T.facts.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&gz).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap();
        assert!(read_pairs(&gz, true) == expected);

        text.push_str("\"quoted\tkey\"\t7\n");
        std::fs::write(&plain, &text).unwrap();
        let rows = read_pairs(&plain, false);
        assert!(rows[..expected.len()] == expected[..]);
        assert_eq!(rows[expected.len()..], [("quoted\tkey".to_string(), 7)]);
    }

    #[test]
    fn chunks_end_at_line_ends() {
        let chunks = split_lines(b"aaaa\nbb\ncccccc\nd", 3);
        assert_eq!(chunks, [&b"aaaa\n"[..], &b"bb\ncccccc\n"[..], &b"d"[..]]);
    }
}
