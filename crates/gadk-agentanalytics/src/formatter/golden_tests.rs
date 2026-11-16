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

//! Golden tests of the formatter against the Go probe's `tests/fixtures/formatter/` corpus
//! (recorded from ADK-Go). The parts collector,
//! summaries and the `redact = false` entry points are crate-internal, so these run as unit
//! tests; `tests/golden_formatter.rs` checks the public `smart_truncate` /
//! `format_content_parts` (redaction on) and the redaction corpus's `SmartTruncate100`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::{TimeZone as _, Utc};
use gadk_genai::{Blob, Content, FunctionCall, FunctionResponse};
use pretty_assertions::assert_eq;
use serde::ser::SerializeMap as _;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::{
    ContentSummaryBuilder, GoAny, JSON_NUMBER_TOKEN, MessageSummary, PartsCollector, PartsOptions,
    content_summary, format_content_parts_with, format_part_attributes, format_response_content,
    normalize_function_response, object_ref, smart_truncate_with,
};
use crate::redaction::decode_std_base64;

fn fixture(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formatter")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    doc["cases"].as_array().unwrap().clone()
}

fn name_of(case: &Value) -> String {
    case["name"].as_str().unwrap_or_default().to_owned()
}

fn b64(s: &str) -> Vec<u8> {
    if s.is_empty() {
        Vec::new()
    } else {
        decode_std_base64(s).unwrap()
    }
}

/// A Go slice that may be nil (`null`): an empty list in Rust.
fn list(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// Checks a SmartTruncate result object `{output | output_len.., truncated, error}`.
fn check_result(what: &str, got: &[u8], truncated: bool, want: &Value) {
    assert_eq!(want["error"], Value::Null, "{what}: Go error");
    assert_eq!(
        truncated,
        want["truncated"].as_bool().unwrap(),
        "{what}: truncated"
    );
    let got_text = String::from_utf8(got.to_vec()).unwrap();
    if let Some(output) = want["output"].as_str() {
        assert_eq!(got_text, output, "{what}: output");
        return;
    }
    assert_eq!(
        got.len() as u64,
        want["output_len"].as_u64().unwrap(),
        "{what}: output_len"
    );
    let head = want["output_head"].as_str().unwrap();
    let tail = want["output_tail"].as_str().unwrap();
    assert!(got_text.starts_with(head), "{what}: output_head");
    assert!(got_text.ends_with(tail), "{what}: output_tail");
}

/// Go `json.Unmarshal` into `any`: every number is a `float64`.
fn go_decoded(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(xs) => Value::Array(xs.iter().map(go_decoded).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), go_decoded(v))).collect())
        }
        other => other.clone(),
    }
}

/// The input of a `smart_truncate_json.json` case.
fn json_case_input(case: &Value) -> Value {
    if let Some(generator) = case.get("input_generator") {
        let count = generator["count"].as_u64().unwrap_or(0) as usize;
        let depth = generator["depth"].as_u64().unwrap_or(0) as usize;
        return match generator["kind"].as_str().unwrap() {
            "repeat" => Value::Array(vec![go_decoded(&generator["value"]); count]),
            "repeat_then" => {
                let mut items = vec![go_decoded(&generator["value"]); count];
                items.extend(list(&generator["then"]).iter().map(go_decoded));
                Value::Array(items)
            }
            "nested_maps" => (0..depth).fold(json!("leaf"), |v, _| json!({ "n": v })),
            "nested_lists" => (0..depth).fold(json!("leaf"), |v, _| json!([v])),
            other => panic!("unknown generator {other}"),
        };
    }
    go_decoded(&case["input"])
}

#[test]
fn golden_smart_truncate_json() {
    let cases = fixture("smart_truncate_json.json");
    assert_eq!(cases.len(), 48);
    for case in &cases {
        let name = name_of(case);
        let input = json_case_input(case);
        let max_len = case["max_len"].as_i64().unwrap();
        for (mode, redact) in [("redact", true), ("no_redact", false)] {
            let (got, truncated) = smart_truncate_with(&input, max_len, redact).unwrap();
            check_result(&format!("{name} {mode}"), &got, truncated, &case[mode]);
        }
    }
}

