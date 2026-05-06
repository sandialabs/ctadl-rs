//! Write synthetic source info to `out/` as five standalone Parquet files.
//!
//! Run with: cargo run --example parquet_out
//! Then inspect with DuckDB: SELECT * FROM read_parquet('out/artifacts.parquet');

use std::fs;
use std::path::Path;

use source_info::{
    ArtifactEncoding, ArtifactKey, ArtifactMetadata, HashAlgorithm, SourceInfoBuilder, SpanLen,
};

fn main() {
    let out = Path::new("out");
    fs::create_dir_all(out).expect("failed to create out/");

    let metadata = ArtifactMetadata {
        hash_algorithm: HashAlgorithm::Sha256,
        hash_len: 32,
        version: 1,
    };
    let mut builder = SourceInfoBuilder::new(metadata);

    let files = [
        ("/src/main.rs", vec![0u8; 32], ArtifactEncoding::Utf8),
        ("/src/lib.rs", vec![1u8; 32], ArtifactEncoding::Utf8),
        ("/assets/icon.png", vec![2u8; 32], ArtifactEncoding::Binary),
    ];

    for (path, hash, encoding) in files {
        let key = ArtifactKey {
            path: path.to_string(),
            sub_artifact_id: 0,
            hash,
            encoding,
        };
        builder.span_for(key.clone(), 0, SpanLen::Empty);
        builder.span_for(key.clone(), 10, SpanLen::ByteLen(5));
        builder.span_for(key.clone(), 100, SpanLen::ByteLen(42));
        builder.span_for(key, 200, SpanLen::ToLineEnd);
    }

    builder.write_parquet(out).expect("write_parquet failed");

    println!("Wrote 5 Parquet files to {}/", out.display());
    for name in [
        "metadata.parquet",
        "artifacts.parquet",
        "files.parquet",
        "spans.parquet",
        "file_spans.parquet",
    ] {
        let p = out.join(name);
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        println!("  {name:25} {size:>6} bytes");
    }
}
