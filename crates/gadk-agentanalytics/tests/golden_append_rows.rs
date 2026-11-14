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

//! Golden tests of `write/stream_writer.rs`'s AppendRows requests (ADK-Go
//! `plugin/agentanalytics/batch_processor.go` `newAppendRowsRequestLocked`,
//! `serializedRequestSize`, `streamWriterTraceIDForVersion`) against the
//! `scripts/gen-go-agentanalytics` probe fixtures `append_rows/` (spec R5): built from Go's own
//! writer schema and batch bytes with the probe's trace id, a request encodes to Go's bytes
//! exactly (so its size is Go's `serializedRequestSize`); with gadk's trace id the size moves
//! by the trace id's length only; built from Rust's schema and record batch it decodes to the
//! same Arrow data as Go's and is no larger. The routing header of the AppendRows stream is a
//! unit test of `stream_writer.rs` (the stream writer is crate-private).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::PathBuf;

use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::Schema;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gadk_agentanalytics::project_arrow_schema;
use gadk_agentanalytics::row::Row;
use gadk_agentanalytics::storagepb::AppendRowsRequest;
use gadk_agentanalytics::storagepb::append_rows_request::Rows;
use gadk_agentanalytics::testutil::row_from_spec;
use gadk_agentanalytics::write::arrow_batch::{build_record_batch, serialize_record};
use gadk_agentanalytics::write::stream_writer::{
    STREAM_WRITER_TRACE_ID_PREFIX, append_rows_request, stream_writer_trace_id,
    stream_writer_trace_id_for_version,
};
use pretty_assertions::assert_eq;
use prost::Message as _;
use serde_json::Value;

/// ADK-Go's `streamWriterTraceIDPrefix`.
const GO_TRACE_ID_PREFIX: &str = "google-adk-bq-logger-go";

/// Arrow IPC end-of-stream marker (continuation token, zero length).
const EOS: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_json(rel: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(rel)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn read_bytes(rel: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(rel)).unwrap()
}

fn cases(rel: &str) -> Vec<Value> {
    read_json(rel)["cases"].as_array().unwrap().clone()
}

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

/// A case's request built from Go's writer schema and batch bytes with `trace_id`.
fn request_from_go_bytes(case: &Value, trace_id: &str) -> AppendRowsRequest {
    let schema = read_bytes(case["writer_schema_file"].as_str().unwrap());
    let batch = case["batch_file"]
        .as_str()
        .map(read_bytes)
        .unwrap_or_default();
    append_rows_request(
        case["stream_name"].as_str().unwrap(),
        trace_id,
        &schema,
        &batch,
        case["num_rows"].as_i64().unwrap(),
        case["offset"].as_i64(),
    )
}

/// The writer schema and record batch bytes of an Arrow request.
fn arrow_payload(req: &AppendRowsRequest) -> (&[u8], &[u8]) {
    match &req.rows {
        Some(Rows::ArrowRows(data)) => (
            data.writer_schema
                .as_ref()
                .map_or(&[][..], |s| &s.serialized_schema),
            data.rows
                .as_ref()
                .map_or(&[][..], |r| &r.serialized_record_batch),
        ),
        other => panic!("not an Arrow request: {other:?}"),
    }
}

#[allow(deprecated)]
fn row_count(req: &AppendRowsRequest) -> i64 {
    match &req.rows {
        Some(Rows::ArrowRows(data)) => data.rows.as_ref().map_or(0, |r| r.row_count),
        _ => 0,
    }
}

/// The top-level fields of an encoded message as `(field number, wire type, encoded value)`,
/// sorted by field number. Go's `proto.Marshal` writes fields in field-number order, prost in
/// declaration order with the `rows` oneof (field 5) last, after `trace_id` (field 6); a
/// protobuf parser accepts either, so the comparison is per field.
fn top_level_fields(mut buf: &[u8]) -> Vec<(u64, u64, Vec<u8>)> {
    fn varint(buf: &mut &[u8]) -> u64 {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = buf[0];
            *buf = &buf[1..];
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
        }
        v
    }
    let mut fields = Vec::new();
    while !buf.is_empty() {
        let key = varint(&mut buf);
        let (number, wire_type) = (key >> 3, key & 7);
        let value = match wire_type {
            0 => varint(&mut buf).to_le_bytes().to_vec(),
            2 => {
                let len = usize::try_from(varint(&mut buf)).unwrap();
                let (value, rest) = buf.split_at(len);
                buf = rest;
                value.to_vec()
            }
            other => panic!("unexpected wire type {other}"),
        };
        fields.push((number, wire_type, value));
    }
    fields.sort_by_key(|f| f.0);
    fields
}

