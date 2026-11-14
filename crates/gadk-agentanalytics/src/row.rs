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

//! The rows of ADK-Go `plugin/agentanalytics` (`batch_processor.go`, `gcs_offloader.go`): Go's
//! `map[string]any` row is [`Row`], a map of [`RowValue`]s, and the side-channel keys Go keeps
//! in the same map (`_raw_parts`, `_summary_builder`, `_deferred_uploads`) are typed fields of
//! [`QueuedRow`].
//!
//! Every reader (the Arrow encoder, the size estimate, the upload orchestration) reads a value
//! through the accessors below, each of which accepts exactly what the matching Go type
//! assertion accepts: [`RowValue::as_str`] is Go `v.(string)`, [`RowValue::as_int64_like`] is
//! the `part_index` chain `v.(int64)` / `v.(float64)` (truncated) / `v.(int)`, and so on. Go's
//! `int` and `int64` are both [`RowValue::Int`] (Go reads them alike wherever it accepts
//! either); a Go value of a type no assertion accepts (`int32`, `[]any`, `map[string]string`,
//! `[]byte`, …) is [`RowValue::Other`]. A missing key and Go's nil map read alike: a lookup
//! misses, so an empty [`Row`] is Go's nil row.
//!
//! [`from_json_map`] / [`to_json_map`] bridge the 7b-1 helpers that build or rewrite rows as
//! `serde_json` maps (`object_ref`, `format_content_parts`, `ContentSummaryBuilder::apply_to_row`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{Map, Number, Value};

use crate::formatter::{ContentSummaryBuilder, PendingUpload};

/// A row (Go `map[string]any`): column name → value. Side-channel data lives in [`QueuedRow`].
pub type Row = BTreeMap<String, RowValue>;

/// A value of a [`Row`] (a Go `any` held by a row map).
#[derive(Debug, Clone, PartialEq)]
pub enum RowValue {
    /// Go `string`.
    Str(String),
    /// Go `time.Time`.
    Timestamp(DateTime<Utc>),
    /// Go `bool`.
    Bool(bool),
    /// Go `int64` or `int`.
    Int(i64),
    /// Go `float64`.
    Float(f64),
    /// Go `[]map[string]any` (`content_parts`); Go's nil element is an empty [`Row`].
    Parts(Vec<Row>),
    /// Go `map[string]any` (`object_ref`).
    Map(Row),
    /// Go `nil` (the key is present, its value nil).
    Null,
    /// A Go value of any other type, which every type assertion of the transports rejects;
    /// the text describes it (for `Debug` output only).
    Other(String),
}

impl RowValue {
    /// Go `v.(string)`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RowValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Go `v.(time.Time)`.
    pub fn as_timestamp(&self) -> Option<DateTime<Utc>> {
        match self {
            RowValue::Timestamp(t) => Some(*t),
            _ => None,
        }
    }

