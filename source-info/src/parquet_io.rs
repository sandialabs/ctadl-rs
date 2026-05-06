//! Parquet-based serialization for SourceInfo.
//!
//! Writes one standalone `.parquet` file per table into a directory:
//! - `metadata.parquet`   — single row: hash_algorithm, hash_len, version
//! - `artifacts.parquet`  — artifact_id, canonical_path, sub_artifact_id, encoding, content_hash
//! - `files.parquet`      — file_id, artifact_id
//! - `spans.parquet`      — span_id, start, len_tag, len_value (nullable)
//! - `file_spans.parquet` — file_span_id, file_id, span_id

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, BinaryArray, StringArray, UInt8Array, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriter;

use crate::artifact::{ArtifactEncoding, ArtifactMetadata, ArtifactRecord, HashAlgorithm};
use crate::file_table::FileEntry;
use crate::ids::{ArtifactId, FileId, SpanId};
use crate::source_info::SourceInfo;
use crate::span::{FileSpan, Span, SpanLen};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ParquetError {
    Parquet(parquet::errors::ParquetError),
    Arrow(arrow::error::ArrowError),
    Io(io::Error),
    MissingMetadata(String),
    InvalidData(String),
}

impl std::fmt::Display for ParquetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParquetError::Parquet(e) => write!(f, "parquet error: {e}"),
            ParquetError::Arrow(e) => write!(f, "arrow error: {e}"),
            ParquetError::Io(e) => write!(f, "io error: {e}"),
            ParquetError::MissingMetadata(k) => write!(f, "missing metadata key: {k}"),
            ParquetError::InvalidData(m) => write!(f, "invalid data: {m}"),
        }
    }
}

impl std::error::Error for ParquetError {}

impl From<parquet::errors::ParquetError> for ParquetError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        ParquetError::Parquet(e)
    }
}

impl From<arrow::error::ArrowError> for ParquetError {
    fn from(e: arrow::error::ArrowError) -> Self {
        ParquetError::Arrow(e)
    }
}

impl From<io::Error> for ParquetError {
    fn from(e: io::Error) -> Self {
        ParquetError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ParquetError>;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn metadata_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("hash_algorithm", DataType::UInt8, false),
        Field::new("hash_len", DataType::UInt8, false),
        Field::new("version", DataType::UInt32, false),
    ]))
}

fn artifacts_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("artifact_id", DataType::UInt32, false),
        Field::new("canonical_path", DataType::Utf8, false),
        Field::new("sub_artifact_id", DataType::UInt32, false),
        Field::new("encoding", DataType::UInt8, false),
        Field::new("content_hash", DataType::Binary, false),
    ]))
}

fn files_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::UInt32, false),
        Field::new("artifact_id", DataType::UInt32, false),
    ]))
}

fn spans_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("span_id", DataType::UInt32, false),
        Field::new("start", DataType::UInt32, false),
        Field::new("len_tag", DataType::UInt8, false),
        Field::new("len_value", DataType::UInt32, true),
    ]))
}

fn file_spans_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("file_span_id", DataType::UInt32, false),
        Field::new("file_id", DataType::UInt32, false),
        Field::new("span_id", DataType::UInt32, false),
    ]))
}

// ---------------------------------------------------------------------------
// Per-table write helpers
// ---------------------------------------------------------------------------

