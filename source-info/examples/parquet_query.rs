use datafusion::prelude::*;

#[tokio::main]
async fn main() -> datafusion::error::Result<()> {
    let ctx = SessionContext::new();

    ctx.register_parquet("spans", "out/spans.parquet", ParquetReadOptions::default())
        .await?;
    ctx.register_parquet(
        "file_spans",
        "out/file_spans.parquet",
        ParquetReadOptions::default(),
    )
    .await?;
    ctx.register_parquet("files", "out/files.parquet", ParquetReadOptions::default())
        .await?;
    ctx.register_parquet(
        "artifacts",
        "out/artifacts.parquet",
        ParquetReadOptions::default(),
    )
    .await?;

    let sql = "WITH s AS (
        SELECT span_id, start, len_tag, len_value FROM spans
    ), f AS (
        SELECT file_id, artifact_id FROM files
    ), art AS (
        SELECT artifact_id, canonical_path, encoding FROM artifacts
    )
    SELECT art.canonical_path, s.start, s.len_tag, s.len_value
    FROM s
    JOIN file_spans fs ON s.span_id     = fs.span_id
    JOIN f             ON fs.file_id    = f.file_id
    JOIN art           ON f.artifact_id = art.artifact_id
    WHERE s.start BETWEEN 10 AND 100
    ORDER BY art.canonical_path, s.start";

    println!("Spans with start offset in [10, 100], joined to source file:\n");
    ctx.sql(sql).await?.show().await?;
    Ok(())
}
