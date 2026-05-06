//! Binary serialization for artifact table, file table, span table, and file span table.
//!
//! Layout order: ArtifactTable, FileTable, SpanTable, FileSpanTable.

mod artifact;
mod file;
mod file_span;
pub mod leb128;

pub use artifact::write_artifact_table;
pub use file::write_file_table;
pub use file_span::{write_file_span_table, write_span_table};

use std::io::{Read, Write};

use crate::artifact::ArtifactTable;
use crate::file_span_table::FileSpanTable;
use crate::file_table::FileTable;
use crate::span_table::SpanTable;

/// Writes all four tables in order to the given writer.
pub fn write_tables(
    buf: &mut impl Write,
    artifacts: &ArtifactTable,
    files: &FileTable,
    span_table: &SpanTable,
    spans: &FileSpanTable,
) -> std::io::Result<()> {
    write_artifact_table(buf, artifacts)?;
    write_file_table(buf, files)?;
    write_span_table(buf, span_table)?;
    write_file_span_table(buf, spans)?;
    Ok(())
}

/// Reads all four tables in order from the given reader.
pub fn read_tables(
    r: &mut impl Read,
) -> std::io::Result<(ArtifactTable, FileTable, SpanTable, FileSpanTable)> {
    let (art_tree, file_tree, span_tree, fspan_tree) = crate::db::open_session_trees();
    let artifacts = artifact::read_artifact_table(r, art_tree)?;
    let files = file::read_file_table(r, file_tree)?;
    let span_table = file_span::read_span_table(r, span_tree)?;
    let spans = file_span::read_file_span_table(r, fspan_tree)?;
    Ok((artifacts, files, span_table, spans))
}
