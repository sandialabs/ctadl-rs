use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ctadl_flowy as flowy;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let contents = std::fs::read(&args[1])?;
    let program = flowy::compile_program(&args[1])?;
    println!("{}", program.requirements.endpoint_requires);
    print!("{:#}", program.program_info);
    std::fs::create_dir_all("/tmp/flowy-test").with_context(|| "creating output dir")?;
    source_info::write_parquet_source_info("/tmp/flowy-test", &program.program_info.source_info)
        .with_context(|| "writing source info")?;
    format_with_datafusion(
        "/tmp/flowy-test",
        &contents,
        &program.requirements.endpoint_requires,
    )
    .await
    .with_context(|| "formatting")?;
    Ok(())
}

async fn format_with_datafusion<P: AsRef<Path>>(
    dir: P,
    contents: &[u8],
    requires: &flowy::EndpointRequires,
) -> anyhow::Result<()> {
    let dir = dir.as_ref();
    let ctx = SessionContext::new();
    ctx.register_parquet(
        "file_spans",
        dir.join("file_spans.parquet").to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await
    .with_context(|| "reading file_spans")?;
    ctx.register_parquet(
        "spans",
        dir.join("spans.parquet").to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await
    .with_context(|| "reading file_spans")?;
    ctx.register_parquet(
        "files",
        dir.join("files.parquet").to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await
    .with_context(|| "reading file_spans")?;
    ctx.register_parquet(
        "artifacts",
        dir.join("artifacts.parquet").to_string_lossy(),
        ParquetReadOptions::default(),
    )
    .await
    .with_context(|| "reading file_spans")?;

    let ids: Vec<_> = requires
        .requires
        .iter()
        .flat_map(|(_k, infos)| infos.iter().map(|(ep, _)| ep.source_info.span_id.0))
        .collect();
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

    let line_map = LineMap::from_bytes(contents);
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
                2 => contents[start as usize..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| start + p as u32 + 1)
                    .unwrap_or(contents.len() as u32),
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
