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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Request body construction: port of genai `generateContentParametersTo{Mldev,Vertex}`.

use gadk_gemini::internal::build_request_body;
use gadk_genai::*;
use gadk_model::{Backend, ModelError};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn cfg(v: Value) -> GenerateContentConfig {
    serde_json::from_value(v).unwrap()
}

fn body(backend: Backend, contents: &[Content], c: &GenerateContentConfig) -> Value {
    Value::Object(build_request_body(backend, contents, c, None).unwrap())
}

fn err(backend: Backend, contents: &[Content], c: &GenerateContentConfig) -> String {
    match build_request_body(backend, contents, c, None) {
        Err(ModelError::Config(m)) => m,
        other => panic!("want Config error, got {other:?}"),
    }
}

#[test]
fn empty_config_emits_empty_generation_config() {
    for backend in [Backend::GeminiApi, Backend::VertexAi] {
        assert_eq!(
            body(backend, &text("ping"), &GenerateContentConfig::default()),
            json!({"contents": [{"parts": [{"text": "ping"}], "role": "user"}], "generationConfig": {}})
        );
    }
}

#[test]
fn generation_and_top_level_fields_are_split() {
    let c = cfg(json!({
        "systemInstruction": {"parts": [{"text": "be brief"}]},
        "temperature": 0.5,
        "maxOutputTokens": 10,
        "stopSequences": ["x"],
        "responseMimeType": "application/json",
        "thinkingConfig": {"includeThoughts": true},
        "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}],
        "tools": [{"functionDeclarations": [{"name": "f"}]}],
        "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
        "labels": {"a": "b"},
        "cachedContent": "cachedContents/abc",
        "automaticContinuation": false
    }));
    let got = body(Backend::GeminiApi, &text("hi"), &c);
    assert_eq!(
        got,
        json!({
            "contents": [{"parts": [{"text": "hi"}], "role": "user"}],
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}],
            "tools": [{"functionDeclarations": [{"name": "f"}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
            "labels": {"a": "b"},
            "cachedContent": "cachedContents/abc",
            "generationConfig": {
                "temperature": 0.5,
                "maxOutputTokens": 10,
                "stopSequences": ["x"],
                "responseMimeType": "application/json",
                "thinkingConfig": {"includeThoughts": true}
            }
        })
    );
}

#[test]
fn cached_content_name_is_prefixed() {
    let c = cfg(json!({"cachedContent": "abc"}));
    assert_eq!(
        body(Backend::GeminiApi, &[], &c)["cachedContent"],
        json!("cachedContents/abc")
    );
}

#[test]
fn vertex_maps_model_selection_config_to_model_config() {
    let c = cfg(
        json!({"modelSelectionConfig": {"featureSelectionPreference": "PRIORITIZE_QUALITY"}, "audioTimestamp": true}),
    );
    let got = body(Backend::VertexAi, &[], &c);
    assert_eq!(
        got["generationConfig"],
        json!({"modelConfig": {"featureSelectionPreference": "PRIORITIZE_QUALITY"}, "audioTimestamp": true})
    );
}

#[test]
fn gemini_api_rejects_vertex_only_fields() {
    let c = cfg(json!({"audioTimestamp": true}));
    assert_eq!(
        err(Backend::GeminiApi, &[], &c),
        "audioTimestamp parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode."
    );
    let c = cfg(
        json!({"safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "method": "SEVERITY"}]}),
    );
    assert!(err(Backend::GeminiApi, &[], &c).starts_with("method parameter"));
}

#[test]
fn vertex_rejects_gemini_only_fields() {
    let c = cfg(json!({"enableEnhancedCivicAnswers": true}));
    assert_eq!(
        err(Backend::VertexAi, &[], &c),
        "enableEnhancedCivicAnswers parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode."
    );
    let contents: Vec<Content> = serde_json::from_value(json!([
        {"role": "user", "parts": [{"partMetadata": {"k": "v"}, "text": "x"}]}
    ]))
    .unwrap();
    assert!(
        err(
            Backend::VertexAi,
            &contents,
            &GenerateContentConfig::default()
        )
        .starts_with("partMetadata parameter")
    );
}

#[test]
fn extras_provider_runs_before_signature_rule() {
    let provider: gadk_model::ExtrasRequestProvider = std::sync::Arc::new(|mut b| {
        b.insert(
            "contents".into(),
            json!([{"parts": [{"thoughtSignature": "c2ln"}]}]),
        );
        b
    });
    let got = build_request_body(
        Backend::GeminiApi,
        &[],
        &GenerateContentConfig::default(),
        Some(&provider),
    )
    .unwrap();
    assert_eq!(got["contents"][0]["parts"][0]["text"], json!(""));
}
