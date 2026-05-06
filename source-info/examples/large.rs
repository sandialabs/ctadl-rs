use std::io::Cursor;

use source_info::{
    ArtifactEncoding, ArtifactKey, ArtifactMetadata, HashAlgorithm, SourceInfoBuilder, SpanLen,
};
use source_info::{read_tables, write_tables};

fn main() {
    let metadata = ArtifactMetadata {
        hash_algorithm: HashAlgorithm::Sha256,
        hash_len: 32,
        version: 1,
    };
    source_info::init_db(sled::open("/tmp/source_info_large").expect("sled error"));
    let mut builder = SourceInfoBuilder::new(metadata);

    let key1 = ArtifactKey {
        path: "/src/main.rs".to_string(),
        sub_artifact_id: 0,
        hash: vec![0u8; 32],
        encoding: ArtifactEncoding::Utf8,
    };
    let key2 = ArtifactKey {
        path: "/src/lib.rs".to_string(),
        sub_artifact_id: 0,
        hash: vec![1u8; 32],
        encoding: ArtifactEncoding::Utf8,
    };

    for i in 0..10000000 {
        builder.span_for(key1.clone(), i, SpanLen::Empty);
    }
    for i in 0..10000000 {
        builder.span_for(key2.clone(), i, SpanLen::Empty);
    }

    let artifact_count = builder.artifacts.artifacts.len();
    let file_count = builder.files.files.len();
    let span_count = builder.spans.len();

    let mut buf = Vec::new();
    write_tables(
        &mut buf,
        &builder.artifacts,
        &builder.files,
        &builder.span_table,
        &builder.spans,
    )
    .unwrap();
    let mut cursor = Cursor::new(buf);
    let (artifacts, files, _span_table, spans) = read_tables(&mut cursor).unwrap();

    assert_eq!(artifacts.artifacts.len(), artifact_count);
    assert_eq!(files.files.len(), file_count);
    assert_eq!(spans.len(), span_count);
}