    /// Go `v.(bool)`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            RowValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Go `v.(int64)` (or `v.(int)`).
    pub fn as_int64(&self) -> Option<i64> {
        match self {
            RowValue::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Go `v.(float64)`.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            RowValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Go's `part_index` read (`buildRecordBatch`): `v.(int64)`, else `int64(v.(float64))`
    /// (truncated toward zero), else `int64(v.(int))`.
    ///
    /// A float outside the `i64` range (or NaN) converts as Rust's `as` does (saturating, NaN
    /// to 0); Go leaves that conversion to the platform (arm64 saturates likewise, amd64 yields
    /// `i64::MIN`).
    pub fn as_int64_like(&self) -> Option<i64> {
        match self {
            RowValue::Int(n) => Some(*n),
            #[allow(clippy::cast_possible_truncation)]
            RowValue::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    /// Go `v.([]map[string]any)`.
    pub fn as_parts(&self) -> Option<&[Row]> {
        match self {
            RowValue::Parts(parts) => Some(parts),
            _ => None,
        }
    }

    /// Go `v.([]map[string]any)`, for rewriting the entries in place.
    pub fn as_parts_mut(&mut self) -> Option<&mut Vec<Row>> {
        match self {
            RowValue::Parts(parts) => Some(parts),
            _ => None,
        }
    }

    /// Go `v.(map[string]any)`.
    pub fn as_map(&self) -> Option<&Row> {
        match self {
            RowValue::Map(m) => Some(m),
            _ => None,
        }
    }
}

impl From<&str> for RowValue {
    fn from(s: &str) -> Self {
        RowValue::Str(s.to_owned())
    }
}

impl From<String> for RowValue {
    fn from(s: String) -> Self {
        RowValue::Str(s)
    }
}

impl From<bool> for RowValue {
    fn from(b: bool) -> Self {
        RowValue::Bool(b)
    }
}

impl From<i64> for RowValue {
    fn from(n: i64) -> Self {
        RowValue::Int(n)
    }
}

impl From<f64> for RowValue {
    fn from(f: f64) -> Self {
        RowValue::Float(f)
    }
}

impl From<DateTime<Utc>> for RowValue {
    fn from(t: DateTime<Utc>) -> Self {
        RowValue::Timestamp(t)
    }
}

impl From<Vec<Row>> for RowValue {
    fn from(parts: Vec<Row>) -> Self {
        RowValue::Parts(parts)
    }
}

impl From<Row> for RowValue {
    fn from(m: Row) -> Self {
        RowValue::Map(m)
    }
}

/// Go's type-asserted lookups on a row map: `row[key].(T)` with the comma-ok form, `None` when
/// the key is missing or holds another type.
pub trait RowExt {
    /// Go `row[key].(string)`.
    fn str(&self, key: &str) -> Option<&str>;
    /// Go `row[key].(time.Time)`.
    fn timestamp(&self, key: &str) -> Option<DateTime<Utc>>;
    /// Go `row[key].(bool)`.
    fn bool(&self, key: &str) -> Option<bool>;
    /// Go `row[key].(int64)` (or `.(int)`).
    fn int64(&self, key: &str) -> Option<i64>;
    /// Go's `part_index` chain on `row[key]` ([`RowValue::as_int64_like`]).
    fn int64_like(&self, key: &str) -> Option<i64>;
    /// Go `row[key].([]map[string]any)`.
    fn parts(&self, key: &str) -> Option<&[Row]>;
    /// Go `row[key].([]map[string]any)`, mutable.
    fn parts_mut(&mut self, key: &str) -> Option<&mut Vec<Row>>;
    /// Go `row[key].(map[string]any)`.
    fn map(&self, key: &str) -> Option<&Row>;
}

impl RowExt for Row {
    fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(RowValue::as_str)
    }

    fn timestamp(&self, key: &str) -> Option<DateTime<Utc>> {
        self.get(key).and_then(RowValue::as_timestamp)
    }

    fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(RowValue::as_bool)
    }

    fn int64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(RowValue::as_int64)
    }

    fn int64_like(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(RowValue::as_int64_like)
    }

    fn parts(&self, key: &str) -> Option<&[Row]> {
        self.get(key).and_then(RowValue::as_parts)
    }

    fn parts_mut(&mut self, key: &str) -> Option<&mut Vec<Row>> {
        self.get_mut(key).and_then(RowValue::as_parts_mut)
    }

    fn map(&self, key: &str) -> Option<&Row> {
        self.get(key).and_then(RowValue::as_map)
    }
}

/// Anything that holds a [`Row`]: the encoder and the size estimate take `&[R]` of
/// [`Row`]s or [`QueuedRow`]s alike.
pub trait AsRow {
    /// The row.
    fn as_row(&self) -> &Row;
}

impl AsRow for Row {
    fn as_row(&self) -> &Row {
        self
    }
}

impl AsRow for QueuedRow {
    fn as_row(&self) -> &Row {
        &self.row
    }
}

impl<T: AsRow + ?Sized> AsRow for &T {
    fn as_row(&self) -> &Row {
        (**self).as_row()
    }
}

/// A content summary builder shared by a row and its deferred uploads (Go
/// `*contentSummaryBuilder`, a pointer both hold).
pub(crate) type SharedSummaryBuilder = Arc<Mutex<ContentSummaryBuilder>>;

/// A row queued for writing (Go's row map with its side-channel keys): `row` holds the
/// columns; `raw_parts` is Go's `_raw_parts` (payloads still to upload; Go's missing key is
/// an empty `Vec`), `summary_builder` Go's `_summary_builder`, `deferred` Go's
/// `_deferred_uploads`.
#[derive(Debug, Clone, Default)]
pub struct QueuedRow {
    /// The columns.
    pub row: Row,
    /// Go `_raw_parts` (`[]pendingUpload`).
    pub(crate) raw_parts: Vec<PendingUpload>,
    /// Go `_summary_builder` (`*contentSummaryBuilder`).
    pub(crate) summary_builder: Option<SharedSummaryBuilder>,
    /// Go `_deferred_uploads` (`*deferredRowUploads`).
    pub(crate) deferred: Option<DeferredRowUploads>,
}

impl QueuedRow {
    /// A row without side-channel data.
    pub fn new(row: Row) -> QueuedRow {
        QueuedRow {
            row,
            ..QueuedRow::default()
        }
    }
}

impl From<Row> for QueuedRow {
    fn from(row: Row) -> Self {
        QueuedRow::new(row)
    }
}

/// Go `deferredUpload`: a failed upload kept for a retry of its batch.
#[allow(dead_code)] // consumer: Task 10 (offload::uploads)
#[derive(Debug, Clone, Default)]
pub(crate) struct DeferredUpload {
    /// Go `upload`.
    pub(crate) upload: PendingUpload,
    /// Go `eventID`.
    pub(crate) event_id: String,
    /// Go `errSummary` (`safeUploadErrorSummary` of the failure).
    pub(crate) err_summary: String,
}

/// Go `deferredRowUploads`: the deferred uploads of a row, and what requeuing them needs to put
/// the row back in the form it was queued in.
#[allow(dead_code)] // consumer: Task 10 (offload::uploads)
#[derive(Debug, Clone, Default)]
pub(crate) struct DeferredRowUploads {
    /// Go `pending`.
    pub(crate) pending: Vec<DeferredUpload>,
    /// Go `builder`.
    pub(crate) builder: Option<SharedSummaryBuilder>,
    /// Go `nonDeferredTruncated`.
    pub(crate) non_deferred_truncated: bool,
}

/// The [`RowValue`] of a JSON value as the 7b-1 helpers mean it: strings, booleans and null as
/// such; an integer that fits `i64` is [`RowValue::Int`], any other number [`RowValue::Float`];
/// an object is [`RowValue::Map`]; an array of objects (or `null`s, Go's nil maps, which become
/// empty rows) — including the empty array — is [`RowValue::Parts`] (Go
/// `[]map[string]any`); any other array is [`RowValue::Other`].
pub fn from_json_value(v: &Value) -> RowValue {
    match v {
        Value::Null => RowValue::Null,
        Value::Bool(b) => RowValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => RowValue::Int(i),
            None => RowValue::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => RowValue::Str(s.clone()),
        Value::Object(m) => RowValue::Map(from_json_map(m)),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Object(m) => parts.push(from_json_map(m)),
                    Value::Null => parts.push(Row::new()),
                    _ => return RowValue::Other(v.to_string()),
                }
            }
            RowValue::Parts(parts)
        }
    }
}