#[test]
fn requests_from_go_bytes_encode_to_gos_requests() {
    let cases = cases("append_rows/requests.json");
    assert_eq!(cases.len(), 10);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let go_trace = case["trace_id"].as_str().unwrap();
        let want = read_bytes(&format!("append_rows/{}", case["file"].as_str().unwrap()));
        let req = request_from_go_bytes(case, go_trace);

        assert_eq!(
            top_level_fields(&req.encode_to_vec()),
            top_level_fields(&want),
            "{name}: fields"
        );
        let byte_len = case["byte_len"].as_u64().unwrap() as usize;
        assert_eq!(want.len(), byte_len, "{name}");
        assert_eq!(req.encoded_len(), byte_len, "{name}: size");
        assert_eq!(
            case["serialized_request_size"].as_u64().unwrap() as usize,
            byte_len,
            "{name}"
        );
        assert_eq!(
            AppendRowsRequest::decode(want.as_slice()).unwrap(),
            req,
            "{name}: decoded"
        );

        // The fields as the probe's protojson shows them.
        let pj = &case["protojson"];
        assert_eq!(
            pj["writeStream"].as_str().unwrap_or_default(),
            req.write_stream,
            "{name}"
        );
        assert_eq!(pj["traceId"], req.trace_id.as_str(), "{name}");
        assert_eq!(
            pj["offset"].as_str().map(|s| s.parse::<i64>().unwrap()),
            req.offset.map(|o| o.value),
            "{name}: offset"
        );
        let arrow = &pj["arrowRows"];
        assert_eq!(
            arrow["rows"]["rowCount"]
                .as_str()
                .map_or(0, |s| s.parse::<i64>().unwrap()),
            row_count(&req),
            "{name}: row count"
        );
        let (schema, batch) = arrow_payload(&req);
        assert_eq!(
            arrow["writerSchema"]["serializedSchema"],
            STANDARD.encode(schema),
            "{name}"
        );
        assert_eq!(
            arrow["rows"]["serializedRecordBatch"]
                .as_str()
                .unwrap_or_default(),
            STANDARD.encode(batch),
            "{name}"
        );
    }
}

#[test]
fn gadk_trace_id_changes_only_the_trace_id() {
    let rust_trace = stream_writer_trace_id();
    for case in cases("append_rows/requests.json") {
        let name = case["name"].as_str().unwrap();
        let go_trace = case["trace_id"].as_str().unwrap();
        let go_req = request_from_go_bytes(&case, go_trace);
        let req = request_from_go_bytes(&case, rust_trace);
        assert_eq!(req.trace_id, rust_trace, "{name}");
        assert_eq!(
            AppendRowsRequest {
                trace_id: go_trace.to_owned(),
                ..req.clone()
            },
            go_req,
            "{name}"
        );
        // Both trace ids are shorter than 128 bytes: the length prefix stays one byte.
        assert!(go_trace.len() < 128 && rust_trace.len() < 128);
        let byte_len = case["byte_len"].as_u64().unwrap() as usize;
        assert_eq!(
            req.encoded_len(),
            byte_len - go_trace.len() + rust_trace.len(),
            "{name}"
        );
    }
}

#[test]
fn trace_ids_match_gos_with_gadks_prefix() {
    let fixture = read_json("append_rows/trace_ids.json");
    for case in fixture["cases"].as_array().unwrap() {
        let version = case["version"].as_str().unwrap();
        let want = case["trace_id"].as_str().unwrap().replacen(
            GO_TRACE_ID_PREFIX,
            STREAM_WRITER_TRACE_ID_PREFIX,
            1,
        );
        assert_eq!(
            stream_writer_trace_id_for_version(version),
            want,
            "{version:?}"
        );
    }
    assert_eq!(
        fixture["probe_trace_id"].as_str().unwrap(),
        GO_TRACE_ID_PREFIX
    );
    assert_eq!(
        stream_writer_trace_id(),
        format!(
            "{STREAM_WRITER_TRACE_ID_PREFIX}/{}",
            env!("CARGO_PKG_VERSION")
        )
    );
}