fn write_metadata_file(dir: &Path, metadata: &ArtifactMetadata) -> Result<()> {
    let schema = metadata_schema();
    let hash_algorithms: UInt8Array = vec![Some(metadata.hash_algorithm.to_u8())]
        .into_iter()
        .collect();
    let hash_lens: UInt8Array = vec![Some(metadata.hash_len)].into_iter().collect();
    let versions: UInt32Array = vec![Some(metadata.version as u32)].into_iter().collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(hash_algorithms) as ArrayRef,
            Arc::new(hash_lens) as ArrayRef,
            Arc::new(versions) as ArrayRef,
        ],
    )?;
    let file = File::create(dir.join("metadata.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

fn write_artifacts_file(dir: &Path, artifacts: &[ArtifactRecord]) -> Result<()> {
    let schema = artifacts_schema();
    let artifact_ids: UInt32Array = (1u32..=artifacts.len() as u32).map(Some).collect();
    let canonical_paths: StringArray = artifacts
        .iter()
        .map(|a| Some(a.canonical_path.as_str()))
        .collect();
    let sub_artifact_ids: UInt32Array = artifacts.iter().map(|a| Some(a.sub_artifact_id)).collect();
    let encodings: UInt8Array = artifacts.iter().map(|a| Some(a.encoding.to_u8())).collect();
    let content_hashes: BinaryArray = artifacts
        .iter()
        .map(|a| Some(a.content_hash.as_slice()))
        .collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(artifact_ids) as ArrayRef,
            Arc::new(canonical_paths) as ArrayRef,
            Arc::new(sub_artifact_ids) as ArrayRef,
            Arc::new(encodings) as ArrayRef,
            Arc::new(content_hashes) as ArrayRef,
        ],
    )?;
    let file = File::create(dir.join("artifacts.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

fn write_files_file(dir: &Path, files: &[FileEntry]) -> Result<()> {
    let schema = files_schema();
    let file_ids: UInt32Array = (1u32..=files.len() as u32).map(Some).collect();
    let artifact_ids: UInt32Array = files.iter().map(|f| Some(f.artifact.0)).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(file_ids) as ArrayRef,
            Arc::new(artifact_ids) as ArrayRef,
        ],
    )?;
    let file = File::create(dir.join("files.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

fn write_spans_file(dir: &Path, spans: &[Span]) -> Result<()> {
    let schema = spans_schema();
    let span_ids: UInt32Array = (1u32..=spans.len() as u32).map(Some).collect();
    let starts: UInt32Array = spans.iter().map(|s| Some(s.start)).collect();
    let len_tags: UInt8Array = spans
        .iter()
        .map(|s| {
            Some(match s.len {
                SpanLen::Empty => 0u8,
                SpanLen::ByteLen(_) => 1u8,
                SpanLen::ToLineEnd => 2u8,
            })
        })
        .collect();
    let len_values: UInt32Array = spans
        .iter()
        .map(|s| match s.len {
            SpanLen::ByteLen(n) => Some(n),
            _ => None,
        })
        .collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(span_ids) as ArrayRef,
            Arc::new(starts) as ArrayRef,
            Arc::new(len_tags) as ArrayRef,
            Arc::new(len_values) as ArrayRef,
        ],
    )?;
    let file = File::create(dir.join("spans.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

fn write_file_spans_file(dir: &Path, file_spans: &[FileSpan]) -> Result<()> {
    let schema = file_spans_schema();
    let file_span_ids: UInt32Array = (1u32..=file_spans.len() as u32).map(Some).collect();
    let file_ids: UInt32Array = file_spans.iter().map(|fs| Some(fs.file.0)).collect();
    let span_ids: UInt32Array = file_spans.iter().map(|fs| Some(fs.span.0)).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(file_span_ids) as ArrayRef,
            Arc::new(file_ids) as ArrayRef,
            Arc::new(span_ids) as ArrayRef,
        ],
    )?;
    let file = File::create(dir.join("file_spans.parquet"))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Public write API
// ---------------------------------------------------------------------------

/// Write all five tables as standalone Parquet files into `dir`.
pub fn write_parquet(
    dir: &Path,
    metadata: &ArtifactMetadata,
    artifacts: &[ArtifactRecord],
    files: &[FileEntry],
    spans: &[Span],
    file_spans: &[FileSpan],
) -> Result<()> {
    write_metadata_file(dir, metadata)?;
    write_artifacts_file(dir, artifacts)?;
    write_files_file(dir, files)?;
    write_spans_file(dir, spans)?;
    write_file_spans_file(dir, file_spans)?;
    Ok(())
}

/// Convenience wrapper: serialize a `SourceInfo` to `dir`.
pub fn write_parquet_source_info<P: AsRef<Path>>(dir: P, info: &SourceInfo) -> Result<()> {
    write_parquet(
        dir.as_ref(),
        &info.metadata,
        &info.artifacts,
        &info.files,
        &info.spans,
        &info.file_spans,
    )
}

// ---------------------------------------------------------------------------
// Per-table read helpers
// ---------------------------------------------------------------------------

fn read_metadata_file(dir: &Path) -> Result<ArtifactMetadata> {
    let file = File::open(dir.join("metadata.parquet"))?;
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    if let Some(batch_result) = reader.next() {
        let batch = batch_result?;
        let hash_algorithms = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("hash_algorithm column type mismatch".to_string())
            })?;
        let hash_lens = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("hash_len column type mismatch".to_string())
            })?;
        let versions = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| ParquetError::InvalidData("version column type mismatch".to_string()))?;
        if batch.num_rows() == 0 {
            return Err(ParquetError::MissingMetadata(
                "metadata.parquet has no rows".to_string(),
            ));
        }
        return Ok(ArtifactMetadata {
            hash_algorithm: HashAlgorithm::from_u8(hash_algorithms.value(0)),
            hash_len: hash_lens.value(0),
            version: versions.value(0) as u16,
        });
    }
    Err(ParquetError::MissingMetadata(
        "metadata.parquet has no rows".to_string(),
    ))
}

