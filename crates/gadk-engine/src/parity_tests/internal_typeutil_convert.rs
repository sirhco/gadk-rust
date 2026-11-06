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

//! Port of Go internal/typeutil/convert_test.go.

use serde_json::{Map, Value};

use crate::tool::utils::{CompiledSchema, convert_with_schema, infer_schema};

/// Port of Go mustResolve[T]: jsonschema.For[T] + Resolve.
fn must_resolve<T: schemars::JsonSchema>() -> CompiledSchema {
    CompiledSchema::new(infer_schema::<T>()).expect("resolve")
}

// parity: adapted: Go unmarshals the raw `null` into a nil map; Rust deserializes the substituted `{}` (see convert_with_schema docs).
#[test]
fn test_convert_to_with_json_schema_nil_input_object_schema() {
    let schema = must_resolve::<Map<String, Value>>();
    let input: Option<Map<String, Value>> = None;
    let got: Map<String, Value> = convert_with_schema(&input, Some(&schema))
        .expect("nil input must satisfy an object schema");
    assert!(got.is_empty(), "got {got:?}, want empty map");
}

#[test]
fn test_convert_to_with_json_schema_scalar_inputs() {
    // subtest "string"
    let schema = must_resolve::<String>();
    let got: String = convert_with_schema(&"hello".to_string(), Some(&schema))
        .expect("string input against string schema");
    assert_eq!(got, "hello");
    // subtest "slice"
    let schema = must_resolve::<Vec<i32>>();
    let got: Vec<i32> = convert_with_schema(&vec![1, 2, 3], Some(&schema))
        .expect("slice input against array schema");
    assert_eq!(got, vec![1, 2, 3]);
}

#[test]
fn test_convert_to_with_json_schema_nil_input_non_object_schema() {
    let schema = must_resolve::<String>();
    let input: Option<String> = None;
    let got: Result<Option<String>, String> = convert_with_schema(&input, Some(&schema));
    assert!(
        got.is_err(),
        "expected validation error for null against string schema"
    );
}

#[test]
fn test_convert_to_with_json_schema_type_mismatch_still_fails() {
    let schema = must_resolve::<Map<String, Value>>();
    let got: Result<Map<String, Value>, String> =
        convert_with_schema(&"not-an-object".to_string(), Some(&schema));
    assert!(
        got.is_err(),
        "expected validation error for string against object schema"
    );
}

// parity: adapted: Rust `Map` cannot deserialize from `null` (Go yields a nil map), so the output type is `Option<Map>` and `None` stands for Go's nil.
#[test]
fn test_convert_to_with_json_schema_no_schema_skips_validation() {
    let input: Option<Map<String, Value>> = None;
    let got: Option<Map<String, Value>> =
        convert_with_schema(&input, None).expect("nil schema should skip validation");
    assert_eq!(got, None);
}