/// The [`Row`] of a JSON object ([`from_json_value`] per entry).
pub fn from_json_map(m: &Map<String, Value>) -> Row {
    m.iter()
        .map(|(k, v)| (k.clone(), from_json_value(v)))
        .collect()
}

/// The JSON value of a [`RowValue`] (Go `encoding/json` of the Go value): a timestamp is its
/// RFC 3339 text with nanoseconds (Go `time.Time.MarshalJSON`), a non-finite float and
/// [`RowValue::Other`] (which has no JSON form here) are `null`.
pub fn to_json_value(v: &RowValue) -> Value {
    match v {
        RowValue::Str(s) => Value::String(s.clone()),
        RowValue::Timestamp(t) => Value::String(gadk_platform::go_format_rfc3339_nano(t)),
        RowValue::Bool(b) => Value::Bool(*b),
        RowValue::Int(n) => Value::from(*n),
        RowValue::Float(f) => Number::from_f64(*f).map_or(Value::Null, Value::Number),
        RowValue::Parts(parts) => Value::Array(
            parts
                .iter()
                .map(|p| Value::Object(to_json_map(p)))
                .collect(),
        ),
        RowValue::Map(m) => Value::Object(to_json_map(m)),
        RowValue::Null | RowValue::Other(_) => Value::Null,
    }
}

