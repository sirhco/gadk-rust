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

//! Golden tests of `schema.rs` (ADK-Go `plugin/agentanalytics/schema.go`) against the
//! `scripts/gen-go-agentanalytics` probe fixtures: `schema/events_schema.json`,
//! `schema/projections.json` (spec R2) and `arrow/` (spec R3).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::PathBuf;

use arrow_ipc::reader::StreamReader;
use arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use gadk_agentanalytics::{
    TableSchema, arrow_schema, events_schema, project_arrow_schema, project_events_schema,
    serialized_arrow_schema,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

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

/// The REST `TableSchema` JSON the bigquery client sends for `fields`.
fn rest_json(fields: Vec<gadk_agentanalytics::FieldSchema>) -> Value {
    serde_json::to_value(TableSchema { fields }).unwrap()
}

#[test]
fn events_schema_matches_go_rest_shape() {
    let want = &read_json("schema/events_schema.json")["schema"];
    assert_eq!(&rest_json(events_schema()), want);
    // The fixture decodes back into the same schema (FieldSchema's Deserialize = Go bqToFieldSchema).
    let decoded: TableSchema = serde_json::from_value(want.clone()).unwrap();
    assert_eq!(decoded.fields, events_schema());
}

#[test]
fn projected_events_schemas_match_go() {
    let cases = cases("schema/projections.json");
    assert_eq!(cases.len(), 7);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let denied = denied_set(&case["denied"]);
        assert_eq!(
            rest_json(project_events_schema(&denied)),
            case["schema"],
            "projection {name}"
        );
    }
}

/// Go `arrow.DataType.String()` of the types the events schema uses.
fn go_type_string(dt: &DataType) -> String {
    match dt {
        DataType::Utf8 => "utf8".to_owned(),
        DataType::Int64 => "int64".to_owned(),
        DataType::Boolean => "bool".to_owned(),
        DataType::Timestamp(TimeUnit::Microsecond, Some(tz)) => format!("timestamp[us, tz={tz}]"),
        DataType::List(f) => format!(
            "list<{}: {}{}>",
            f.name(),
            go_type_string(f.data_type()),
            if f.is_nullable() { ", nullable" } else { "" }
        ),
        DataType::Struct(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}{}",
                        f.name(),
                        go_type_string(f.data_type()),
                        if f.is_nullable() { " nullable" } else { "" }
                    )
                })
                .collect();
            format!("struct<{}>", inner.join(", "))
        }
        other => panic!("type {other} is not in the events schema"),
    }
}

/// The probe's `Type` description of an arrow-rs data type.
fn describe_type(dt: &DataType) -> Value {
    let mut out = match dt {
        DataType::Utf8 => json!({"id": "utf8"}),
        DataType::Int64 => json!({"id": "int64"}),
        DataType::Boolean => json!({"id": "bool"}),
        DataType::Timestamp(TimeUnit::Microsecond, Some(tz)) => {
            json!({"id": "timestamp", "unit": "us", "timezone": tz.as_ref()})
        }
        DataType::List(f) => json!({"id": "list", "element": describe_field(f)}),
        DataType::Struct(fields) => json!({
            "id": "struct",
            "fields": fields.iter().map(|f| describe_field(f)).collect::<Vec<_>>(),
        }),
        other => panic!("type {other} is not in the events schema"),
    };
    out["go_string"] = Value::String(go_type_string(dt));
    out
}

/// Metadata as the probe's ordered `[key, value]` pairs (arrow-rs keeps a `HashMap`; sorted by
/// key, which is Go's order for the single-key maps the schema uses).
fn describe_metadata<'a>(md: impl IntoIterator<Item = (&'a String, &'a String)>) -> Value {
    let mut pairs: Vec<(&String, &String)> = md.into_iter().collect();
    pairs.sort();
    Value::Array(pairs.into_iter().map(|(k, v)| json!([k, v])).collect())
}

/// The probe's `Field` description of an arrow-rs field.
fn describe_field(f: &Field) -> Value {
    json!({
        "name": f.name(),
        "nullable": f.is_nullable(),
        "metadata": describe_metadata(f.metadata()),
        "type": describe_type(f.data_type()),
    })
}

/// The probe's schema description (`ipc.NewReader` schema) of an arrow-rs schema.
fn describe_schema(s: &ArrowSchema) -> Value {
    json!({
        "endianness": "little",
        "metadata": describe_metadata(s.metadata()),
        "fields": s.fields().iter().map(|f| describe_field(f)).collect::<Vec<_>>(),
    })
}

/// Decodes Arrow IPC stream bytes that hold a schema and no batches.
fn decode_stream(bytes: &[u8]) -> ArrowSchema {
    let mut reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
    let schema = reader.schema().as_ref().clone();
    assert!(reader.next().is_none(), "stream holds no record batches");
    schema
}

#[test]
fn arrow_schema_matches_go_description() {
    let cases = cases("arrow/schemas.json");
    assert_eq!(cases.len(), 8);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let rust = if name == "serialized" {
            arrow_schema()
        } else {
            project_arrow_schema(&denied_set(&case["denied"]))
        };
        assert_eq!(describe_schema(&rust), case["schema"], "{name}");
    }
}

#[test]
fn go_arrow_stream_bytes_decode_to_rust_schema() {
    for case in cases("arrow/schemas.json") {
        let name = case["name"].as_str().unwrap();
        let file = case["file"].as_str().unwrap();
        let go_bytes = std::fs::read(fixtures().join("arrow").join(file)).unwrap();
        assert_eq!(
            go_bytes.len() as u64,
            case["byte_len"].as_u64().unwrap(),
            "{name}"
        );
        assert!(go_bytes.ends_with(&EOS), "{name}: Go stream ends with EOS");
        let want = if name == "serialized" {
            arrow_schema()
        } else {
            project_arrow_schema(&denied_set(&case["denied"]))
        };
        assert_eq!(decode_stream(&go_bytes), want, "{name}");
    }
}

#[test]
fn serialized_arrow_schema_decodes_like_go_bytes() {
    let rust_bytes = serialized_arrow_schema().unwrap();
    let go_bytes = std::fs::read(fixtures().join("arrow/events.arrows")).unwrap();
    // Both streams are one framed schema message (continuation marker first) then EOS.
    assert_eq!(&rust_bytes[..4], &EOS[..4]);
    assert!(rust_bytes.ends_with(&EOS));
    assert_eq!(decode_stream(&rust_bytes), decode_stream(&go_bytes));
    assert_eq!(decode_stream(&rust_bytes), arrow_schema());
    // Pins `serialize_arrow_schema`'s ledgered divergence (7b-1): arrow-rs and arrow-go lay out
    // the schema flatbuffer differently (1864 vs 1880 bytes), so the bytes are not compared (R3 compares decoded
    // schemas).
    assert_eq!((rust_bytes.len(), go_bytes.len()), (1864, 1880));
    assert_ne!(rust_bytes, go_bytes);
}

#[test]
fn projected_none_stream_equals_serialized_stream() {
    let go_events = std::fs::read(fixtures().join("arrow/events.arrows")).unwrap();
    let go_none = std::fs::read(fixtures().join("arrow/projected_none.arrows")).unwrap();
    assert_eq!(go_events, go_none);
    assert_eq!(project_arrow_schema(&BTreeSet::new()), arrow_schema());
}
