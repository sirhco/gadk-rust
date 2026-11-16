// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Golden tests of `write/arrow_batch.rs` (ADK-Go `plugin/agentanalytics/batch_processor.go`
//! `buildRecordBatch`, `serializeRecord`, `estimateRowBytes`, `splitBatchByEstimatedBytes`)
//! against the Go probe fixtures `record_batch/` (spec R3): Go's
//! and Rust's record batch IPC messages are decoded and compared, as are the size estimates and
//! the split chunk lengths.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow_array::cast::AsArray;
use arrow_array::types::{Int64Type, TimestampMicrosecondType};
use arrow_array::{Array, RecordBatch, StructArray};
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{Field, FieldRef, Schema};
use gadk_agentanalytics::config::Logger;
use gadk_agentanalytics::project_arrow_schema;
use gadk_agentanalytics::row::Row;
use gadk_agentanalytics::testutil::row_from_spec;
use gadk_agentanalytics::write::arrow_batch::{
    MAX_APPEND_REQUEST_BYTES, build_record_batch, estimate_row_bytes, serialize_record,
    split_batch_by_estimated_bytes,
};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

/// Arrow IPC end-of-stream marker (continuation token, zero length).
const EOS: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_json(rel: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(rel)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn cases(rel: &str) -> Vec<Value> {
    read_json(rel)["cases"].as_array().unwrap().clone()
}

/// A fixture `denied` value (`null` or a list of column names) as the denylist set.
fn denied_set(v: &Value) -> BTreeSet<String> {
    match v {
        Value::Null => BTreeSet::new(),
        Value::Array(items) => items
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect(),
        other => panic!("unexpected denied value {other}"),
    }
}

fn rows_of(case: &Value) -> Vec<Row> {
    case["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| row_from_spec(spec).unwrap())
        .collect()
}

/// `schema` with every top-level field nullable: Go's batches may carry NULL in the
/// non-nullable `timestamp`, which arrow-rs's IPC reader refuses under the declared schema.
fn relaxed(schema: &Schema) -> Schema {
    let fields: Vec<FieldRef> = schema
        .fields()
        .iter()
        .map(|f| Arc::new(f.as_ref().clone().with_nullable(true)))
        .collect();
    Schema::new_with_metadata(fields, schema.metadata().clone())
}

/// Decodes a record batch IPC message (no schema, no end-of-stream marker) as the T2 format
/// says: the schema stream (without its end-of-stream marker), the message, the marker. The
/// schema stream is arrow-rs's encoding of `relaxed(schema)`.
fn decode(schema: &Schema, message: &[u8]) -> RecordBatch {
    let mut writer = StreamWriter::try_new(Vec::new(), &relaxed(schema)).unwrap();
    writer.finish().unwrap();
    let mut stream = writer.into_inner().unwrap();
    stream.truncate(stream.len() - EOS.len());
    stream.extend_from_slice(message);
    stream.extend_from_slice(&EOS);
    let mut reader = StreamReader::try_new(Cursor::new(stream), None).unwrap();
    let batch = reader.next().expect("one record batch").unwrap();
    assert!(reader.next().is_none(), "exactly one record batch");
    batch
}

/// The schema of Go's `schema_file` stream (`arrow/events.arrows` or a projection).
fn go_schema(rel: &str) -> Schema {
    let bytes = std::fs::read(fixtures().join(rel)).unwrap();
    let reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
    reader.schema().as_ref().clone()
}

fn string_or_null(a: &dyn Array, i: usize) -> Value {
    if a.is_null(i) {
        Value::Null
    } else {
        Value::from(a.as_string::<i32>().value(i))
    }
}

/// A decoded struct's fields by name as the fixture's `decoded` writes them.
fn struct_row(s: &StructArray, i: usize) -> Value {
    let mut out = Map::new();
    for (f, col) in s.fields().iter().zip(s.columns()) {
        out.insert(f.name().clone(), cell(f, col.as_ref(), i));
    }
    Value::Object(out)
}

/// One cell in the fixture's `decoded` form: `null`, strings, bools, timestamps as µs,
/// `part_index` as int64, `content_parts` as an array of part objects, `object_ref` as an
/// object or `null`.
fn cell(field: &Field, a: &dyn Array, i: usize) -> Value {
    if a.is_null(i) {
        return Value::Null;
    }
    match field.name().as_str() {
        "timestamp" => json!(a.as_primitive::<TimestampMicrosecondType>().value(i)),
        "part_index" => json!(a.as_primitive::<Int64Type>().value(i)),
        "is_truncated" => json!(a.as_boolean().value(i)),
        "content_parts" => {
            let parts = a.as_list::<i32>().value(i);
            let parts = parts.as_struct();
            Value::Array((0..parts.len()).map(|j| struct_row(parts, j)).collect())
        }
        "object_ref" => struct_row(a.as_struct(), i),
        _ => string_or_null(a, i),
    }
}

/// The decoded rows in the fixture's form (every column, sorted keys).
fn decoded_rows(batch: &RecordBatch) -> Vec<Value> {
    (0..batch.num_rows())
        .map(|i| {
            let mut out = Map::new();
            for (f, col) in batch.schema().fields().iter().zip(batch.columns()) {
                out.insert(f.name().clone(), cell(f, col.as_ref(), i));
            }
            Value::Object(out)
        })
        .collect()
}

/// `[[column, null_count], ...]` in schema order.
fn null_counts(batch: &RecordBatch) -> Value {
    Value::Array(
        batch
            .schema()
            .fields()
            .iter()
            .zip(batch.columns())
            .map(|(f, c)| json!([f.name(), c.null_count()]))
            .collect(),
    )
}

#[test]
fn record_batches_decode_like_gos() {
    let cases = cases("record_batch/corpus.json");
    assert_eq!(cases.len(), 24);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let schema = project_arrow_schema(&denied_set(&case["denied"]));
        assert_eq!(
            go_schema(case["schema_file"].as_str().unwrap()),
            schema,
            "{name}: schema_file"
        );
        let go_bytes = std::fs::read(
            fixtures()
                .join("record_batch")
                .join(case["file"].as_str().unwrap()),
        )
        .unwrap();
        assert_eq!(go_bytes.len() as u64, case["byte_len"], "{name}: byte_len");

        let rows = rows_of(case);
        let record = build_record_batch(&rows, &schema).unwrap();
        assert_eq!(
            record.num_rows() as u64,
            case["num_rows"],
            "{name}: num_rows"
        );
        let rust_bytes = serialize_record(&record, None).unwrap();

        let go = decode(&schema, &go_bytes);
        let rust = decode(&schema, &rust_bytes);
        assert_eq!(rust, go, "{name}: decoded batches");
        assert_eq!(
            Value::Array(decoded_rows(&go)),
            case["decoded"],
            "{name}: Go decoded"
        );
        assert_eq!(null_counts(&go), case["null_counts"], "{name}: null_counts");
        assert_eq!(null_counts(&record), case["null_counts"], "{name}: built");
        assert_eq!(
            decoded_rows(&record),
            decoded_rows(&rust),
            "{name}: round trip"
        );
    }
}

/// The flatbuffer header of a record batch IPC message: its length, the field nodes
/// `(length, null_count)` and the body buffers `(offset, length)`.
type Layout = (usize, Vec<(i64, i64)>, Vec<(usize, usize)>);

fn layout(message: &[u8]) -> Layout {
    assert_eq!(
        message[..4],
        [0xff, 0xff, 0xff, 0xff],
        "continuation marker"
    );
    let len = usize::try_from(i32::from_le_bytes(message[4..8].try_into().unwrap())).unwrap();
    let header = arrow_ipc::root_as_message(&message[8..8 + len]).unwrap();
    let batch = header.header_as_record_batch().unwrap();
    let nodes = batch
        .nodes()
        .unwrap()
        .iter()
        .map(|n| (n.length(), n.null_count()))
        .collect();
    let buffers = batch
        .buffers()
        .unwrap()
        .iter()
        .map(|b| {
            (
                usize::try_from(b.offset()).unwrap(),
                usize::try_from(b.length()).unwrap(),
            )
        })
        .collect();
    (len, nodes, buffers)
}

/// Spec R3 beyond decoding: Rust's message has Go's field nodes and buffer order, and every
/// body buffer holds Go's bytes; only a validity bitmap may be shorter (arrow-go writes its
/// builder's allocation, zero past the last row) and the flatbuffer header may differ in
/// layout (ledger 7b-2).
#[test]
fn record_batch_messages_match_gos_layout() {
    for case in cases("record_batch/corpus.json") {
        let name = case["name"].as_str().unwrap();
        let schema = project_arrow_schema(&denied_set(&case["denied"]));
        let go_bytes = std::fs::read(
            fixtures()
                .join("record_batch")
                .join(case["file"].as_str().unwrap()),
        )
        .unwrap();
        let record = build_record_batch(&rows_of(&case), &schema).unwrap();
        let rust_bytes = serialize_record(&record, None).unwrap();
        let (go_len, go_nodes, go_buffers) = layout(&go_bytes);
        let (rust_len, rust_nodes, rust_buffers) = layout(&rust_bytes);
        assert_eq!(rust_nodes, go_nodes, "{name}: field nodes");
        assert_eq!(rust_buffers.len(), go_buffers.len(), "{name}: buffer count");
        if name == "empty_batch" {
            assert_eq!((go_len, rust_len), (1808, 1800), "{name}: header");
        } else {
            assert_eq!(rust_len, go_len, "{name}: header length");
        }
        let go_body = &go_bytes[8 + go_len..];
        let rust_body = &rust_bytes[8 + rust_len..];
        for (i, (&(g_off, g_len), &(r_off, r_len))) in
            go_buffers.iter().zip(&rust_buffers).enumerate()
        {
            let go_buf = &go_body[g_off..g_off + g_len];
            let rust_buf = &rust_body[r_off..r_off + r_len];
            if r_len == g_len {
                assert_eq!(rust_buf, go_buf, "{name}: buffer {i}");
            } else {
                assert!(
                    r_len < g_len && g_len <= 64,
                    "{name}: buffer {i} {g_len} vs {r_len}"
                );
                assert_eq!(rust_buf, &go_buf[..r_len], "{name}: bitmap {i}");
                assert!(
                    go_buf[r_len..].iter().all(|b| *b == 0),
                    "{name}: bitmap {i} tail"
                );
            }
        }
        assert!(rust_bytes.len() <= go_bytes.len(), "{name}: total length");
        if go_buffers == rust_buffers {
            assert_eq!(rust_body, go_body, "{name}: body");
        }
    }
}

#[test]
fn record_batch_keeps_the_schema_unless_the_timestamp_is_null() {
    for case in cases("record_batch/corpus.json") {
        let name = case["name"].as_str().unwrap();
        let schema = project_arrow_schema(&denied_set(&case["denied"]));
        let record = build_record_batch(&rows_of(&case), &schema).unwrap();
        let null_timestamps = record.column(0).null_count();
        if null_timestamps == 0 {
            assert_eq!(record.schema().as_ref(), &schema, "{name}");
        } else {
            // Go's NULL in the non-nullable timestamp: only that field turns nullable.
            assert!(
                matches!(
                    name,
                    "empty_row" | "nil_row" | "explicit_nil_values" | "wrong_typed_scalars"
                ),
                "{name}"
            );
            assert_eq!(record.schema().as_ref(), &relaxed(&schema), "{name}");
            assert!(!schema.field(0).is_nullable());
        }
    }
}

#[test]
fn record_batch_accepts_queued_rows_and_unknown_columns() {
    let rows: Vec<gadk_agentanalytics::row::QueuedRow> = rows_of(&json!({"rows": [
        {"timestamp": {"t": "ts", "v": "2026-03-04T05:06:07Z"}, "agent": {"t": "str", "v": "a"}}
    ]}))
    .into_iter()
    .map(Into::into)
    .collect();
    let schema = Schema::new(vec![
        Field::new("agent", arrow_schema::DataType::Utf8, true),
        Field::new("not_a_go_column", arrow_schema::DataType::Utf8, true),
    ]);
    let record = build_record_batch(&rows, &schema).unwrap();
    assert_eq!(
        decoded_rows(&record),
        vec![json!({"agent": "a", "not_a_go_column": null})]
    );
}

#[test]
fn record_batch_rejects_a_known_column_of_the_wrong_type() {
    let schema = Schema::new(vec![Field::new(
        "content_parts",
        arrow_schema::DataType::Utf8,
        true,
    )]);
    let err = build_record_batch::<Row>(&[Row::new()], &schema).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Schema error: unexpected Arrow type for field content_parts: Utf8"
    );
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Logger for Recorder {
    fn printf(&self, args: std::fmt::Arguments<'_>) {
        self.0.lock().unwrap().push(args.to_string());
    }
    fn println(&self, args: std::fmt::Arguments<'_>) {
        self.0.lock().unwrap().push(args.to_string());
    }
}

#[test]
fn serialize_record_logs_nothing_on_success() {
    let logger = Recorder::default();
    let schema = project_arrow_schema(&BTreeSet::new());
    let record = build_record_batch::<Row>(&[], &schema).unwrap();
    let bytes = serialize_record(&record, Some(&logger)).unwrap();
    assert_eq!(bytes[..4], [0xff, 0xff, 0xff, 0xff], "continuation marker");
    assert_eq!(decode(&schema, &bytes).num_rows(), 0);
    assert!(logger.0.lock().unwrap().is_empty());
}

#[test]
fn estimate_row_bytes_matches_go() {
    let cases = cases("record_batch/estimate_row_bytes.json");
    assert_eq!(cases.len(), 13);
    for case in cases {
        let row = row_from_spec(&case["row"]).unwrap();
        assert_eq!(
            estimate_row_bytes(&row) as u64,
            case["bytes"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn split_batch_by_estimated_bytes_matches_go() {
    let cases = cases("record_batch/split_batch.json");
    assert_eq!(cases.len(), 13);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let rows = rows_of(&case);
        let estimates: Vec<u64> = rows.iter().map(|r| estimate_row_bytes(r) as u64).collect();
        assert_eq!(json!(estimates), case["estimates"], "{name}: estimates");
        let limit = case["limit"].as_i64().unwrap();
        // Go's `limit <= 0` is the only chunk; a Rust `usize` limit has only 0.
        let limit = usize::try_from(limit).unwrap_or(0);
        let chunks = split_batch_by_estimated_bytes(&rows, limit);
        let lens: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
        assert_eq!(json!(lens), case["chunk_lens"], "{name}: chunk_lens");
        // Consecutive sub-slices of the batch, in order.
        let flat: Vec<&Row> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(flat, rows.iter().collect::<Vec<_>>(), "{name}: order");
    }
}

#[test]
fn split_batch_returns_the_batch_itself_when_it_fits() {
    let rows = vec![Row::new(); 3];
    let chunks = split_batch_by_estimated_bytes(&rows, MAX_APPEND_REQUEST_BYTES);
    assert_eq!(chunks.len(), 1);
    assert!(std::ptr::eq(chunks[0], rows.as_slice()));
}