// --- smart_truncate_typed.json --------------------------------------------------------------

trait Erased {
    fn sanitize(&self, max_len: i64, redact: bool) -> (Vec<u8>, bool);
}

impl<T: Serialize> Erased for T {
    fn sanitize(&self, max_len: i64, redact: bool) -> (Vec<u8>, bool) {
        smart_truncate_with(self, max_len, redact).unwrap()
    }
}

/// Go `[]byte` (`serde_bytes`).
struct GoBytes(Vec<u8>);

impl Serialize for GoBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

/// Go `json.Number`.
struct JsonNumber(&'static str);

impl Serialize for JsonNumber {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_newtype_struct(JSON_NUMBER_TOKEN, self.0)
    }
}

/// A map whose entries are written as given (Go map key kinds serde maps cannot mix).
struct Entries<K, V>(Vec<(K, V)>);

impl<K: Serialize, V: Serialize> Serialize for Entries<K, V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// A Go `interface{}` map key: a string or an `[2]int`.
enum AnyKey {
    Str(&'static str),
    Pair(i32, i32),
}

impl Serialize for AnyKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            AnyKey::Str(k) => s.serialize_str(k),
            AnyKey::Pair(a, b) => (a, b).serialize(s),
        }
    }
}

/// Probe `zzInner`.
#[derive(Serialize)]
struct Inner {
    #[serde(rename = "Value")]
    value: String,
    #[serde(rename = "client_secret")]
    secret: String,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// Probe `zzTagged`.
#[derive(Serialize)]
struct Tagged {
    #[serde(rename = "name")]
    name: String,
    #[serde(skip)]
    #[allow(dead_code)]
    skipped: String,
    #[serde(skip)]
    #[allow(dead_code)]
    hidden: String,
    #[serde(rename = "Count")]
    count: i64,
    #[serde(rename = "opt", skip_serializing_if = "String::is_empty")]
    opt: String,
    #[serde(rename = "opt_set", skip_serializing_if = "String::is_empty")]
    opt_set: String,
    #[serde(rename = "ptr", skip_serializing_if = "Option::is_none")]
    ptr: Option<Inner>,
    #[serde(rename = "nil_ptr")]
    nil_ptr: Option<Inner>,
    #[serde(rename = "inner")]
    inner: Inner,
    #[serde(rename = "tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "nil_tags")]
    nil_tags: Option<Vec<String>>,
    #[serde(rename = "attrs")]
    attrs: BTreeMap<String, Value>,
    #[serde(rename = "APIKey")]
    api_key: String,
    #[serde(rename = "password")]
    password: String,
    #[serde(rename = "raw")]
    raw: GoBytes,
    #[serde(rename = "ratio")]
    ratio: f32,
    #[serde(rename = "zero", skip_serializing_if = "is_zero")]
    zero: i64,
}

/// Probe `zzEmbedOuter` after Go's field promotion (adapted: serde has no embedding).
#[derive(Serialize)]
struct EmbedOuter {
    #[serde(rename = "A")]
    a: String,
    #[serde(rename = "b")]
    b: String,
    #[serde(rename = "C")]
    c: i64,
}

/// Probe `zzCredentials`.
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

fn inner(value: &str, secret: &str) -> Inner {
    Inner {
        value: value.into(),
        secret: secret.into(),
    }
}

/// The serde value of each typed case (`rust` in the fixture).
fn typed_value(case: &Value) -> Box<dyn Erased> {
    let go_json = || case["go_json"].clone();
    match name_of(case).as_str() {
        "nil_interface" => Box::new(()),
        "nil_pointer" => Box::new(Option::<Inner>::None),
        "int64_beyond_2_53" => Box::new(9_007_199_254_740_993_i64),
        "int64_min" => Box::new(i64::MIN),
        "uint64_max" => Box::new(u64::MAX),
        "int8_negative" => Box::new(-5_i8),
        "json_number_big" => Box::new(JsonNumber("12345678901234567890")),
        "floats_f64" => Box::new(vec![
            1.0_f64,
            1e21,
            1.5,
            -0.0,
            1e-7,
            123_456_789.0,
            0.1,
            5e-324,
            f64::MAX,
            1e20,
            2.5e-6,
        ]),
        "floats_f32" => Box::new(vec![0.1_f32, 1.0, 3.4e38, 1e-7, 16_777_217.0]),
        "non_finite" => Box::new(vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
        "non_finite_f32" => Box::new(vec![f32::NAN, f32::INFINITY]),
        "bytes_binary" => Box::new(GoBytes(vec![0xff, 0xfe])),
        "bytes_utf8" => Box::new(GoBytes(b"hello".to_vec())),
        "bytes_utf8_json_secret" => Box::new(GoBytes(br#"{"client_secret":"sec","v":2}"#.to_vec())),
        "bytes_utf8_long" => Box::new(GoBytes("é".repeat(120).into_bytes())),
        "bytes_empty" => Box::new(GoBytes(Vec::new())),
        "bytes_nil" => Box::new(Option::<GoBytes>::None),
        "bytes_in_map" => Box::new(BTreeMap::from([
            ("bin", GoBytes(vec![0x89, b'P', b'N', b'G', 0xff])),
            ("txt", GoBytes(b"hi".to_vec())),
        ])),
        "map_int_keys" => Box::new(BTreeMap::from([
            (1_i64, "a"),
            (2, "b"),
            (-3, "c"),
            (10, "d"),
        ])),
        "map_bool_keys" => Box::new(BTreeMap::from([(true, 1_i32), (false, 0)])),
        "map_float_keys" => Box::new(Entries(vec![(1.5_f64, "a"), (2.0, "b"), (1e21, "c")])),
        "map_unsupported_key" => Box::new(BTreeMap::from([((1_i32, 2_i32), "x")])),
        "map_two_unsupported_keys" => {
            Box::new(BTreeMap::from([((1_i32, 2_i32), "x"), ((3, 4), "x")]))
        }
        "map_unsupported_and_string_key" => Box::new(Entries(vec![
            (AnyKey::Str("a"), "x"),
            (AnyKey::Pair(1, 2), "y"),
        ])),
        "map_sensitive_int_key_values" => {
            Box::new(BTreeMap::from([("token", 5_i32), ("count", 2)]))
        }
        "struct_tags" => Box::new(Tagged {
            name: "a long name value".into(),
            skipped: "skipped".into(),
            hidden: "hidden".into(),
            count: 3,
            opt: String::new(),
            opt_set: "set".into(),
            ptr: Some(inner("p", "s1")),
            nil_ptr: None,
            inner: inner("i", "s2"),
            tags: Some(vec!["x".into(), "y".into()]),
            nil_tags: None,
            attrs: BTreeMap::from([("token".into(), json!("t")), ("k".into(), json!("v"))]),
            api_key: "k".into(),
            password: "pw".into(),
            raw: GoBytes(b"raw".to_vec()),
            ratio: 0.1,
            zero: 0,
        }),
        "struct_embedded_promotion" => Box::new(EmbedOuter {
            a: "a".into(),
            b: "outer".into(),
            c: 3,
        }),
        "struct_credentials" => Box::new(Credentials {
            user: "u".into(),
            api_key: "k".into(),
            token: "t".into(),
            unset: String::new(),
        }),
        "struct_pointer" => Box::new(Some(inner("a value longer than ten", "s"))),
        "array" => Box::new(["very long string", "x"]),
        "time" => Box::new(
            Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap()
                + chrono::TimeDelta::nanoseconds(123_456_789),
        ),
        "time_whole_second" => Box::new(Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap()),
        "json_raw_message" => Box::new(
            serde_json::value::RawValue::from_string(
                r#"{"n":9007199254740993,"api_key":"s","f":1.50}"#.to_owned(),
            )
            .unwrap(),
        ),
        "genai_function_call" => {
            Box::new(serde_json::from_value::<FunctionCall>(go_json()).unwrap())
        }
        "genai_blob_binary" | "genai_blob_utf8" => {
            Box::new(serde_json::from_value::<Blob>(go_json()).unwrap())
        }
        "genai_content" => Box::new(serde_json::from_value::<Content>(go_json()).unwrap()),
        other => panic!("typed case {other} has no Rust value"),
    }
}

#[test]
fn golden_smart_truncate_typed() {
    let cases = fixture("smart_truncate_typed.json");
    assert_eq!(cases.len(), 37);
    for case in &cases {
        let name = name_of(case);
        let value = typed_value(case);
        let max_len = case["max_len"].as_i64().unwrap();
        for (mode, redact) in [("redact", true), ("no_redact", false)] {
            let (got, truncated) = value.sanitize(max_len, redact);
            check_result(&format!("{name} {mode}"), &got, truncated, &case[mode]);
        }
    }
}

// --- content parts and summaries ------------------------------------------------------------

fn content_of(v: &Value) -> Option<Content> {
    (!v.is_null()).then(|| serde_json::from_value(v.clone()).unwrap())
}

#[test]
fn golden_format_content_parts() {
    let cases = fixture("format_content_parts.json");
    assert_eq!(cases.len(), 35);
    for case in &cases {
        let name = name_of(case);
        let content = content_of(&case["content"]);
        let max_len = case["max_len"].as_i64().unwrap();
        for (mode, redact) in [("redact", true), ("no_redact", false)] {
            let (parts, truncated) = format_content_parts_with(content.as_ref(), max_len, redact);
            let parts: Vec<Value> = parts.into_iter().map(Value::Object).collect();
            assert_eq!(parts, list(&case[mode]["parts"]), "{name} {mode}: parts");
            assert_eq!(
                truncated,
                case[mode]["truncated"].as_bool().unwrap(),
                "{name} {mode}: truncated"
            );
        }
    }
}

#[test]
fn golden_content_summaries() {
    let cases = fixture("content_summaries.json");
    assert_eq!(cases.len(), 35);
    for case in &cases {
        let name = name_of(case);
        let content = content_of(&case["content"]);
        let max_len = case["max_len"].as_i64().unwrap();
        for (mode, redact) in [("redact", true), ("no_redact", false)] {
            let want = &case[mode];
            let (summary, st) = content_summary(content.as_ref(), max_len, redact);
            let (response, rt) = format_response_content(content.as_ref(), max_len, redact);
            assert_eq!(
                json!({"summary": summary, "summary_truncated": st, "response": response, "response_truncated": rt}),
                *want,
                "{name} {mode}"
            );
        }
    }
}

#[test]
fn golden_parts_collector() {
    let cases = fixture("parts_collector.json");
    assert_eq!(cases.len(), 29);
    for case in &cases {
        let name = name_of(case);
        let o = &case["opts"];
        let calls = Arc::new(Mutex::new(Vec::<i64>::new()));
        let reserve_bytes: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>> =
            match o["reserve"]["mode"].as_str().unwrap() {
                "none" => None,
                mode => {
                    let calls = Arc::clone(&calls);
                    let mode = mode.to_owned();
                    let budget = o["reserve"]["budget"].as_i64().unwrap_or(0);
                    let used = Arc::new(Mutex::new(0_i64));
                    Some(Arc::new(move |n| {
                        calls.lock().unwrap().push(n);
                        match mode.as_str() {
                            "accept" => true,
                            "reject" => false,
                            _ => {
                                let mut used = used.lock().unwrap();
                                if *used + n <= budget {
                                    *used += n;
                                    true
                                } else {
                                    false
                                }
                            }
                        }
                    }))
                }
            };
        let mut c = PartsCollector::new(PartsOptions {
            max_len: o["maxLen"].as_i64().unwrap(),
            redact: o["redact"].as_bool().unwrap(),
            offload: o["offload"].as_bool().unwrap(),
            inline_limit: o["inlineLimit"].as_i64().unwrap(),
            connection_id: o["connectionID"].as_str().unwrap().to_owned(),
            reserve_bytes,
        });
        let contents: Vec<Option<Content>> =
            list(&case["contents"]).iter().map(content_of).collect();
        for (i, content) in contents.iter().enumerate() {
            c.add_content(content.as_ref(), i);
        }
        let parts: Vec<Value> = c.parts.iter().cloned().map(Value::Object).collect();
        assert_eq!(parts, list(&case["parts"]), "{name}: parts");
        let want_pending = list(&case["pending"]);
        assert_eq!(c.pending.len(), want_pending.len(), "{name}: pending");
        for (u, w) in c.pending.iter().zip(&want_pending) {
            assert_eq!(u.slot as u64, w["slot"].as_u64().unwrap(), "{name}: slot");
            assert_eq!(
                u.content_ord as u64,
                w["contentOrd"].as_u64().unwrap(),
                "{name}"
            );
            assert_eq!(u.part_idx as u64, w["partIdx"].as_u64().unwrap(), "{name}");
            assert_eq!(u.mime_type, w["mimeType"].as_str().unwrap(), "{name}");
            assert_eq!(u.is_text, w["isText"].as_bool().unwrap(), "{name}");
            assert_eq!(u.text, w["text"].as_str().unwrap(), "{name}");
            assert_eq!(
                &u.data[..],
                &b64(w["data_base64"].as_str().unwrap())[..],
                "{name}"
            );
            assert_eq!(
                u.fallback_truncated,
                w["fallbackTruncated"].as_bool().unwrap(),
                "{name}"
            );
            assert_eq!(
                u.reserved,
                w["reserved"].as_i64().unwrap(),
                "{name}: reserved"
            );
            assert_eq!(u.size() as u64, w["size"].as_u64().unwrap(), "{name}: size");
            assert_eq!(
                u.content_type(),
                w["contentType"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                &u.payload()[..],
                &b64(w["payload_base64"].as_str().unwrap())[..],
                "{name}"
            );
        }
        assert_eq!(
            c.truncated,
            case["truncated"].as_bool().unwrap(),
            "{name}: truncated"
        );
        assert_eq!(
            c.non_upload_truncated,
            case["nonUploadTruncated"].as_bool().unwrap(),
            "{name}: nonUploadTruncated"
        );
        assert_eq!(
            c.has_pending_text(),
            case["hasPendingText"].as_bool().unwrap(),
            "{name}: hasPendingText"
        );
        let want_calls: Vec<i64> = list(&case["reserve_calls"])
            .iter()
            .map(|n| n.as_i64().unwrap())
            .collect();
        assert_eq!(*calls.lock().unwrap(), want_calls, "{name}: reserve_calls");
    }
}

#[test]
fn golden_object_ref() {
    let cases = fixture("object_ref.json");
    assert_eq!(cases.len(), 3);
    for case in &cases {
        let got = object_ref(
            case["uri"].as_str().unwrap(),
            case["details"].as_str().unwrap(),
            case["authorizer"].as_str().unwrap(),
        );
        assert_eq!(Value::Object(got), case["objectRef"]);
    }
}

#[test]
fn golden_summary_builder() {
    let cases = fixture("summary_builder.json");
    assert_eq!(cases.len(), 10);
    for case in &cases {
        let name = name_of(case);
        let spec = &case["spec"];
        let max_len = spec["maxLen"].as_i64().unwrap();
        let redact = spec["redact"].as_bool().unwrap();
        let messages = list(&spec["messages"])
            .iter()
            .map(|m| {
                let content = content_of(&m["content"]);
                let ord = m["contentOrd"].as_u64().unwrap() as usize;
                let mut s = if m["response"].as_bool().unwrap() {
                    MessageSummary::new_response(content.as_ref(), ord, max_len, redact)
                } else {
                    MessageSummary::new(content.as_ref(), ord, max_len, redact)
                };
                s.role = m["role"].as_str().unwrap().to_owned();
                s.has_role = m["hasRole"].as_bool().unwrap();
                s.is_system = m["isSystem"].as_bool().unwrap();
                s
            })
            .collect();
        let mut b = ContentSummaryBuilder {
            is_llm_request: spec["isLLMRequest"].as_bool().unwrap(),
            is_llm_response: spec["isLLMResponse"].as_bool().unwrap(),
            content_denied: spec["contentDenied"].as_bool().unwrap(),
            max_len,
            redact,
            role_truncated: spec["roleTruncated"].as_bool().unwrap(),
            other_truncated: spec["otherTruncated"].as_bool().unwrap(),
            needs_rebuild: false,
            usage: spec["usage"].as_object().cloned(),
            messages,
        };
        let (before, before_t) = b.build_content();
        assert_eq!(
            Value::Object(before),
            case["build_before"],
            "{name}: build_before"
        );
        assert_eq!(
            before_t,
            case["build_before_truncated"].as_bool().unwrap(),
            "{name}"
        );
        let after_ops = list(&case["after_ops"]);
        for (i, op) in list(&spec["ops"]).iter().enumerate() {
            let ord = op["contentOrd"].as_u64().unwrap() as usize;
            let idx = op["partIdx"].as_u64().unwrap() as usize;
            match op["op"].as_str().unwrap() {
                "omit" => b.omit_text_from_summary(ord, idx),
                "retain" => b.retain_inline_summary(
                    ord,
                    idx,
                    op["fallbackText"].as_str().unwrap(),
                    op["fallbackTruncated"].as_bool().unwrap_or(false),
                ),
                other => panic!("unknown op {other}"),
            }
            assert_eq!(
                json!({"needsRebuild": b.needs_rebuild, "hasFailedUploadTruncated": b.has_failed_upload_truncated()}),
                after_ops[i],
                "{name}: after op {i}"
            );
        }
        let (after, after_t) = b.build_content();
        assert_eq!(
            Value::Object(after),
            case["build_after"],
            "{name}: build_after"
        );
        assert_eq!(
            after_t,
            case["build_after_truncated"].as_bool().unwrap(),
            "{name}"
        );
        let mut row = Map::new();
        b.apply_to_row(&mut row);
        assert_eq!(
            Value::Object(row),
            case["apply_to_row"],
            "{name}: apply_to_row"
        );
    }
}

/// The fixture's `normalized` (Go `json.Marshal`, `[]byte` as base64) with every
/// `inline_data.data` decoded to the byte array `serde_json` writes for [`GoAny::Bytes`].
fn normalized_with_byte_arrays(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(parts) = v.get_mut("parts").and_then(Value::as_array_mut) {
        for p in parts {
            if let Some(data) = p.pointer_mut("/inline_data/data") {
                *data = json!(b64(data.as_str().unwrap()));
            }
        }
    }
    v
}

#[test]
fn golden_normalize_function_response() {
    let cases = fixture("normalize_function_response.json");
    assert_eq!(cases.len(), 11);
    for case in &cases {
        let name = name_of(case);
        let fr: Option<FunctionResponse> = (!case["function_response"].is_null())
            .then(|| serde_json::from_value(case["function_response"].clone()).unwrap());
        let normalized: Option<BTreeMap<String, GoAny>> =
            fr.as_ref().map(normalize_function_response);
        assert_eq!(
            serde_json::to_value(&normalized).unwrap(),
            normalized_with_byte_arrays(&case["normalized"]),
            "{name}: normalized"
        );
        let attrs = BTreeMap::from([("function_response", &normalized)]);
        let max_len = case["max_len"].as_i64().unwrap();
        for (mode, redact) in [("redact", true), ("no_redact", false)] {
            let (got, truncated) = format_part_attributes(&attrs, max_len, redact);
            assert_eq!(
                json!({"part_attributes": got, "truncated": truncated}),
                case[mode],
                "{name} {mode}"
            );
        }
    }
}

/// A `GoAny::Bytes` leaf is sanitized as Go's `[]byte`.
#[test]
fn go_any_bytes_round_trip() {
    let v = GoAny::Bytes(Bytes::from_static(b"\xff"));
    let (got, truncated) = smart_truncate_with(&v, 10, true).unwrap();
    assert_eq!(got, br#""[BINARY_DATA]""#);
    assert!(truncated);
}
