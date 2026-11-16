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

//! Port of ADK-Go `plugin/agentanalytics/json_formatter_test.go` (ledger 7b-1).
//! The redaction tests of that file (`TestSanitizeURI`, `TestRedactErrorMessage`,
//! `TestSanitizeRawText`) are in `redaction.rs`.
//!
//! Go's reflection walk has values Rust cannot build (cyclic maps / slices / pointers, funcs,
//! channels, panicking `MarshalJSON`, nil `*Part` entries, struct embedding); those cases are
//! left out per case with a "skipped case (ledger 7b-1)" comment, or adapted to the serde
//! value that serializes to the same JSON.

// Table tests hold heterogeneous tuples.
#![allow(clippy::type_complexity)]

mod credentials;
mod redaction;
mod summary_caps;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    FunctionResponseBlob, FunctionResponseFileData, FunctionResponsePart,
    FunctionResponseScheduling, Language, Outcome, Part,
};
use pretty_assertions::assert_eq;
use serde::ser::Error as _;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::bigquery::fake::{check_json, lookup};
use crate::formatter::{
    BINARY_DATA_PLACEHOLDER, GoAny, JSON_NUMBER_TOKEN, MAX_DEPTH_PLACEHOLDER, MAX_SANITIZE_DEPTH,
    MAX_SANITIZE_NODES, MessageSummary, PartsCollector, PartsOptions, PendingUpload,
    SANITIZE_BUDGET_PLACEHOLDER, TRUNCATION_SUFFIX, UPLOAD_FAILED_TEXT, content_summary,
    format_content_parts, format_content_parts_with, format_response_content, smart_truncate,
    smart_truncate_with,
};
use crate::redaction::{REDACTED_URI_PLACEHOLDER, REDACTED_VALUE};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Go `[]byte`: serializes through `serialize_bytes`.
struct GoBytes<'a>(&'a [u8]);

impl Serialize for GoBytes<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.0)
    }
}

fn st(obj: &(impl Serialize + ?Sized), max_len: i64) -> (String, bool) {
    let (b, t) = smart_truncate(obj, max_len).unwrap();
    (String::from_utf8(b).unwrap(), t)
}

fn st_with(obj: &(impl Serialize + ?Sized), max_len: i64, redact: bool) -> (String, bool) {
    let (b, t) = smart_truncate_with(obj, max_len, redact).unwrap();
    (String::from_utf8(b).unwrap(), t)
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// A `Map` from a `json!` object literal.
fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn text_part(text: impl Into<String>) -> Part {
    Part {
        text: text.into(),
        ..Part::default()
    }
}

fn inline_part(mime_type: &str, data: &[u8]) -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: mime_type.to_owned(),
            data: Bytes::copy_from_slice(data),
            ..Blob::default()
        }),
        ..Part::default()
    }
}

fn file_part(mime_type: &str, uri: &str) -> Part {
    Part {
        file_data: Some(FileData {
            mime_type: mime_type.to_owned(),
            file_uri: uri.to_owned(),
            ..FileData::default()
        }),
        ..Part::default()
    }
}

fn call_part(call: FunctionCall) -> Part {
    Part {
        function_call: Some(call),
        ..Part::default()
    }
}

fn response_part(resp: FunctionResponse) -> Part {
    Part {
        function_response: Some(resp),
        ..Part::default()
    }
}

fn code_part(code: &str, language: Option<Language>) -> Part {
    Part {
        executable_code: Some(ExecutableCode {
            code: code.to_owned(),
            language,
            ..ExecutableCode::default()
        }),
        ..Part::default()
    }
}

fn result_part(output: &str, outcome: Option<Outcome>) -> Part {
    Part {
        code_execution_result: Some(CodeExecutionResult {
            output: output.to_owned(),
            outcome,
            ..CodeExecutionResult::default()
        }),
        ..Part::default()
    }
}

fn content(parts: Vec<Part>) -> Content {
    Content {
        parts,
        ..Content::default()
    }
}

/// Go `singlePart(part)`.
fn single_part(part: Part) -> Content {
    content(vec![part])
}

/// Go `collectParts(opts, contents...)`.
fn collect_parts(opts: PartsOptions, contents: &[Option<&Content>]) -> PartsCollector {
    let mut c = PartsCollector::new(opts);
    for (i, content) in contents.iter().enumerate() {
        c.add_content(*content, i);
    }
    c
}

/// Go `partAttributes(t, entry)`.
fn part_attributes(entry: &Map<String, Value>) -> Value {
    let s = entry
        .get("part_attributes")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("part_attributes is not a JSON string; entry: {entry:?}"));
    serde_json::from_str(s).unwrap_or_else(|e| panic!("part_attributes is not JSON: {e}\n{s}"))
}

fn opts(max_len: i64) -> PartsOptions {
    PartsOptions {
        max_len,
        ..PartsOptions::default()
    }
}

fn offload_opts(max_len: i64, inline_limit: i64) -> PartsOptions {
    PartsOptions {
        max_len,
        offload: true,
        inline_limit,
        ..PartsOptions::default()
    }
}

fn reject_all() -> Option<Arc<dyn Fn(i64) -> bool + Send + Sync>> {
    Some(Arc::new(|_| false))
}