fn read_artifacts_file(dir: &Path) -> Result<Vec<ArtifactRecord>> {
    let file = File::open(dir.join("artifacts.parquet"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut artifacts = Vec::new();
    for batch_result in reader {
        let batch = batch_result?;
        let paths = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                ParquetError::InvalidData("canonical_path column type mismatch".to_string())
            })?;
        let sub_ids = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("sub_artifact_id column type mismatch".to_string())
            })?;
        let encodings = batch
            .column(3)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("encoding column type mismatch".to_string())
            })?;
        let hashes = batch
            .column(4)
            .as_any()
            .downcast_ref::<BinaryArray>()
            .ok_or_else(|| {
                ParquetError::InvalidData("content_hash column type mismatch".to_string())
            })?;
        for i in 0..batch.num_rows() {
            artifacts.push(ArtifactRecord {
                canonical_path: paths.value(i).to_string(),
                sub_artifact_id: sub_ids.value(i),
                encoding: ArtifactEncoding::from_u8(encodings.value(i)),
                content_hash: hashes.value(i).to_vec(),
            });
        }
    }
    Ok(artifacts)
}

fn read_files_file(dir: &Path) -> Result<Vec<FileEntry>> {
    let file = File::open(dir.join("files.parquet"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut files = Vec::new();
    for batch_result in reader {
        let batch = batch_result?;
        let artifact_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("artifact_id column type mismatch".to_string())
            })?;
        for i in 0..batch.num_rows() {
            files.push(FileEntry {
                artifact: ArtifactId(artifact_ids.value(i)),
            });
        }
    }
    Ok(files)
}

fn read_spans_file(dir: &Path) -> Result<Vec<Span>> {
    let file = File::open(dir.join("spans.parquet"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut spans = Vec::new();
    for batch_result in reader {
        let batch = batch_result?;
        let starts = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| ParquetError::InvalidData("start column type mismatch".to_string()))?;
        let len_tags = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .ok_or_else(|| ParquetError::InvalidData("len_tag column type mismatch".to_string()))?;
        let len_values = batch
            .column(3)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| {
                ParquetError::InvalidData("len_value column type mismatch".to_string())
            })?;
        for i in 0..batch.num_rows() {
            let len = match len_tags.value(i) {
                0 => SpanLen::Empty,
                1 => SpanLen::ByteLen(len_values.value(i)),
                2 => SpanLen::ToLineEnd,
                t => return Err(ParquetError::InvalidData(format!("unknown len_tag: {t}"))),
            };
            spans.push(Span {
                start: starts.value(i),
                len,
            });
        }
    }
    Ok(spans)
}

fn read_file_spans_file(dir: &Path) -> Result<Vec<FileSpan>> {
    let file = File::open(dir.join("file_spans.parquet"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    let mut file_spans = Vec::new();
    for batch_result in reader {
        let batch = batch_result?;
        let file_ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| ParquetError::InvalidData("file_id column type mismatch".to_string()))?;
        let span_ids = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .ok_or_else(|| ParquetError::InvalidData("span_id column type mismatch".to_string()))?;
        for i in 0..batch.num_rows() {
            file_spans.push(FileSpan {
                file: FileId(file_ids.value(i)),
                span: SpanId(span_ids.value(i)),
            });
        }
    }
    Ok(file_spans)
}

// ---------------------------------------------------------------------------
// Public read API
// ---------------------------------------------------------------------------

/// Read a `SourceInfo` from the five parquet files in `dir`.
pub fn read_parquet_source_info(dir: &Path) -> Result<SourceInfo> {
    let metadata = read_metadata_file(dir)?;
    let artifacts = read_artifacts_file(dir)?;
    let files = read_files_file(dir)?;
    let spans = read_spans_file(dir)?;
    let file_spans = read_file_spans_file(dir)?;
    Ok(SourceInfo {
        metadata,
        artifacts,
        files,
        spans,
        file_spans,
    })
}
