//! File table binary serialization.
//!
//! Layout: file_count (ULEB128), then artifact_id (ULEB128) per file.

use std::io::{Read, Write};

use crate::file_table::{FileEntry, FileTable};
use crate::ids::ArtifactId;
use crate::serialize::leb128;
use crate::store::FilesStore;

pub fn write_file_table(buf: &mut impl Write, table: &FileTable) -> std::io::Result<()> {
    leb128::write_uleb128(buf, table.files.len() as u32)?;
    for entry in &table.files {
        leb128::write_uleb128(buf, entry.artifact.0)?;
    }
    Ok(())
}

pub fn read_file_table(r: &mut impl Read, store: FilesStore) -> std::io::Result<FileTable> {
    let (count, _) = leb128::read_uleb128(r)?;
    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (aid, _) = leb128::read_uleb128(r)?;
        files.push(FileEntry {
            artifact: ArtifactId(aid),
        });
    }
    Ok(FileTable::from_entries(files, store))
}
