//! Export SARIF 2.1.0 for a fixed list of `file_span_id` values.
//!
//! Run: cargo run --example parquet_out && cargo run --example sarif_export

use datafusion::arrow::array::{StringViewArray, UInt8Array, UInt32Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;
use datafusion::prelude::*;
use serde_sarif::sarif::{
    ArtifactLocation, Location, Message, PhysicalLocation, Region, Result as SarifResult, Run,
    Sarif, Tool, ToolComponent,
};
use source_info::{LineMap, offset_to_line_column};
use std::sync::Arc;

// Synthetic source content shared across all three files in the parquet_out example.
// ~300 bytes, multiple lines; byte offsets 10-14 correspond to "n ma" in "fn main()".
const SOURCE: &[u8] = b"\
// source\n\
fn main() {\n\
    let alpha = 1;\n\
    let beta  = 2;\n\
    let gamma = 3;\n\
    let delta = 4;\n\
    let sum   = alpha + beta + gamma + delta;\n\
    println!(\"{}\", sum);\n\
}\n\
";

#[tokio::main]
async fn main() -> datafusion::error::Result<()> {
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "file_spans",
        "out/file_spans.parquet",
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet("spans", "out/spans.parquet", ParquetReadOptions::default())
        .await?;
    ctx.register_parquet("files", "out/files.parquet", ParquetReadOptions::default())
        .await?;
    ctx.register_parquet(
        "artifacts",
        "out/artifacts.parquet",
        ParquetReadOptions::default(),
    )
    .await?;

    let ids: Vec<u32> = vec![2, 6, 10];
    let schema = Arc::new(Schema::new(vec![Field::new(
        "file_span_id",
        DataType::UInt32,
        false,
    )]));
    let id_array = UInt32Array::from(ids);
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(id_array)])?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;
    ctx.register_table("filter_ids", Arc::new(table))?;

    let sql = "
        WITH
          s   AS (SELECT span_id, start, len_tag, len_value FROM spans),
          f   AS (SELECT file_id, artifact_id FROM files),
          art AS (SELECT artifact_id, canonical_path FROM artifacts)
        SELECT fs.file_span_id, art.canonical_path, s.start, s.len_tag, s.len_value
        FROM file_spans fs
        JOIN filter_ids fi ON fs.file_span_id = fi.file_span_id
        JOIN s   ON fs.span_id    = s.span_id
        JOIN f   ON fs.file_id    = f.file_id
        JOIN art ON f.artifact_id = art.artifact_id
        ORDER BY fs.file_span_id
    ";

    let batches = ctx.sql(sql).await?.collect().await?;

    let line_map = LineMap::from_bytes(SOURCE);
    let mut results: Vec<SarifResult> = Vec::new();

    for batch in &batches {
        let file_span_ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let canonical_paths = batch
            .column(1)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .unwrap();
        let starts = batch
            .column(2)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        let len_tags = batch
            .column(3)
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        let len_values = batch
            .column(4)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();

        for i in 0..batch.num_rows() {
            let file_span_id = file_span_ids.value(i);
            let canonical_path = canonical_paths.value(i);
            let start = starts.value(i);
            let len_tag = len_tags.value(i);
            let len_value = if len_tags.value(i) == 1 {
                len_values.value(i)
            } else {
                0
            };

            let end_byte = match len_tag {
                0 => start,
                1 => start + len_value,
                2 => SOURCE[start as usize..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| start + p as u32 + 1)
                    .unwrap_or(SOURCE.len() as u32),
                _ => start,
            };

            let start_lc = offset_to_line_column(&line_map, start);
            let end_lc = offset_to_line_column(&line_map, end_byte.saturating_sub(1).max(start));

            let region = Region::builder()
                .start_line(start_lc.line as i64)
                .start_column((start_lc.column + 1) as i64)
                .end_line(end_lc.line as i64)
                .end_column((end_lc.column + 1) as i64)
                .build();

            let artifact_location = ArtifactLocation::builder()
                .uri(format!("file://{canonical_path}"))
                .build();

            let physical_location = PhysicalLocation::builder()
                .artifact_location(artifact_location)
                .region(region)
                .build();

            let location = Location::builder()
                .physical_location(physical_location)
                .build();

            let result = SarifResult::builder()
                .rule_id(format!("finding/{file_span_id}"))
                .message(
                    Message::builder()
                        .text(format!("span {file_span_id}"))
                        .build(),
                )
                .locations(vec![location])
                .build();

            results.push(result);
        }
    }

    let tool = Tool::builder()
        .driver(ToolComponent::builder().name("source-info").build())
        .build();

    let run = Run::builder().tool(tool).results(results).build();

    let sarif = Sarif::builder().version("2.1.0").runs(vec![run]).build();

    println!("{}", serde_json::to_string_pretty(&sarif).unwrap());
    Ok(())
}
