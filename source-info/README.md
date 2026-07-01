# Source Info

Library for keeping track of source info locations in source code and binaries.

# Schema

A `SourceInfo` is serialized as a directory of five standalone Parquet files,
one per table. Each table is written as a single Arrow `RecordBatch`. All
integer IDs are 1-based (value `0` is reserved for "none"; see `FileSpanId(0)`
= no source info).

Inspect with DuckDB, e.g. `SELECT * FROM read_parquet('out/artifacts.parquet')`.

## `metadata.parquet`

Single row describing the artifact table.

| column           | arrow type | null | notes                                         |
| ---------------- | ---------- | ---- | --------------------------------------------- |
| `hash_algorithm` | `uint8`    | no   | `0` = SHA-256; any other value = `Other(n)`   |
| `hash_len`       | `uint8`    | no   | content hash length in bytes (e.g. `32`)      |
| `version`        | `uint32`   | no   | schema version (stored as `u16`, written u32) |

## `artifacts.parquet`

Immutable source artifacts, interned by
`(canonical_path, sub_artifact_id, content_hash, encoding)`.

| column            | arrow type | null | notes                                         |
| ----------------- | ---------- | ---- | --------------------------------------------- |
| `artifact_id`     | `uint32`   | no   | 1-based row id                                |
| `canonical_path`  | `string`   | no   | canonical path of the artifact                |
| `sub_artifact_id` | `uint32`   | no   | disambiguates artifacts sharing a path        |
| `encoding`        | `uint8`    | no   | `0` = UTF-8, `1` = UTF-16, `2` = Binary       |
| `content_hash`    | `binary`   | no   | raw hash bytes (`hash_len` bytes)             |

## `files.parquet`

Files, each referencing an artifact.

| column        | arrow type | null | notes                                |
| ------------- | ---------- | ---- | ------------------------------------ |
| `file_id`     | `uint32`   | no   | 1-based row id                       |
| `artifact_id` | `uint32`   | no   | references `artifacts.artifact_id`   |

## `spans.parquet`

Raw spans: a byte offset and a length. Length is encoded as a tag plus an
optional value.

| column      | arrow type | null | notes                                                        |
| ----------- | ---------- | ---- | ------------------------------------------------------------ |
| `span_id`   | `uint32`   | no   | 1-based row id                                               |
| `start`     | `uint32`   | no   | byte offset of the span start                                |
| `len_tag`   | `uint8`    | no   | `0` = Empty (caret), `1` = ByteLen, `2` = ToLineEnd          |
| `len_value` | `uint32`   | yes  | byte length when `len_tag == 1`; null otherwise              |

`len_tag` meanings:
- `0` (Empty): zero-length caret position.
- `1` (ByteLen): span covers `[start, start + len_value)`.
- `2` (ToLineEnd): span runs from `start` to the next newline.

## `file_spans.parquet`

Associates a file with a span, forming an addressable source location.

| column         | arrow type | null | notes                            |
| -------------- | ---------- | ---- | -------------------------------- |
| `file_span_id` | `uint32`   | no   | 1-based row id                   |
| `file_id`      | `uint32`   | no   | references `files.file_id`       |
| `span_id`      | `uint32`   | no   | references `spans.span_id`       |