fn entry(v: Value) -> Map<String, Value> {
    obj(v)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn test_format_content_parts() {
    struct Case {
        name: &'static str,
        content: Option<Content>,
        max_length: i64,
        want: Vec<Value>,
    }
    let cases = vec![
        Case {
            name: "nil content",
            content: None,
            max_length: 10,
            want: vec![],
        },
        Case {
            name: "empty parts",
            content: Some(content(vec![])),
            max_length: 10,
            want: vec![],
        },
        Case {
            name: "various parts",
            content: Some(content(vec![
                text_part("this is a very long text"),
                inline_part("image/png", &[]),
                file_part("video/mp4", "gs://bucket/file.mp4"),
                text_part("short"),
            ])),
            max_length: 10,
            want: vec![
                json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "text/plain", "text": "this is a ...[TRUNCATED]"}),
                json!({"part_index": 1, "storage_mode": "INLINE", "mime_type": "image/png", "text": "[BINARY DATA]"}),
                json!({"part_index": 2, "storage_mode": "EXTERNAL_URI", "mime_type": "video/mp4", "uri": "gs://bucket/file.mp4"}),
                json!({"part_index": 3, "storage_mode": "INLINE", "mime_type": "text/plain", "text": "short"}),
            ],
        },
        Case {
            name: "file URI with credentials",
            content: Some(single_part(file_part(
                "image/png",
                "https://example.com/i.png?key=abc&alt=media",
            ))),
            max_length: 10,
            want: vec![
                json!({"part_index": 0, "storage_mode": "EXTERNAL_URI", "mime_type": "image/png", "uri": "https://example.com/i.png?key=%5BREDACTED%5D&alt=media"}),
            ],
        },
        Case {
            name: "text part with JSON credentials sanitized before truncation",
            content: Some(single_part(text_part(
                r#"{"api_key":"secret","note":"0123456789"}"#,
            ))),
            max_length: 25,
            want: vec![
                json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "text/plain", "text": r#"{"api_key":"[REDACTED]","...[TRUNCATED]"#}),
            ],
        },
        Case {
            name: "structured function and code execution parts",
            content: Some(content(vec![
                call_part(FunctionCall {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    args: obj(json!({"api_key": "secret", "q": "weather"})),
                    ..FunctionCall::default()
                }),
                response_part(FunctionResponse {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    response: obj(json!({"token": "tok", "answer": "sunny"})),
                    ..FunctionResponse::default()
                }),
                code_part("print(1)", Some(Language::Python)),
                result_part("1\n", Some(Outcome::Ok)),
                Part::default(),
            ])),
            max_length: 200,
            want: vec![
                json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "application/json", "text": "Function: lookup",
                    "part_attributes": r#"{"function_call":{"args":{"api_key":"[REDACTED]","q":"weather"},"id":"call-1","name":"lookup"},"function_name":"lookup"}"#}),
                json!({"part_index": 1, "storage_mode": "INLINE", "mime_type": "application/json", "text": "Function response: lookup",
                    "part_attributes": r#"{"function_response":{"id":"call-1","name":"lookup","response":{"answer":"sunny","token":"[REDACTED]"}}}"#}),
                json!({"part_index": 2, "storage_mode": "INLINE", "mime_type": "text/plain", "text": "print(1)",
                    "part_attributes": r#"{"executable_code":{"code":"print(1)","language":"PYTHON"}}"#}),
                json!({"part_index": 3, "storage_mode": "INLINE", "mime_type": "text/plain", "text": "1\n",
                    "part_attributes": r#"{"code_execution_result":{"outcome":"OUTCOME_OK","output":"1\n"}}"#}),
                json!({"part_index": 4, "storage_mode": "INLINE", "mime_type": "text/plain", "part_attributes": "{}"}),
            ],
        },
    ];
    for tt in cases {
        let got = format_content_parts(tt.content.as_ref(), tt.max_length);
        let got: Vec<Value> = got.into_iter().map(Value::Object).collect();
        assert_eq!(got, tt.want, "{}", tt.name);
    }
}

#[derive(Serialize)]
struct NestedStruct {
    #[serde(rename = "Value")]
    value: String,
}

/// Go `testStruct`: `json:"-"` is `#[serde(skip)]`, untagged fields keep their Go names.
#[derive(Serialize)]
struct TestStruct {
    #[serde(rename = "exported_string")]
    exported_string: String,
    #[serde(skip)]
    #[allow(dead_code)]
    ignored_field: String,
    #[serde(rename = "UnexportedField")]
    unexported_field: String,
    #[serde(rename = "ExportedInt")]
    exported_int: i64,
    #[serde(rename = "nested", skip_serializing_if = "Option::is_none")]
    nested: Option<NestedStruct>,
    #[serde(rename = "ExportedStringNoTag")]
    exported_string_no_tag: String,
}

// parity: adapted: Go values become serde values with the same JSON (testStruct via serde renames, `map[int]string` a `BTreeMap<i64, _>`, `[1]string` an array).
#[test]
fn test_smart_truncate() {
    let cases: Vec<(&str, Box<dyn erased::Ser>, i64, &str, bool)> = vec![
        ("nil", Box::new(()), 10, "null", false),
        ("short string", Box::new("short"), 10, r#""short""#, false),
        (
            "simple string",
            Box::new("this is a very long string"),
            10,
            r#""this is a ...[TRUNCATED]""#,
            true,
        ),
        (
            "map with strings",
            Box::new(json!({"key1": "long long long string", "key2": "short"})),
            10,
            r#"{"key1":"long long ...[TRUNCATED]","key2":"short"}"#,
            true,
        ),
        (
            "slice of strings",
            Box::new(json!(["long long long string", "short"])),
            10,
            r#"["long long ...[TRUNCATED]","short"]"#,
            true,
        ),
        (
            "complex struct",
            Box::new(TestStruct {
                exported_string: "long string here".into(),
                ignored_field: "should not appear".into(),
                unexported_field: "should not appear".into(),
                exported_int: 42,
                exported_string_no_tag: "another long string".into(),
                nested: Some(NestedStruct {
                    value: "nested long string".into(),
                }),
            }),
            10,
            r#"{"ExportedInt":42,"ExportedStringNoTag":"another lo...[TRUNCATED]","UnexportedField":"should not...[TRUNCATED]","exported_string":"long strin...[TRUNCATED]","nested":{"Value":"nested lon...[TRUNCATED]"}}"#,
            true,
        ),
        (
            "typed slice",
            Box::new(vec!["very long string 1", "short"]),
            10,
            r#"["very long ...[TRUNCATED]","short"]"#,
            true,
        ),
        (
            "map with non-string keys",
            Box::new(BTreeMap::from([(1_i64, "very long string 1")])),
            10,
            r#"{"1":"very long ...[TRUNCATED]"}"#,
            true,
        ),
        (
            "array",
            Box::new(["very long string"]),
            10,
            r#"["very long ...[TRUNCATED]"]"#,
            true,
        ),
    ];
    for (name, obj, max_len, want, want_trunc) in cases {
        let (got, truncated) = obj.smart_truncate(max_len, true);
        assert_eq!(truncated, want_trunc, "{name}: truncated");
        assert_eq!(parse(&got), parse(want), "{name}");
        assert_eq!(got, want, "{name}: exact text");
    }
}

/// Boxed values of different `Serialize` types for table tests.
mod erased {
    use serde::Serialize;

    pub(super) trait Ser {
        fn smart_truncate(&self, max_len: i64, redact: bool) -> (String, bool);
    }

    impl<T: Serialize> Ser for T {
        fn smart_truncate(&self, max_len: i64, redact: bool) -> (String, bool) {
            super::st_with(self, max_len, redact)
        }
    }
}

/// Go `errorMarshaler`: a value whose serialization fails.
struct ErrorMarshaler;

impl Serialize for ErrorMarshaler {
    fn serialize<S: Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
        Err(S::Error::custom("cannot marshal"))
    }
}

/// Go `json.Number(text)`.
struct JsonNumber(&'static str);

impl Serialize for JsonNumber {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_newtype_struct(JSON_NUMBER_TOKEN, self.0)
    }
}

// Cyclic map / slice / pointer, func, chan and panicking MarshalJSON cases have no Rust value
// (skipped per case, ledger 7b-1). json.Number is a JSON_NUMBER_TOKEN newtype, json.RawMessage
// a serde_json RawValue, a failing MarshalJSON a failing Serialize impl.
// parity: adapted: 6 of 16 Go cases have no Rust value (cycles, func, chan, panic)
#[test]
fn test_smart_truncate_guards() {
    let shared = json!({"v": 1});
    let raw =
        serde_json::value::RawValue::from_string(r#"{"n":9007199254740993}"#.to_owned()).unwrap();
    let cases: Vec<(&str, Box<dyn erased::Ser>, &str, bool)> = vec![
        // skipped case (ledger 7b-1): "map cycle" — a Rust map cannot contain itself.
        // skipped case (ledger 7b-1): "slice cycle" — a Rust Vec cannot contain itself.
        // skipped case (ledger 7b-1): "pointer cycle" — no cyclic Serialize value.
        (
            "shared value is not a cycle",
            Box::new(BTreeMap::from([("x", &shared), ("y", &shared)])),
            r#"{"x":{"v":1},"y":{"v":1}}"#,
            false,
        ),
        (
            "int64 beyond 2^53",
            Box::new(9_007_199_254_740_993_i64),
            "9007199254740993",
            false,
        ),
        (
            "uint64 maximum",
            Box::new(u64::MAX),
            "18446744073709551615",
            false,
        ),
        (
            "json.Number",
            Box::new(JsonNumber("12345678901234567890")),
            "12345678901234567890",
            false,
        ),
        (
            "json.RawMessage",
            Box::new(raw),
            r#"{"n":9007199254740993}"#,
            false,
        ),
        (
            "NaN and infinities",
            Box::new(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
            r#"["NaN","+Inf","-Inf"]"#,
            false,
        ),
        // skipped case (ledger 7b-1): "panicking MarshalJSON" — a panicking Serialize impl
        // unwinds; it is not caught.
        (
            "failing MarshalJSON",
            Box::new(ErrorMarshaler),
            r#""[UNSUPPORTED_OBJECT]""#,
            true,
        ),
        // skipped case (ledger 7b-1): "function" and "channel" — no Serialize value.
        (
            "binary bytes",
            Box::new(GoBytes(&[0xff, 0xfe])),
            r#""[BINARY_DATA]""#,
            true,
        ),
        (
            "UTF-8 bytes",
            Box::new(GoBytes(b"hello")),
            r#""hello""#,
            false,
        ),
        (
            "unsupported map key",
            Box::new(BTreeMap::from([([1, 2], "x")])),
            r#"{"[UNSUPPORTED_KEY]":"x"}"#,
            true,
        ),
    ];
    for (name, obj, want, want_trunc) in cases {
        let (got, truncated) = obj.smart_truncate(100, true);
        assert_eq!(got, want, "{name}");
        assert_eq!(truncated, want_trunc, "{name}: truncated");
    }
}

/// Go `nested(depth)`: `depth` maps nested in each other around the string `"leaf"`.
fn nested(depth: usize) -> Value {
    let mut v = json!("leaf");
    for _ in 0..depth {
        v = json!({ "n": v });
    }
    v
}

#[test]
fn test_smart_truncate_depth_limit() {
    for (name, depth, want_leaf) in [
        ("within the limit", 10, true),
        ("beyond the limit", 60, false),
    ] {
        let (got, truncated) = st(&nested(depth), 100);
        assert_eq!(
            got.contains(r#""leaf""#),
            want_leaf,
            "{name}: kept the leaf"
        );
        assert_eq!(
            got.contains(MAX_DEPTH_PLACEHOLDER),
            !want_leaf,
            "{name}: wrote {MAX_DEPTH_PLACEHOLDER}"
        );
        assert_eq!(truncated, !want_leaf, "{name}: truncated");
    }
}

/// Go `credentials`.
#[derive(Serialize)]
struct Credentials {
    #[serde(rename = "user")]
    user: String,
    #[serde(rename = "api_key")]
    api_key: String,
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "password", skip_serializing_if = "String::is_empty")]
    unset: String,
}

fn credentials() -> Credentials {
    Credentials {
        user: "u".into(),
        api_key: "k".into(),
        token: "t".into(),
        unset: String::new(),
    }
}

/// The anonymous struct of the "struct fields by Go name" case.
#[derive(Serialize)]
#[allow(non_snake_case)]
struct GoNamed {
    APIKey: String,
    ClientSecret: String,
    Name: String,
}

// parity: adapted: Go structs are serde structs with the same field names; the map with a `[]byte` value is a map of GoAny values.
#[test]
fn test_smart_truncate_redacts_sensitive_keys() {
    let bytes_map = BTreeMap::from([
        (
            "arr".to_owned(),
            GoAny::Json(json!(r#"[{"password":"pw","id":1}]"#)),
        ),
        (
            "bytes".to_owned(),
            GoAny::Bytes(Bytes::from_static(br#"{"client_secret":"sec","v":2}"#)),
        ),
    ]);
    let cases: Vec<(&str, Box<dyn erased::Ser>, bool, &str)> = vec![
        (
            "credential keys at any depth",
            Box::new(json!({
                "api_key": "k", "PassWord": "p", "query": "weather",
                "headers": {"Authorization": "Bearer t", "Accept": "json"},
                "items": [{"x-goog-signature": "s", "id": 1}],
            })),
            false,
            concat!(
                r#"{"PassWord":"[REDACTED]","api_key":"[REDACTED]","#,
                r#""headers":{"Accept":"json","Authorization":"[REDACTED]"},"#,
                r#""items":[{"id":1,"x-goog-signature":"[REDACTED]"}],"query":"weather"}"#
            ),
        ),
        (
            "session temp keys",
            Box::new(json!({"temp:draft": "d", "user:name": "n"})),
            false,
            r#"{"temp:draft":"[REDACTED]","user:name":"n"}"#,
        ),
        (
            "struct fields by JSON name",
            Box::new(credentials()),
            false,
            r#"{"Token":"[REDACTED]","api_key":"[REDACTED]","user":"u"}"#,
        ),
        (
            "redaction off keeps map keys",
            Box::new(json!({"api_key": "k", "temp:draft": "d"})),
            true,
            r#"{"api_key":"k","temp:draft":"d"}"#,
        ),
        (
            "redaction off keeps struct fields",
            Box::new(credentials()),
            true,
            r#"{"Token":"t","api_key":"k","user":"u"}"#,
        ),
        (
            "JSON-encoded object string has sensitive keys redacted",
            Box::new(
                json!({"payload": r#"{"api_key":"secret","nested":{"token":"tok"},"keep":"ok"}"#}),
            ),
            false,
            r#"{"payload":"{\"api_key\":\"[REDACTED]\",\"keep\":\"ok\",\"nested\":{\"token\":\"[REDACTED]\"}}"}"#,
        ),
        (
            "JSON-encoded array string and UTF-8 bytes have sensitive keys redacted",
            Box::new(bytes_map),
            false,
            r#"{"arr":"[{\"id\":1,\"password\":\"[REDACTED]\"}]","bytes":"{\"client_secret\":\"[REDACTED]\",\"v\":2}"}"#,
        ),
        (
            "double-encoded JSON string has sensitive keys redacted",
            Box::new(json!({"payload": r#""{\"api_key\":\"secret\"}""#})),
            false,
            r#"{"payload":"\"{\\\"api_key\\\":\\\"[REDACTED]\\\"}\""}"#,
        ),
        (
            "duplicate keys in JSON string are re-serialized without leaking earlier key",
            Box::new(json!({"payload": r#"{"data":{"api_key":"secret"},"data":"ok"}"#})),
            false,
            r#"{"payload":"{\"data\":\"ok\"}"}"#,
        ),
        (
            "multiple leading BOMs before JSON object string have sensitive keys redacted",
            Box::new(json!({"payload": "\u{feff}\u{feff}{\"api_key\":\"secret\"}"})),
            false,
            r#"{"payload":"{\"api_key\":\"[REDACTED]\"}"}"#,
        ),
        (
            "redaction off keeps JSON-encoded string credentials",
            Box::new(json!({"payload": r#"{"api_key":"secret"}"#})),
            true,
            r#"{"payload":"{\"api_key\":\"secret\"}"}"#,
        ),
        (
            "camelCase and unseparated credential keys",
            Box::new(json!({
                "accessToken": "a", "APIKey": "k", "clientSecret": "c", "XGoogAPIKey": "g",
                "x_goog_api_key": "x", "apikey": "p", "tokenCount": 3, "api_key_id": "id",
                "secretName": "projects/p/secrets/s",
            })),
            false,
            concat!(
                r#"{"APIKey":"[REDACTED]","XGoogAPIKey":"[REDACTED]","accessToken":"[REDACTED]","#,
                r#""api_key_id":"id","apikey":"[REDACTED]","clientSecret":"[REDACTED]","#,
                r#""secretName":"projects/p/secrets/s","tokenCount":3,"x_goog_api_key":"[REDACTED]"}"#
            ),
        ),
        (
            "struct fields by Go name",
            Box::new(GoNamed {
                APIKey: "k".into(),
                ClientSecret: "c".into(),
                Name: "n".into(),
            }),
            false,
            r#"{"APIKey":"[REDACTED]","ClientSecret":"[REDACTED]","Name":"n"}"#,
        ),
        (
            "session user and app keys",
            Box::new(json!({
                "user:api_key": "k", "app:clientSecret": "c", "USER:Token": "t",
                "user:name": "n", "app:theme": "dark",
            })),
            false,
            concat!(
                r#"{"USER:Token":"[REDACTED]","app:clientSecret":"[REDACTED]","app:theme":"dark","#,
                r#""user:api_key":"[REDACTED]","user:name":"n"}"#
            ),
        ),
        (
            "JSON-encoded string with camelCase keys has sensitive keys redacted",
            Box::new(json!({"payload": r#"{"accessToken":"tok","keep":"ok"}"#})),
            false,
            r#"{"payload":"{\"accessToken\":\"[REDACTED]\",\"keep\":\"ok\"}"}"#,
        ),
        (
            "redaction off keeps camelCase and prefixed keys",
            Box::new(json!({"accessToken": "a", "user:api_key": "k"})),
            true,
            r#"{"accessToken":"a","user:api_key":"k"}"#,
        ),
    ];
    for (name, obj, no_redact, want) in cases {
        let (got, truncated) = obj.smart_truncate(100, !no_redact);
        assert_eq!(got, want, "{name}");
        assert!(!truncated, "{name}: truncated = true, want false");
    }
}

#[test]
fn test_redact_json_text_fail_closed() {
    let mut deep_json = r#"{"api_key":"secret"}"#.to_owned();
    for _ in 0..MAX_SANITIZE_DEPTH + 2 {
        deep_json = format!(r#"{{"nested":{deep_json}}}"#);
    }
    let cases = [
        ("malformed JSON object string", r#"{"api_key":"secret""#),
        (
            "JSON object with trailing suffix",
            r#"{"api_key":"secret"} trailing"#,
        ),
        (
            "JSON object followed by second JSON object",
            r#"{"ok":1} {"api_key":"secret"}"#,
        ),
        (
            "double-encoded malformed JSON string",
            r#""{\"api_key\":\"secret\"""#,
        ),
        (
            "quoted string with trailing JSON container",
            r#""note" {"api_key":"secret"}"#,
        ),
        (
            "quoted string followed by unicode-escaped JSON credential string",
            r#""note" then "{\"api_key\":\"SECRET\"}""#,
        ),
        (
            "multiple leading BOMs before malformed JSON object string",
            "\u{feff}\u{feff}{\"api_key\":\"secret\"",
        ),
    ];
    for (name, payload) in cases {
        let (got, truncated) = st(&json!({ "payload": payload }), 200);
        assert_eq!(got, r#"{"payload":"[UNPARSEABLE_JSON_BLOB]"}"#, "{name}");
        assert!(
            truncated,
            "{name}: truncated = false, want true when failing closed"
        );
    }

    // deeply nested JSON string fails closed
    let (got, truncated) = st(&json!({ "payload": deep_json }), 10000);
    assert!(!got.contains("secret"), "leaked secret in deep JSON: {got}");
    assert!(truncated, "truncated = false, want true for deep JSON");
}

/// Go `Outer` of `TestWalkStruct_OmitEmptyShadowsEmbeddedField` after field promotion: the
/// outer `x` (omitempty, empty) shadows the embedded `x`; the embedded `y` is promoted.
#[derive(Serialize)]
struct ShadowedOuter {
    #[serde(rename = "x", skip_serializing_if = "String::is_empty")]
    x: String,
    #[serde(rename = "y")]
    y: String,
}

// parity: adapted: serde has no struct embedding; the promoted field set Go's encoding/json writes is a plain serde struct and the want is Go's json.Marshal output.
#[test]
fn test_walk_struct_omit_empty_shadows_embedded_field() {
    let v = ShadowedOuter {
        x: String::new(),
        y: "inner-y".into(),
    };
    let (got, truncated) = st(&v, 100);
    assert!(!truncated, "truncated = true, want false");
    // Go: json.Marshal(Outer{Embedded: Embedded{X: "inner-x", Y: "inner-y"}, X: ""}).
    assert_eq!(got, r#"{"y":"inner-y"}"#);
}

#[derive(Serialize)]
struct PromotedInner {
    #[serde(rename = "name")]
    name: String,
    #[serde(rename = "token", skip_serializing_if = "String::is_empty")]
    token: String,
}

/// Go `promotedPtrOuter`: the embedded `*promotedInner` is a flattened `Option`.
#[derive(Serialize)]
struct PromotedPtrOuter {
    #[serde(flatten)]
    promoted_inner: Option<PromotedInner>,
    #[serde(rename = "extra")]
    extra: String,
}

// parity: adapted: the embedded pointer is `#[serde(flatten)] Option<_>`; the wants are Go's json.Marshal output.
#[test]
fn test_walk_struct_promotes_embedded_pointer_to_unexported_struct() {
    let cases = [
        (
            "non-nil embedded pointer",
            PromotedPtrOuter {
                promoted_inner: Some(PromotedInner {
                    name: "inner-name".into(),
                    token: String::new(),
                }),
                extra: "outer-extra".into(),
            },
            json!({"name": "inner-name", "extra": "outer-extra"}),
        ),
        (
            "nil embedded pointer",
            PromotedPtrOuter {
                promoted_inner: None,
                extra: "outer-extra".into(),
            },
            json!({"extra": "outer-extra"}),
        ),
    ];
    for (name, v, want) in cases {
        let (got, truncated) = st(&v, 1000);
        assert!(!truncated, "{name}: truncated = true, want false");
        assert_eq!(parse(&got), want, "{name} matches encoding/json");
    }

    // sensitive promoted field is redacted rather than dropped
    let v = PromotedPtrOuter {
        promoted_inner: Some(PromotedInner {
            name: "inner-name".into(),
            token: "r14-promoted-token-secret".into(),
        }),
        extra: "outer-extra".into(),
    };
    let (got, _) = st(&v, 1000);
    assert!(
        !got.contains("r14-promoted-token-secret"),
        "leaked the promoted token: {got}"
    );
    assert_eq!(
        parse(&got),
        json!({"name": "inner-name", "token": REDACTED_VALUE, "extra": "outer-extra"})
    );
}

#[test]
fn test_smart_truncate_node_budget_limit() {
    let nodes = usize::try_from(MAX_SANITIZE_NODES).unwrap();

    // oversized slice stops at budget with single sentinel
    let items = vec!["x"; nodes * 2];
    let (got, truncated) = st(&items, 100);
    assert!(
        truncated,
        "truncated = false, want true when node budget is exceeded"
    );
    assert_eq!(got.matches(SANITIZE_BUDGET_PLACEHOLDER).count(), 1);
    let parsed: Vec<Value> = serde_json::from_str(&got).unwrap();
    assert!(parsed.len() <= nodes, "parsed slice len = {}", parsed.len());
    assert_eq!(parsed.last(), Some(&json!(SANITIZE_BUDGET_PLACEHOLDER)));

    // oversized sensitive temp map charges budget and stops
    let m: BTreeMap<String, &str> = (0..nodes + 500)
        .map(|i| (format!("temp:k_{i}"), "secret"))
        .collect();
    let (got, truncated) = st(&m, 100);
    assert!(
        truncated,
        "truncated = false, want true when temp map exceeds node budget"
    );
    assert!(got.contains(SANITIZE_BUDGET_PLACEHOLDER));
    let parsed: Map<String, Value> = serde_json::from_str(&got).unwrap();
    assert!(parsed.len() <= nodes, "parsed map len = {}", parsed.len());

    // JSON text shares sanitizer node budget
    let mut items = vec!["x"; nodes - 5];
    items.push("[1,2,3,4,5,6,7,8,9,10]");
    let (got, truncated) = st(&items, 100);
    assert!(
        truncated,
        "truncated = false, want true when JSON text exhausts shared node budget"
    );
    assert!(got.contains(SANITIZE_BUDGET_PLACEHOLDER));
}

type Format = fn(Option<&Content>, i64, bool) -> (String, bool);

#[test]
fn test_content_summaries() {
    let mixed = content(vec![
        text_part("hello world"),
        call_part(FunctionCall {
            name: "lookup".into(),
            ..FunctionCall::default()
        }),
        response_part(FunctionResponse {
            name: "lookup".into(),
            ..FunctionResponse::default()
        }),
        inline_part("image/png", &[]),
    ]);
    let json_secret = single_part(text_part(r#"{"api_key":"secret","note":"0123456789"}"#));
    let multi_part_json_secret = content(vec![
        text_part(r#"{"api_key":"secret"}"#),
        text_part("plain follow-up"),
    ]);
    let markdown_link = single_part(text_part("[link](https://example.test)"));
    let bracket_tag = single_part(text_part("[INFO] ready"));
    let summary: Format = content_summary;
    let response: Format = format_response_content;
    let cases: Vec<(&str, Format, Option<&Content>, i64, &str, bool)> = vec![
        (
            "summary",
            summary,
            Some(&mixed),
            100,
            "hello world | Function response: lookup",
            false,
        ),
        (
            "truncated summary",
            summary,
            Some(&mixed),
            5,
            "hello...[TRUNCATED]",
            true,
        ),
        ("summary of nil", summary, None, 100, "", false),
        (
            "summary sanitizes JSON before truncating",
            summary,
            Some(&json_secret),
            25,
            r#"{"api_key":"[REDACTED]","...[TRUNCATED]"#,
            true,
        ),
        (
            "multi-part summary sanitizes JSON per part before joining",
            summary,
            Some(&multi_part_json_secret),
            200,
            r#"{"api_key":"[REDACTED]"} | plain follow-up"#,
            false,
        ),
        (
            "summary preserves markdown link starting with bracket",
            summary,
            Some(&markdown_link),
            200,
            "[link](https://example.test)",
            false,
        ),
        (
            "summary preserves bracketed tag",
            summary,
            Some(&bracket_tag),
            200,
            "[INFO] ready",
            false,
        ),
        (
            "response",
            response,
            Some(&mixed),
            100,
            "text: 'hello world' | call: lookup | resp: lookup | other",
            false,
        ),
        (
            "truncated response",
            response,
            Some(&mixed),
            5,
            "text: 'hello...[TRUNCATED]' | call: lookup | resp: lookup | other",
            true,
        ),
        (
            "response sanitizes JSON before truncating",
            response,
            Some(&json_secret),
            25,
            r#"text: '{"api_key":"[REDACTED]","...[TRUNCATED]'"#,
            true,
        ),
        ("response of nil", response, None, 100, "None", false),
    ];
    for (name, format, content, max_len, want, want_trunc) in cases {
        let got = format(content, max_len, true);
        assert_eq!(got, (want.to_owned(), want_trunc), "{name}");
    }
}

#[test]
fn test_format_content_parts_redaction_and_preservation() {
    // function response with URL and query parameter preserves trailing query params
    let c = single_part(response_part(FunctionResponse {
        name: "convert_currency".into(),
        response: obj(json!({
            "error": "API error: GET https://api.exchangerate.host/convert?api_key=SECRET_12345&to=USD",
        })),
        ..FunctionResponse::default()
    }));
    let parts = format_content_parts(Some(&c), 1000);
    assert_eq!(parts.len(), 1);
    let attrs = parts[0]["part_attributes"].as_str().unwrap();
    assert!(
        !attrs.contains("SECRET_12345"),
        "part_attributes leaked credential: {attrs}"
    );
    assert!(
        attrs.contains("api_key=%5BREDACTED%5D") || attrs.contains("api_key=[REDACTED]"),
        "part_attributes did not redact api_key: {attrs}"
    );
    let decoded = parse(attrs);
    let error = lookup(&decoded, &["function_response", "response", "error"])
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        error.contains("&to=USD"),
        "decoded error {error:?} lost query param &to=USD"
    );

    // plain text part with Authorization header scrubs credential
    let c = single_part(text_part(
        "Failed with error: Authorization: Bearer secret-bearer-token-12345",
    ));
    let parts = format_content_parts(Some(&c), 1000);
    assert_eq!(parts.len(), 1);
    let text = parts[0]["text"].as_str().unwrap();
    assert!(
        !text.contains("secret-bearer-token-12345"),
        "text leaked credential: {text}"
    );
    assert!(
        text.contains("Authorization: [REDACTED]") || text.contains("Bearer [REDACTED]"),
        "text did not redact credential: {text}"
    );
}

#[test]
fn test_format_content_parts_structured_parts() {
    let cases = [
        (
            "function call",
            call_part(FunctionCall {
                name: "lookup".into(),
                args: obj(json!({"q": "weather"})),
                ..FunctionCall::default()
            }),
            "application/json",
            "Function: lookup",
            r#"{"function_call":{"args":{"q":"weather"},"name":"lookup"},"function_name":"lookup"}"#,
        ),
        (
            "function response",
            response_part(FunctionResponse {
                name: "lookup".into(),
                response: obj(json!({"temp": 21})),
                ..FunctionResponse::default()
            }),
            "application/json",
            "Function response: lookup",
            r#"{"function_response":{"name":"lookup","response":{"temp":21}}}"#,
        ),
        (
            "function response without a name",
            response_part(FunctionResponse {
                response: obj(json!({"ok": true})),
                ..FunctionResponse::default()
            }),
            "application/json",
            "Function response: unknown",
            r#"{"function_response":{"response":{"ok":true}}}"#,
        ),
        (
            "executable code",
            code_part("print(1)", Some(Language::Python)),
            "text/plain",
            "print(1)",
            r#"{"executable_code":{"code":"print(1)","language":"PYTHON"}}"#,
        ),
        (
            "code execution result",
            result_part("1", Some(Outcome::Ok)),
            "text/plain",
            "1",
            r#"{"code_execution_result":{"outcome":"OUTCOME_OK","output":"1"}}"#,
        ),
    ];
    for (name, part, want_mime, want_text, want_attrs) in cases {
        let (mut got, truncated) = format_content_parts_with(Some(&single_part(part)), 100, true);
        assert!(!truncated, "{name}: truncated = true, want false");
        assert_eq!(got.len(), 1, "{name}: {got:?}");
        check_json("part_attributes", &part_attributes(&got[0]), want_attrs).unwrap();
        got[0].remove("part_attributes");
        assert_eq!(
            Value::Object(got.remove(0)),
            json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": want_mime, "text": want_text}),
            "{name}"
        );
    }
}

#[test]
fn test_parts_collector_part_attributes_redact_sensitive_keys() {
    for (name, redact, want) in [
        (
            "redaction on",
            true,
            r#"{"function_response":{"name":"login","response":{"api_key":"[REDACTED]","user":"ann"}}}"#,
        ),
        (
            "redaction off",
            false,
            r#"{"function_response":{"name":"login","response":{"api_key":"SECRET-KEY","user":"ann"}}}"#,
        ),
    ] {
        let c = single_part(response_part(FunctionResponse {
            name: "login".into(),
            response: obj(json!({"api_key": "SECRET-KEY", "user": "ann"})),
            ..FunctionResponse::default()
        }));
        let collector = collect_parts(
            PartsOptions {
                max_len: 100,
                redact,
                ..PartsOptions::default()
            },
            &[Some(&c)],
        );
        assert_eq!(collector.parts.len(), 1, "{name}");
        check_json(
            "part_attributes",
            &part_attributes(&collector.parts[0]),
            want,
        )
        .unwrap();
    }
}

#[test]
fn test_parts_collector_truncated_flag() {
    const MAX_LEN: i64 = 30;
    let long = "x".repeat(50);
    let cases = [
        ("short text", text_part("short"), false),
        ("text at the limit", text_part("x".repeat(30)), false),
        ("long text", text_part(long.clone()), true),
        ("inline data", inline_part("image/png", &[0; 1000]), false),
        (
            "function call",
            call_part(FunctionCall {
                name: "lookup".into(),
                ..FunctionCall::default()
            }),
            false,
        ),
        (
            "function call with a long name",
            call_part(FunctionCall {
                name: long.clone(),
                ..FunctionCall::default()
            }),
            true,
        ),
        (
            "function response",
            response_part(FunctionResponse {
                name: "f".into(),
                response: obj(json!({"k": "v"})),
                ..FunctionResponse::default()
            }),
            false,
        ),
        (
            "function response with a long value",
            response_part(FunctionResponse {
                name: "f".into(),
                response: obj(json!({ "k": long })),
                ..FunctionResponse::default()
            }),
            true,
        ),
        ("executable code", code_part("print(1)", None), false),
        ("long executable code", code_part(&long, None), true),
        ("long code output", result_part(&long, None), true),
        (
            "file URI",
            file_part("", "https://example.com/a.png"),
            false,
        ),
        (
            "file URI with a secret",
            file_part("", "https://example.com/a.png?key=abc"),
            true,
        ),
    ];
    for (name, part, want) in cases {
        let (_, got) = format_content_parts_with(Some(&single_part(part)), MAX_LEN, true);
        assert_eq!(got, want, "{name}: truncated");
    }
}

// parity: adapted: a Rust Content cannot hold Go's nil *Part, so the first (nil) part of Go's message is left out and the indexes of the others shift down by one.
#[test]
fn test_format_content_parts_empty_parts() {
    let c = content(vec![Part::default(), text_part(""), Part::default()]);
    let got: Vec<Value> = format_content_parts(Some(&c), 100)
        .into_iter()
        .map(Value::Object)
        .collect();
    // Every part keeps its index, whether or not there is anything to log.
    let want = vec![
        json!({"mime_type": "text/plain", "part_attributes": "{}", "part_index": 0, "storage_mode": "INLINE"}),
        json!({"mime_type": "text/plain", "part_attributes": "{}", "part_index": 1, "storage_mode": "INLINE"}),
        json!({"mime_type": "text/plain", "part_attributes": "{}", "part_index": 2, "storage_mode": "INLINE"}),
    ];
    assert_eq!(got, want);
}

#[test]
fn test_parts_collector_inline_data_without_mime_type() {
    let c = single_part(inline_part("", &[1]));
    // Without offloading, a missing MIME type is written as it is.
    let want = vec![entry(
        json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "", "text": "[BINARY DATA]"}),
    )];
    // Offloading needs a content type for the object, so the part records the generic binary
    // type that the object is stored with.
    let want_offloaded = vec![entry(
        json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "application/octet-stream", "text": "[BINARY DATA]"}),
    )];

    // exported function
    assert_eq!(format_content_parts(Some(&c), 100), want);

    // offloading off
    let collector = collect_parts(opts(100), &[Some(&c)]);
    assert_eq!(collector.parts, want);
    assert!(
        collector.pending.is_empty(),
        "queued uploads = {:?}",
        collector.pending
    );

    // offloading on
    let collector = collect_parts(offload_opts(100, 10), &[Some(&c)]);
    assert_eq!(collector.parts, want_offloaded);
    // The object is stored with the same type that the row names.
    assert_eq!(
        collector.pending,
        vec![PendingUpload {
            slot: 0,
            mime_type: "application/octet-stream".into(),
            data: Bytes::from_static(&[1]),
            ..PendingUpload::default()
        }]
    );

    // offloading on, upload budget exhausted
    let collector = collect_parts(
        PartsOptions {
            reserve_bytes: reject_all(),
            ..offload_opts(100, 10)
        },
        &[Some(&c)],
    );
    let want_kept = vec![entry(
        json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "application/octet-stream", "text": UPLOAD_FAILED_TEXT}),
    )];
    assert_eq!(collector.parts, want_kept);
    assert!(
        collector.pending.is_empty(),
        "queued uploads = {:?}",
        collector.pending
    );
}

fn text_upload(text: String) -> PendingUpload {
    PendingUpload {
        slot: 0,
        mime_type: "text/plain".into(),
        is_text: true,
        text,
        ..PendingUpload::default()
    }
}

#[test]
fn test_parts_collector_offloading() {
    const INLINE_LIMIT: i64 = 10;
    let on = offload_opts(100, INLINE_LIMIT);
    let text_entry = |text: &str| {
        entry(
            json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "text/plain", "text": text}),
        )
    };
    let image_entry = entry(
        json!({"part_index": 0, "storage_mode": "INLINE", "mime_type": "image/png", "text": "[BINARY DATA]"}),
    );
    let text = |n: usize| "a".repeat(n);
    let off = PartsOptions {
        max_len: 100,
        inline_limit: INLINE_LIMIT,
        ..PartsOptions::default()
    };
    let cases: Vec<(
        &str,
        PartsOptions,
        Part,
        Option<Map<String, Value>>,
        Vec<PendingUpload>,
    )> = vec![
        (
            "offloading off keeps long text inline",
            off.clone(),
            text_part(text(40)),
            Some(text_entry(&text(40))),
            vec![],
        ),
        (
            "offloading off keeps images inline",
            off,
            inline_part("image/png", &[1, 2, 3]),
            Some(image_entry.clone()),
            vec![],
        ),
        (
            "text at the limit stays inline",
            on.clone(),
            text_part(text(10)),
            Some(text_entry(&text(10))),
            vec![],
        ),
        (
            "text over the limit is queued",
            on.clone(),
            text_part(text(11)),
            Some(text_entry(&text(11))),
            vec![text_upload(text(11))],
        ),
        (
            // The limit counts bytes: six runes of two bytes are twelve.
            "text over the limit in bytes is queued",
            on.clone(),
            text_part("é".repeat(6)),
            None,
            vec![text_upload("é".repeat(6))],
        ),
        (
            "text at the limit in bytes stays inline",
            on.clone(),
            text_part("é".repeat(5)),
            Some(text_entry(&"é".repeat(5))),
            vec![],
        ),
        (
            // The row cannot keep all of this inline, so the object must.
            "text cut by MaxContentLen is queued in full",
            offload_opts(30, 100),
            text_part(text(40)),
            Some(text_entry(&format!("{}...[TRUNCATED]", text(30)))),
            vec![PendingUpload {
                fallback_truncated: true,
                ..text_upload(text(40))
            }],
        ),
        (
            "text at MaxContentLen stays inline",
            offload_opts(30, 100),
            text_part(text(30)),
            Some(text_entry(&text(30))),
            vec![],
        ),
        (
            "without a length limit only the inline limit counts",
            offload_opts(-1, 100),
            text_part(text(40)),
            Some(text_entry(&text(40))),
            vec![],
        ),
        (
            "image is queued",
            on.clone(),
            inline_part("image/png", &[1, 2, 3]),
            Some(image_entry.clone()),
            vec![PendingUpload {
                slot: 0,
                mime_type: "image/png".into(),
                data: Bytes::from_static(&[1, 2, 3]),
                ..PendingUpload::default()
            }],
        ),
        (
            "image without data is queued too",
            on.clone(),
            inline_part("image/png", &[]),
            Some(image_entry),
            vec![PendingUpload {
                slot: 0,
                mime_type: "image/png".into(),
                ..PendingUpload::default()
            }],
        ),
        (
            "function call is not queued",
            on.clone(),
            call_part(FunctionCall {
                name: "f".repeat(40),
                ..FunctionCall::default()
            }),
            None,
            vec![],
        ),
        (
            "function response is not queued",
            on.clone(),
            response_part(FunctionResponse {
                name: "f".into(),
                response: obj(json!({ "k": text(40) })),
                ..FunctionResponse::default()
            }),
            None,
            vec![],
        ),
        (
            "code is not queued",
            on,
            code_part(&text(40), None),
            None,
            vec![],
        ),
    ];
    for (name, opts, part, want_entry, want_pending) in cases {
        let c = collect_parts(opts, &[Some(&single_part(part))]);
        if let Some(want_entry) = want_entry {
            assert_eq!(c.parts, vec![want_entry], "{name}: content_parts");
        }
        assert_eq!(c.pending, want_pending, "{name}: queued uploads");
    }
}

// parity: adapted: Go's "Cloud Storage file with invalid UTF-8 in the URI" case has no Rust input (a &str is valid UTF-8)
#[test]
fn test_parts_collector_file_data_object_refs() {
    struct Case {
        name: &'static str,
        offload: bool,
        connection_id: &'static str,
        uri: &'static str,
        want_uri: &'static str,
        want_ref: Option<Value>,
        want_truncated: bool,
    }
    let cases = [
        Case {
            name: "Cloud Storage file with a connection",
            offload: true,
            connection_id: "us.conn",
            uri: "gs://bucket/doc.pdf",
            want_uri: "gs://bucket/doc.pdf",
            want_ref: Some(
                json!({"uri": "gs://bucket/doc.pdf", "details": "{}", "authorizer": "us.conn"}),
            ),
            want_truncated: false,
        },
        Case {
            name: "Cloud Storage file without a connection",
            offload: true,
            connection_id: "",
            uri: "gs://bucket/doc.pdf",
            want_uri: "gs://bucket/doc.pdf",
            want_ref: Some(json!({"uri": "gs://bucket/doc.pdf", "details": "{}"})),
            want_truncated: false,
        },
        Case {
            name: "other file",
            offload: true,
            connection_id: "us.conn",
            uri: "https://example.com/doc.pdf",
            want_uri: "https://example.com/doc.pdf",
            want_ref: None,
            want_truncated: false,
        },
        Case {
            name: "offloading off",
            offload: false,
            connection_id: "us.conn",
            uri: "gs://bucket/doc.pdf",
            want_uri: "gs://bucket/doc.pdf",
            want_ref: None,
            want_truncated: false,
        },
        Case {
            name: "Cloud Storage file with a secret in the URI",
            offload: true,
            connection_id: "us.conn",
            uri: "gs://bucket/doc.pdf?key=abc",
            want_uri: "gs://bucket/doc.pdf?key=%5BREDACTED%5D",
            want_ref: None,
            want_truncated: true,
        },
        Case {
            name: "Cloud Storage file with user information",
            offload: true,
            connection_id: "us.conn",
            uri: "gs://user:pw@bucket/doc.pdf",
            want_uri: REDACTED_URI_PLACEHOLDER,
            want_ref: None,
            want_truncated: true,
        },
        // skipped case: "Cloud Storage file with invalid UTF-8 in the URI" — a Rust &str is valid UTF-8.
        Case {
            name: "no URI",
            offload: true,
            connection_id: "us.conn",
            uri: "",
            want_uri: "",
            want_ref: None,
            want_truncated: false,
        },
    ];
    for tt in cases {
        let part = file_part("application/pdf", tt.uri);
        let c = collect_parts(
            PartsOptions {
                max_len: 100,
                redact: true,
                offload: tt.offload,
                inline_limit: 10,
                connection_id: tt.connection_id.to_owned(),
                ..PartsOptions::default()
            },
            &[Some(&single_part(part))],
        );
        let mut want = entry(
            json!({"part_index": 0, "storage_mode": "EXTERNAL_URI", "mime_type": "application/pdf", "uri": tt.want_uri}),
        );
        if let Some(r) = tt.want_ref {
            want.insert("object_ref".into(), r);
        }
        assert_eq!(c.parts, vec![want], "{}: content_parts", tt.name);
        assert_eq!(c.truncated, tt.want_truncated, "{}: truncated", tt.name);
        assert!(
            c.pending.is_empty(),
            "{}: the file is stored already",
            tt.name
        );
    }
}

// parity: adapted: Go's nil *Part in the third message is a default Part (still one entry, so every slot and part index matches Go's).
#[test]
fn test_parts_collector_slots_and_ordinals() {
    let png = |b: u8| inline_part("image/png", &[b]);
    let long = "a".repeat(40);
    let first = content(vec![text_part("short"), png(1)]);
    let third = content(vec![
        Part::default(),
        call_part(FunctionCall {
            name: "f".into(),
            ..FunctionCall::default()
        }),
        png(2),
        text_part(long.clone()),
    ]);
    // A nil message has no parts, but keeps its number.
    let c = collect_parts(offload_opts(100, 10), &[Some(&first), None, Some(&third)]);

    // Slots count the entries of all messages; part indexes restart in each.
    let want = vec![
        PendingUpload {
            slot: 1,
            content_ord: 0,
            part_idx: 1,
            mime_type: "image/png".into(),
            data: Bytes::from_static(&[1]),
            ..PendingUpload::default()
        },
        PendingUpload {
            slot: 4,
            content_ord: 2,
            part_idx: 2,
            mime_type: "image/png".into(),
            data: Bytes::from_static(&[2]),
            ..PendingUpload::default()
        },
        PendingUpload {
            slot: 5,
            content_ord: 2,
            part_idx: 3,
            mime_type: "text/plain".into(),
            is_text: true,
            text: long,
            ..PendingUpload::default()
        },
    ];
    assert_eq!(c.pending, want);
    assert_eq!(c.parts.len(), 6, "{:?}", c.parts);
    for u in &c.pending {
        assert_eq!(
            c.parts[u.slot]["part_index"],
            json!(u.part_idx),
            "the entry in slot {}",
            u.slot
        );
    }
}

// parity: adapted: Rust `Bytes` are immutable, so the caller cannot change the queued bytes in place; the test changes the message after collecting and checks the queue kept its data.
#[test]
fn test_parts_collector_queued_data_is_a_copy() {
    let mut part = inline_part("image/png", &[1, 2, 3]);
    let c = collect_parts(offload_opts(100, 10), &[Some(&single_part(part.clone()))]);

    // The caller changes the message.
    if let Some(blob) = part.inline_data.as_mut() {
        blob.data = Bytes::new();
        blob.mime_type = "image/gif".into();
    }

    let want = vec![PendingUpload {
        slot: 0,
        mime_type: "image/png".into(),
        data: Bytes::from_static(&[1, 2, 3]),
        ..PendingUpload::default()
    }];
    assert_eq!(c.pending, want);
}

#[test]
fn test_parts_collector_text_and_structured_redaction() {
    let secret_json = format!(
        r#"{{"api_key":"sk-secret-key-999","msg":"{}"}}"#,
        "x".repeat(40)
    );
    let code_json = r#"{"password":"pw-secret-123"}"#;

    // redact enabled
    let msg = content(vec![
        text_part(secret_json.clone()),
        code_part(code_json, Some(Language::Python)),
        result_part(code_json, Some(Outcome::Ok)),
    ]);
    let c = collect_parts(
        PartsOptions {
            max_len: 500,
            redact: true,
            offload: true,
            inline_limit: 20,
            ..PartsOptions::default()
        },
        &[Some(&msg)],
    );
    assert_eq!(c.pending.len(), 1);
    assert!(
        !c.pending[0].text.contains("sk-secret-key-999")
            && c.pending[0].text.contains(REDACTED_VALUE),
        "queued upload text was not redacted: {:?}",
        c.pending[0].text
    );
    for (i, part) in c.parts.iter().enumerate() {
        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
        assert!(
            !text.contains("sk-secret-key-999") && !text.contains("pw-secret-123"),
            "part {i} text contains unredacted secret: {text:?}"
        );
        assert!(
            text.contains(REDACTED_VALUE),
            "part {i} text lacks {REDACTED_VALUE:?}: {text:?}"
        );
    }

    // redact disabled
    let c = collect_parts(
        PartsOptions {
            max_len: 500,
            redact: false,
            offload: true,
            inline_limit: 20,
            ..PartsOptions::default()
        },
        &[Some(&single_part(text_part(secret_json)))],
    );
    assert!(
        c.pending.len() == 1 && c.pending[0].text.contains("sk-secret-key-999"),
        "pending text with redact=false = {:?}, want original secret preserved",
        c.pending
    );
}

fn response_blob(mime_type: &str, data: &[u8], display_name: &str) -> FunctionResponsePart {
    FunctionResponsePart {
        inline_data: Some(FunctionResponseBlob {
            mime_type: mime_type.to_owned(),
            data: Bytes::copy_from_slice(data),
            display_name: display_name.to_owned(),
        }),
        ..FunctionResponsePart::default()
    }
}

#[test]
fn test_normalize_function_response_python_snake_case_parity() {
    let fr = FunctionResponse {
        id: "call-1".into(),
        name: "lookup_doc".into(),
        will_continue: Some(true),
        scheduling: Some(FunctionResponseScheduling::WhenIdle),
        response: obj(json!({"CallerCamelKey": "untouched", "api_key": "sk-hidden"})),
        parts: vec![
            response_blob("text/plain", b"hello", "greeting.txt"),
            response_blob("image/png", &[0x89, b'P', b'N', b'G', 0xff], "pixel.png"),
            FunctionResponsePart {
                file_data: Some(FunctionResponseFileData {
                    file_uri: "gs://bucket/file.pdf".into(),
                    mime_type: "application/pdf".into(),
                    display_name: "report.pdf".into(),
                }),
                ..FunctionResponsePart::default()
            },
        ],
    };
    let c = collect_parts(
        PartsOptions {
            max_len: 1000,
            redact: true,
            ..PartsOptions::default()
        },
        &[Some(&single_part(response_part(fr)))],
    );
    assert_eq!(c.parts.len(), 1);
    let got = part_attributes(&c.parts[0]);
    let want = json!({
        "function_response": {
            "id": "call-1",
            "name": "lookup_doc",
            "will_continue": true,
            "scheduling": "WHEN_IDLE",
            "response": {"CallerCamelKey": "untouched", "api_key": REDACTED_VALUE},
            "parts": [
                {"inline_data": {"mime_type": "text/plain", "data": "hello", "display_name": "greeting.txt"}},
                {"inline_data": {"mime_type": "image/png", "data": BINARY_DATA_PLACEHOLDER, "display_name": "pixel.png"}},
                {"file_data": {"file_uri": "gs://bucket/file.pdf", "mime_type": "application/pdf", "display_name": "report.pdf"}},
            ],
        }
    });
    assert_eq!(got, want);
}

/// Go `inlineMedia(t, opts, mimeType, data)`: the collector, its `part_attributes` and the
/// data written there for a function response with one inline media part.
fn inline_media(
    opts: PartsOptions,
    mime_type: &str,
    data: &[u8],
) -> (PartsCollector, String, Value) {
    let msg = single_part(response_part(FunctionResponse {
        name: "fetch".into(),
        parts: vec![response_blob(mime_type, data, "")],
        ..FunctionResponse::default()
    }));
    let c = collect_parts(opts, &[Some(&msg)]);
    assert_eq!(c.parts.len(), 1);
    let attrs = c.parts[0]["part_attributes"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let decoded = part_attributes(&c.parts[0]);
    let list = lookup(&decoded, &["function_response", "parts"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        list.len(),
        1,
        "function_response.parts; part_attributes: {attrs}"
    );
    let got = lookup(&list[0], &["inline_data", "data"])
        .cloned()
        .unwrap_or_else(|| panic!("part has no inline_data.data: {}", list[0]));
    (c, attrs, got)
}

#[test]
fn test_normalize_function_response_inline_media_data_is_sanitized() {
    const CREDENTIAL_TEXT: &str = "db password: hunter2\napi_key=SECRET123\nAuthorization: Bearer abc.def.ghi\n{\"token\": \"TOKENVALUE\"}";
    const KEYED_JSON: &str = r#"{"password": "p4ssw0rd", "user": "alice"}"#;
    let redacting = |max_len| PartsOptions {
        max_len,
        redact: true,
        ..PartsOptions::default()
    };

    // credentials in UTF-8 data are redacted
    let (_, attrs, data) = inline_media(redacting(1000), "text/plain", CREDENTIAL_TEXT.as_bytes());
    for secret in ["hunter2", "SECRET123", "abc.def.ghi", "TOKENVALUE"] {
        assert!(
            !attrs.contains(secret),
            "part_attributes leaks {secret:?}: {attrs}"
        );
    }
    assert!(
        data.as_str().unwrap_or_default().contains(REDACTED_VALUE),
        "data = {data}, want it to contain {REDACTED_VALUE:?}"
    );

    // sensitive keys in JSON data are redacted
    let (_, attrs, data) = inline_media(redacting(1000), "application/json", KEYED_JSON.as_bytes());
    assert!(
        !attrs.contains("p4ssw0rd"),
        "part_attributes leaks the password: {attrs}"
    );
    let s = data.as_str().unwrap_or_default();
    assert!(
        s.contains(REDACTED_VALUE) && s.contains("alice"),
        "data = {data}, want the password redacted and the user kept"
    );

    // binary data is replaced by a placeholder
    let (c, _, data) = inline_media(
        redacting(1000),
        "image/png",
        &[0x89, b'P', b'N', b'G', 0xff],
    );
    assert_eq!(data, json!(BINARY_DATA_PLACEHOLDER));
    assert!(
        c.truncated,
        "collector is not marked truncated after binary data was dropped"
    );

    // long data is truncated
    let (c, _, data) = inline_media(redacting(10), "text/plain", "a".repeat(50).as_bytes());
    assert_eq!(
        data,
        json!(format!("{}{TRUNCATION_SUFFIX}", "a".repeat(10)))
    );
    assert!(
        c.truncated,
        "collector is not marked truncated after data was cut"
    );

    // data is kept as text when redaction is off
    let (_, _, data) = inline_media(
        PartsOptions {
            max_len: 1000,
            ..PartsOptions::default()
        },
        "text/plain",
        CREDENTIAL_TEXT.as_bytes(),
    );
    assert_eq!(data, json!(CREDENTIAL_TEXT));
}

#[test]
fn test_parts_collector_queued_text_does_not_mark_truncated() {
    let long_text = "a".repeat(50);
    let msg = single_part(text_part(long_text));
    let c = collect_parts(offload_opts(20, 100), &[Some(&msg)]);
    assert_eq!(c.pending.len(), 1);
    assert!(
        !c.truncated && !c.non_upload_truncated,
        "truncated={} nonUploadTruncated={}, want both false when text is queued for offload",
        c.truncated,
        c.non_upload_truncated
    );

    // When pending upload budget rejects the reservation, truncation is recorded immediately.
    let rejected = collect_parts(
        PartsOptions {
            reserve_bytes: reject_all(),
            ..offload_opts(20, 100)
        },
        &[Some(&msg)],
    );
    assert!(
        rejected.pending.is_empty(),
        "pending len = {}",
        rejected.pending.len()
    );
    assert!(
        rejected.truncated && rejected.non_upload_truncated,
        "truncated={} nonUploadTruncated={}, want both true when reservation fails",
        rejected.truncated,
        rejected.non_upload_truncated
    );
}

#[test]
fn test_content_summary_part_with_text_and_file_data_precedence() {
    let c = Content {
        role: "user".into(),
        parts: vec![Part {
            text: "should be ignored in summary".into(),
            file_data: Some(FileData {
                file_uri: "gs://bucket/file.png".into(),
                mime_type: "image/png".into(),
                ..FileData::default()
            }),
            ..Part::default()
        }],
    };
    let m = MessageSummary::new(Some(&c), 0, 100, false);
    assert!(
        m.pieces.is_empty(),
        "m.pieces len = {}, want 0 (FileData takes precedence over Text in contentSummary)",
        m.pieces.len()
    );
}

#[test]
fn test_content_summary_content_denied_failed_upload_truncated_pins_is_truncated() {
    let req = gadk_model::LlmRequest {
        contents: vec![Content {
            role: "user".into(),
            parts: vec![Part::from_text("x".repeat(50))],
        }],
        ..gadk_model::LlmRequest::default()
    };
    let mut parts = PartsCollector::new(PartsOptions {
        max_len: 10,
        offload: true,
        inline_limit: 5,
        reserve_bytes: Some(Arc::new(|_| true)),
        ..PartsOptions::default()
    });
    let (_, _, builder) =
        crate::plugin::rows::build_llm_request_content(&req, 10, false, true, Some(&mut parts));
    let Some(mut builder) = builder else {
        panic!("builder is nil");
    };
    assert_eq!(
        parts.pending.len(),
        1,
        "pending len = {}, want 1",
        parts.pending.len()
    );
    let u = &parts.pending[0];
    assert!(u.fallback_truncated, "fallbackTruncated = false, want true");

    let mut row = Map::new();
    row.insert(
        "event_type".into(),
        json!(crate::event_types::EVENT_LLM_REQUEST),
    );
    builder.retain_inline_summary(
        u.content_ord,
        u.part_idx,
        "xxxxxxxxxx[TRUNCATED]",
        u.fallback_truncated,
    );
    builder.apply_to_row(&mut row);

    assert!(
        !row.contains_key(crate::redaction::COLUMN_CONTENT),
        "row[{:?}] is set, want omitted when contentDenied is true",
        crate::redaction::COLUMN_CONTENT
    );
    assert!(
        row.get("is_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "row[is_truncated] = {:?}, want true (pinned by hasFailedUploadTruncated)",
        row.get("is_truncated")
    );
}
