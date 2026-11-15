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

//! Golden tests of the public formatter API — ADK-Go `SmartTruncate` and `FormatContentParts`
//! (redaction on) — against the Go probe's corpus (`tests/fixtures/formatter/`), and the
//! `SmartTruncate100` column of the redaction corpus (`tests/fixtures/redaction/text_corpus.json`).
//! The crate-internal entry points (`redact = false`, parts collector, summaries) are checked by
//! `src/formatter/golden_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use gadk_agentanalytics::{format_content_parts, smart_truncate};
use gadk_genai::Content;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn cases(path: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["cases"].as_array().unwrap().clone()
}

/// Checks a SmartTruncate result object `{output | output_len/output_head/output_tail,
/// truncated, error}` (outputs over 64 KiB are compared by length, head and tail).
fn check_result(what: &str, got: &[u8], truncated: bool, want: &Value) {
    assert_eq!(want["error"], Value::Null, "{what}: Go error");
    assert_eq!(
        truncated,
        want["truncated"].as_bool().unwrap(),
        "{what}: truncated"
    );
    let got = String::from_utf8(got.to_vec()).unwrap();
    if let Some(output) = want["output"].as_str() {
        assert_eq!(got, output, "{what}: output");
        return;
    }
    assert_eq!(
        got.len() as u64,
        want["output_len"].as_u64().unwrap(),
        "{what}: output_len"
    );
    assert!(
        got.starts_with(want["output_head"].as_str().unwrap()),
        "{what}: output_head"
    );
    assert!(
        got.ends_with(want["output_tail"].as_str().unwrap()),
        "{what}: output_tail"
    );
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

fn input(case: &Value) -> Value {
    let Some(g) = case.get("input_generator") else {
        return go_decoded(&case["input"]);
    };
    let count = usize::try_from(g["count"].as_u64().unwrap_or(0)).unwrap();
    let depth = g["depth"].as_u64().unwrap_or(0);
    match g["kind"].as_str().unwrap() {
        "repeat" => Value::Array(vec![go_decoded(&g["value"]); count]),
        "repeat_then" => {
            let mut items = vec![go_decoded(&g["value"]); count];
            items.extend(g["then"].as_array().unwrap().iter().map(go_decoded));
            Value::Array(items)
        }
        "nested_maps" => (0..depth).fold(json!("leaf"), |v, _| json!({ "n": v })),
        "nested_lists" => (0..depth).fold(json!("leaf"), |v, _| json!([v])),
        other => panic!("unknown generator {other}"),
    }
}

#[test]
fn smart_truncate_matches_go_json_corpus() {
    let cases = cases("formatter/smart_truncate_json.json");
    assert_eq!(cases.len(), 48);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let (got, truncated) =
            smart_truncate(&input(case), case["max_len"].as_i64().unwrap()).unwrap();
        check_result(name, &got, truncated, &case["redact"]);
    }
}

#[test]
fn format_content_parts_matches_go_corpus() {
    let cases = cases("formatter/format_content_parts.json");
    assert_eq!(cases.len(), 35);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let content: Option<Content> = (!case["content"].is_null())
            .then(|| serde_json::from_value(case["content"].clone()).unwrap());
        let got: Vec<Value> =
            format_content_parts(content.as_ref(), case["max_len"].as_i64().unwrap())
                .into_iter()
                .map(Value::Object)
                .collect();
        let want = case["redact"]["parts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn smart_truncate_matches_redaction_corpus() {
    let cases = cases("redaction/text_corpus.json");
    assert_eq!(cases.len(), 1341);
    for case in &cases {
        let text = case["input"].as_str().unwrap();
        let (got, truncated) = smart_truncate(text, 100).unwrap();
        check_result(
            &format!("{text:?}"),
            &got,
            truncated,
            &case["SmartTruncate100"],
        );
    }
}