/// The JSON object of a [`Row`] ([`to_json_value`] per entry).
pub fn to_json_map(row: &Row) -> Map<String, Value> {
    row.iter()
        .map(|(k, v)| (k.clone(), to_json_value(v)))
        .collect()
}

/// Go `builder.applyToRow(row)` on a [`Row`]: runs the 7b-1
/// [`ContentSummaryBuilder::apply_to_row`] on a JSON map and copies the columns it writes
/// (`content`, `is_truncated`) into `row`. `apply_to_row` only writes, so the map starts empty.
#[allow(dead_code)] // consumer: Task 10 (offload::uploads)
pub(crate) fn apply_summary_to_row(builder: &ContentSummaryBuilder, row: &mut Row) {
    let mut written = Map::new();
    builder.apply_to_row(&mut written);
    for (k, v) in &written {
        row.insert(k.clone(), from_json_value(v));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::path::PathBuf;

    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::formatter::{MessageSummary, SummaryPiece, object_ref};
    use crate::testutil::row_from_spec;

    fn read_json(rel: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap()
    }

    /// One value of every variant.
    fn variants() -> Vec<(&'static str, RowValue)> {
        let part: Row = [("text".to_owned(), RowValue::from("t"))].into();
        vec![
            ("str", RowValue::from("s")),
            ("ts", RowValue::Timestamp(ts())),
            ("bool", RowValue::Bool(true)),
            ("int", RowValue::Int(7)),
            ("float", RowValue::Float(2.9)),
            ("parts", RowValue::Parts(vec![part.clone(), Row::new()])),
            ("map", RowValue::Map(part)),
            ("null", RowValue::Null),
            ("other", RowValue::Other("int32(5)".into())),
        ]
    }

    /// Go's type-assertion table: for each variant, which accessor accepts it.
    #[test]
    fn accessors_accept_exactly_what_go_asserts() {
        for (name, v) in variants() {
            let accepted: Vec<&str> = [
                ("as_str", v.as_str().is_some()),
                ("as_timestamp", v.as_timestamp().is_some()),
                ("as_bool", v.as_bool().is_some()),
                ("as_int64", v.as_int64().is_some()),
                ("as_float", v.as_float().is_some()),
                ("as_int64_like", v.as_int64_like().is_some()),
                ("as_parts", v.as_parts().is_some()),
                ("as_map", v.as_map().is_some()),
            ]
            .into_iter()
            .filter_map(|(accessor, ok)| ok.then_some(accessor))
            .collect();
            let want: &[&str] = match name {
                "str" => &["as_str"],
                "ts" => &["as_timestamp"],
                "bool" => &["as_bool"],
                "int" => &["as_int64", "as_int64_like"],
                "float" => &["as_float", "as_int64_like"],
                "parts" => &["as_parts"],
                "map" => &["as_map"],
                "null" | "other" => &[],
                _ => unreachable!(),
            };
            assert_eq!(accepted, want, "{name}");
        }
    }

    #[test]
    fn part_index_reads_truncate_floats_toward_zero() {
        assert_eq!(RowValue::Float(2.9).as_int64_like(), Some(2));
        assert_eq!(RowValue::Float(-2.9).as_int64_like(), Some(-2));
        assert_eq!(RowValue::Float(1e3).as_int64_like(), Some(1000));
        assert_eq!(RowValue::Float(f64::NAN).as_int64_like(), Some(0));
        assert_eq!(RowValue::Float(1e300).as_int64_like(), Some(i64::MAX));
        assert_eq!(
            RowValue::Int(9_007_199_254_740_993).as_int64_like(),
            Some(9_007_199_254_740_993)
        );
    }

    #[test]
    fn row_ext_looks_up_and_asserts() {
        let mut row: Row = [
            ("s".to_owned(), RowValue::from("x")),
            ("t".to_owned(), RowValue::Timestamp(ts())),
            ("b".to_owned(), RowValue::Bool(false)),
            ("i".to_owned(), RowValue::Int(3)),
            ("f".to_owned(), RowValue::Float(-1.5)),
            ("p".to_owned(), RowValue::Parts(vec![Row::new()])),
            ("m".to_owned(), RowValue::Map(Row::new())),
            ("n".to_owned(), RowValue::Null),
        ]
        .into();
        assert_eq!(row.str("s"), Some("x"));
        assert_eq!(row.str("missing"), None);
        assert_eq!(row.str("n"), None);
        assert_eq!(row.timestamp("t"), Some(ts()));
        assert_eq!(row.bool("b"), Some(false));
        assert_eq!(row.int64("i"), Some(3));
        assert_eq!(row.int64("f"), None);
        assert_eq!(row.int64_like("f"), Some(-1));
        assert_eq!(row.parts("p").map(<[Row]>::len), Some(1));
        assert_eq!(row.parts("m"), None);
        assert_eq!(row.map("m"), Some(&Row::new()));
        row.parts_mut("p")
            .unwrap()
            .push([("text".to_owned(), RowValue::from("y"))].into());
        assert_eq!(row.parts("p").unwrap()[1].str("text"), Some("y"));
        assert!(row.parts_mut("s").is_none());
    }

    /// What Go's `buildRecordBatch` writes for `row`, read through the accessors (the decoded
    /// form of `record_batch/corpus.json`): every column of `columns`, `null` for NULL,
    /// timestamps in microseconds (floor), `content_parts` an always-present list.
    fn decode_like_go(row: &Row, columns: &[String]) -> Value {
        let mut out = Map::new();
        for col in columns {
            let v = match col.as_str() {
                "timestamp" => row
                    .timestamp(col)
                    .map_or(Value::Null, |t| json!(t.timestamp_micros())),
                "is_truncated" => row.bool(col).map_or(Value::Null, Value::Bool),
                "content_parts" => Value::Array(
                    row.parts(col)
                        .unwrap_or_default()
                        .iter()
                        .map(decode_part_like_go)
                        .collect(),
                ),
                _ => row.str(col).map_or(Value::Null, Value::from),
            };
            out.insert(col.clone(), v);
        }
        Value::Object(out)
    }

    fn decode_part_like_go(part: &Row) -> Value {
        let s = |m: &Row, k: &str| m.str(k).map_or(Value::Null, Value::from);
        let object_ref = part.map("object_ref").map_or(Value::Null, |m| {
            json!({
                "uri": s(m, "uri"),
                "version": s(m, "version"),
                "authorizer": s(m, "authorizer"),
                "details": s(m, "details"),
            })
        });
        json!({
            "mime_type": s(part, "mime_type"),
            "uri": s(part, "uri"),
            "object_ref": object_ref,
            "text": s(part, "text"),
            "part_index": part.int64_like("part_index").map_or(Value::Null, Value::from),
            "part_attributes": s(part, "part_attributes"),
            "storage_mode": s(part, "storage_mode"),
        })
    }

    /// The accessors read every probed corpus row as Go's `buildRecordBatch` type assertions
    /// did (wrong-typed scalars NULL, missing / wrong-typed `content_parts` an empty list,
    /// `part_index` from `int64` / `float64` / `int`, nested `object_ref`, nil parts).
    #[test]
    fn accessors_match_gos_record_batch_reads() {
        let corpus = read_json("record_batch/corpus.json");
        let cases = corpus["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 24);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let rows: Vec<Row> = case["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|spec| row_from_spec(spec).unwrap())
                .collect();
            let decoded = case["decoded"].as_array().unwrap();
            assert_eq!(rows.len(), decoded.len(), "{name}");
            for (row, want) in rows.iter().zip(decoded) {
                let columns: Vec<String> = want.as_object().unwrap().keys().cloned().collect();
                assert_eq!(&decode_like_go(row, &columns), want, "{name}");
            }
        }
    }

    #[test]
    fn json_bridge_maps_values_as_the_7b1_helpers_mean_them() {
        let m = json!({
            "s": "x", "b": true, "i": -3, "big": 18_446_744_073_709_551_615_u64, "f": 1.5,
            "n": null, "parts": [{"text": "a"}, null], "empty": [], "list": [1, 2],
            "obj": {"uri": "gs://b/o"},
        });
        let row = from_json_map(m.as_object().unwrap());
        assert_eq!(row["s"], RowValue::from("x"));
        assert_eq!(row["b"], RowValue::Bool(true));
        assert_eq!(row["i"], RowValue::Int(-3));
        assert_eq!(
            row["big"],
            RowValue::Float(18_446_744_073_709_551_615_u64 as f64)
        );
        assert_eq!(row["f"], RowValue::Float(1.5));
        assert_eq!(row["n"], RowValue::Null);
        assert_eq!(
            row["parts"],
            RowValue::Parts(vec![
                [("text".to_owned(), RowValue::from("a"))].into(),
                Row::new()
            ])
        );
        assert_eq!(row["empty"], RowValue::Parts(Vec::new()));
        assert_eq!(row["list"], RowValue::Other("[1,2]".into()));
        assert_eq!(
            row["obj"],
            RowValue::Map([("uri".to_owned(), RowValue::from("gs://b/o"))].into())
        );
    }

    #[test]
    fn json_bridge_round_trips_and_writes_go_json_forms() {
        let m = json!({
            "s": "x", "b": false, "i": 9_007_199_254_740_993_i64, "f": -0.25, "n": null,
            "parts": [{"part_index": 0, "object_ref": {"uri": "gs://b/o"}}], "obj": {},
        });
        let m = m.as_object().unwrap();
        assert_eq!(&to_json_map(&from_json_map(m)), m);
        let t = Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap()
            + chrono::Duration::nanoseconds(1_500);
        assert_eq!(
            to_json_value(&RowValue::Timestamp(t)),
            json!("2026-03-04T05:06:07.0000015Z")
        );
        assert_eq!(to_json_value(&RowValue::Float(f64::NAN)), Value::Null);
        assert_eq!(to_json_value(&RowValue::Other("x".into())), Value::Null);
    }

    #[test]
    fn object_ref_bridges_into_a_map_value() {
        let r = from_json_value(&Value::Object(object_ref("gs://b/o", "{}", "")));
        let m = r.as_map().unwrap();
        assert_eq!(m.str("uri"), Some("gs://b/o"));
        assert_eq!(m.str("details"), Some("{}"));
        assert_eq!(m.get("authorizer"), None);
        assert_eq!(m.get("version"), None);
    }

    #[test]
    fn apply_summary_to_row_writes_content_and_is_truncated() {
        let mut builder = ContentSummaryBuilder {
            max_len: -1,
            messages: vec![MessageSummary {
                pieces: vec![SummaryPiece {
                    is_text: true,
                    text: "hello".into(),
                    ..SummaryPiece::default()
                }],
                ..MessageSummary::default()
            }],
            ..ContentSummaryBuilder::default()
        };
        let mut row: Row = [("event_id".to_owned(), RowValue::from("e"))].into();
        apply_summary_to_row(&builder, &mut row);
        // No rebuild needed: nothing is written.
        assert_eq!(row.len(), 1);
        builder.needs_rebuild = true;
        apply_summary_to_row(&builder, &mut row);
        assert_eq!(row.str("content"), Some(r#"{"text_summary":"hello"}"#));
        assert_eq!(row.bool("is_truncated"), Some(false));
        assert_eq!(row.str("event_id"), Some("e"));
    }

    #[test]
    fn queued_row_from_row_has_no_side_channel_data() {
        let row: Row = [("event_type".to_owned(), RowValue::from("X"))].into();
        let q = QueuedRow::from(row.clone());
        assert_eq!(q.row, row);
        assert!(q.raw_parts.is_empty());
        assert!(q.summary_builder.is_none());
        assert!(q.deferred.is_none());
        assert_eq!([&q].as_slice()[0].as_row(), &row);
        assert_eq!(row.as_row(), &q.row);
    }
}