/// arrow-rs's schema stream of `schema` (the crate's `serialize_arrow_schema`).
fn rust_schema_bytes(schema: &Schema) -> Vec<u8> {
    let mut writer = StreamWriter::try_new(Vec::new(), schema).unwrap();
    writer.finish().unwrap();
    writer.into_inner().unwrap()
}

/// Decodes a request's Arrow payload as one stream: the writer schema stream without its
/// end-of-stream marker, the record batch message, the marker.
fn decode_payload(req: &AppendRowsRequest) -> (Schema, RecordBatch) {
    let (schema, batch) = arrow_payload(req);
    let mut stream = schema[..schema.len() - EOS.len()].to_vec();
    stream.extend_from_slice(batch);
    stream.extend_from_slice(&EOS);
    let mut reader = StreamReader::try_new(Cursor::new(stream), None).unwrap();
    let schema = reader.schema().as_ref().clone();
    let batch = reader.next().expect("one record batch").unwrap();
    assert!(reader.next().is_none(), "exactly one record batch");
    (schema, batch)
}

#[test]
fn requests_from_rust_batches_decode_like_gos() {
    let corpus = cases("record_batch/corpus.json");
    let mut compared = 0;
    for case in cases("append_rows/requests.json") {
        let name = case["name"].as_str().unwrap();
        let Some(batch_file) = case["batch_file"].as_str() else {
            continue;
        };
        let corpus_name = batch_file
            .trim_start_matches("record_batch/")
            .trim_end_matches(".batch");
        let spec = corpus
            .iter()
            .find(|c| c["name"] == corpus_name)
            .unwrap_or_else(|| panic!("{name}: no corpus case {corpus_name}"));
        let rows: Vec<Row> = spec["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| row_from_spec(s).unwrap())
            .collect();
        let schema = project_arrow_schema(&denied_set(&case["denied"]));
        let record = build_record_batch(&rows, &schema).unwrap();
        let batch = serialize_record(&record, None).unwrap();
        let num_rows = i64::try_from(record.num_rows()).unwrap();
        assert_eq!(num_rows, case["num_rows"].as_i64().unwrap(), "{name}");

        let go_trace = case["trace_id"].as_str().unwrap();
        let go_req = request_from_go_bytes(&case, go_trace);
        let req = append_rows_request(
            case["stream_name"].as_str().unwrap(),
            go_trace,
            &rust_schema_bytes(&schema),
            &batch,
            num_rows,
            case["offset"].as_i64(),
        );

        // Everything but the Arrow bytes is Go's.
        assert_eq!(req.write_stream, go_req.write_stream, "{name}");
        assert_eq!(req.trace_id, go_req.trace_id, "{name}");
        assert_eq!(req.offset, go_req.offset, "{name}");
        assert_eq!(row_count(&req), row_count(&go_req), "{name}");
        // The Arrow payload decodes to Go's schema and rows.
        let (rust_schema, rust_batch) = decode_payload(&req);
        let (go_schema, go_batch) = decode_payload(&go_req);
        assert_eq!(rust_schema, go_schema, "{name}: writer schema");
        assert_eq!(rust_batch, go_batch, "{name}: record batch");
        // arrow-rs's record batch message is never longer than arrow-go's (ledgered by
        // Task 6); its schema stream may be longer or shorter (ledgered by 7b-1).
        let (rust_schema_bytes, rust_batch_bytes) = arrow_payload(&req);
        let (go_schema_bytes, go_batch_bytes) = arrow_payload(&go_req);
        assert!(rust_batch_bytes.len() <= go_batch_bytes.len(), "{name}");
        assert_eq!(
            req.encoded_len() as i64 - go_req.encoded_len() as i64,
            (rust_schema_bytes.len() as i64 - go_schema_bytes.len() as i64)
                + (rust_batch_bytes.len() as i64 - go_batch_bytes.len() as i64),
            "{name}: the size moves by the Arrow byte differences only"
        );
        compared += 1;
    }
    assert_eq!(compared, 9);
}
