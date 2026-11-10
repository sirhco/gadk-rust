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

//! Ports of ADK-Go `internal/telemetry/converters_test.go`. Go's `any` is a `serde_json::Value`.

use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

use crate::telemetry::converters::{AttrValue, from_log_value, json_number, to_log_value};

// parity: adapted: Go's "json number beyond float64 keeps its text" input is a json.Number, which a serde_json::Value cannot hold, so that case goes through json_number (toLogValue's json.Number branch); Go's "fallback for unsupported type" case (a struct, formatted with %v) has no counterpart, as every serde_json::Value is a JSON type
#[rstest]
#[case::nil(json!(null), json!(null))]
#[case::string(json!("hello"), json!("hello"))]
#[case::bool(json!(true), json!(true))]
#[case::float64(json!(123.456), json!(123.456))]
#[case::int_to_int64(json!(123), json!(123))]
#[case::slice_of_mixed_types(json!([1.0, true, "foo"]), json!([1.0, true, "foo"]))]
#[case::map(json!({"foo": "bar", "baz": 123.0}), json!({"foo": "bar", "baz": 123.0}))]
#[case::nested_structure(json!({"list": [{"a": 1.0}]}), json!({"list": [{"a": 1.0}]}))]
fn test_converters_round_trip(#[case] val: Value, #[case] want: Value) {
    let got = from_log_value(&to_log_value(&val));
    assert_eq!(got, want);
    // Go's int and float64 stay apart.
    if let (Value::Number(g), Value::Number(w)) = (&got, &want) {
        assert_eq!(g.is_f64(), w.is_f64());
    }
}

#[test]
fn test_converters_round_trip_json_number_beyond_float64_keeps_its_text() {
    let text = "9".repeat(401);
    assert_eq!(json_number(&text), AttrValue::String(text.clone()));
    assert_eq!(from_log_value(&json_number(&text)), Value::String(text));
}

// parity: adapted: Go returns typed slices ([]bool, []int64, ...); a serde_json::Value is a JSON array of the same elements (a byte slice an array of its bytes)
#[rstest]
#[case::bool(AttrValue::BoolSlice(vec![true, false]), json!([true, false]))]
#[case::int64(AttrValue::Int64Slice(vec![1, 2]), json!([1, 2]))]
#[case::float64(AttrValue::Float64Slice(vec![1.5, 2.5]), json!([1.5, 2.5]))]
#[case::string(AttrValue::StringSlice(vec!["a".into(), "b".into()]), json!(["a", "b"]))]
#[case::bytes(AttrValue::ByteSlice(vec![1, 2]), json!([1, 2]))]
fn test_from_log_value_typed_slices(#[case] val: AttrValue, #[case] want: Value) {
    assert_eq!(from_log_value(&val), want);
}
