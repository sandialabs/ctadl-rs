//! Artifact table binary serialization.
//!
//! Layout: metadata (hash_algorithm u8, hash_len u8, version 2 bytes), artifact_count (ULEB128),
//! then per record: path_len (ULEB128), path_bytes, sub_artifact_id (ULEB128), encoding (u8),
//! hash_len (ULEB128), hash_bytes.

use std::io::{Read, Write};

use crate::artifact::{
    ArtifactEncoding, ArtifactMetadata, ArtifactRecord, ArtifactTable, HashAlgorithm,
};
use crate::serialize::leb128;
use crate::store::ArtifactsStore;

pub fn write_artifact_table(buf: &mut impl Write, table: &ArtifactTable) -> std::io::Result<()> {
    buf.write_all(&[table.metadata.hash_algorithm.to_u8()])?;
    buf.write_all(&[table.metadata.hash_len])?;
    buf.write_all(&table.metadata.version.to_le_bytes())?;
    leb128::write_uleb128(buf, table.artifacts.len() as u32)?;
    for rec in &table.artifacts {
        write_artifact_record(buf, rec)?;
    }
    Ok(())
}

fn write_artifact_record(buf: &mut impl Write, rec: &ArtifactRecord) -> std::io::Result<()> {
    leb128::write_uleb128(buf, rec.canonical_path.len() as u32)?;
    buf.write_all(rec.canonical_path.as_bytes())?;
    leb128::write_uleb128(buf, rec.sub_artifact_id)?;
    buf.write_all(&[rec.encoding.to_u8()])?;
    leb128::write_uleb128(buf, rec.content_hash.len() as u32)?;
    buf.write_all(&rec.content_hash)?;
    Ok(())
}

pub fn read_artifact_table(
    r: &mut impl Read,
    store: ArtifactsStore,
) -> std::io::Result<ArtifactTable> {
    let mut alg = [0u8; 1];
    r.read_exact(&mut alg)?;
    let mut hash_len = [0u8; 1];
    r.read_exact(&mut hash_len)?;
    let mut version = [0u8; 2];
    r.read_exact(&mut version)?;
    let (count, _) = leb128::read_uleb128(r)?;
    let metadata = ArtifactMetadata {
        hash_algorithm: HashAlgorithm::from_u8(alg[0]),
        hash_len: hash_len[0],
        version: u16::from_le_bytes(version),
    };
    let mut artifacts = Vec::with_capacity(count as usize);
    for _ in 0..count {
        artifacts.push(read_artifact_record(r)?);
    }
    Ok(ArtifactTable::from_records(metadata, artifacts, store))
}

fn read_artifact_record(r: &mut impl Read) -> std::io::Result<ArtifactRecord> {
    let (path_len, _) = leb128::read_uleb128(r)?;
    let mut path = vec![0u8; path_len as usize];
    r.read_exact(&mut path)?;
    let canonical_path = String::from_utf8(path).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "artifact path not UTF-8")
    })?;
    let (sub_artifact_id, _) = leb128::read_uleb128(r)?;
    let mut enc = [0u8; 1];
    r.read_exact(&mut enc)?;
    let encoding = ArtifactEncoding::from_u8(enc[0]);
    let (hash_len, _) = leb128::read_uleb128(r)?;
    let mut content_hash = vec![0u8; hash_len as usize];
    r.read_exact(&mut content_hash)?;
    Ok(ArtifactRecord {
        canonical_path,
        sub_artifact_id,
        encoding,
        content_hash,
    })
}
