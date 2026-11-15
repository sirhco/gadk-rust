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
//! The genai v1.77.0 cases expect the bodies and errors genai v1.77.0 itself produces
//! (captured from `Models.GenerateContent` against a recording HTTP client).

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

const GEMINI_ONLY: &str = "parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode.";
const VERTEX_ONLY: &str = "parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode.";

fn hi() -> Vec<Content> {
    text("hi")
}

/// genai v1.77.0 `store`: copied to the request root on the Gemini API (false included),
/// rejected on Vertex AI.
#[test]
fn store_is_gemini_api_only() {
    for store in [true, false] {
        let c = GenerateContentConfig {
            store: Some(store),
            ..Default::default()
        };
        assert_eq!(
            body(Backend::GeminiApi, &hi(), &c),
            json!({"contents": [{"parts": [{"text": "hi"}], "role": "user"}], "generationConfig": {}, "store": store})
        );
        assert_eq!(
            err(Backend::VertexAi, &hi(), &c),
            format!("store {GEMINI_ONLY}")
        );
    }
}

/// genai v1.77.0 `generationOutputConfig`: becomes the request's `outputConfig` on Vertex
/// AI (an empty config included), rejected on the Gemini API.
#[test]
fn generation_output_config_is_vertex_only() {
    for (goc, want) in [
        (
            GenerationOutputConfig {
                output_uri_prefix: "gs://b/p/".into(),
            },
            json!({"outputUriPrefix": "gs://b/p/"}),
        ),
        (GenerationOutputConfig::default(), json!({})),
    ] {
        let c = GenerateContentConfig {
            generation_output_config: Some(goc),
            ..Default::default()
        };
        assert_eq!(
            body(Backend::VertexAi, &hi(), &c),
            json!({"contents": [{"parts": [{"text": "hi"}], "role": "user"}], "generationConfig": {}, "outputConfig": want})
        );
        assert_eq!(
            err(Backend::GeminiApi, &hi(), &c),
            format!("generationOutputConfig {VERTEX_ONLY}")
        );
    }
}

fn function_response_content(fr: Value) -> Vec<Content> {
    vec![content_of(
        json!({"role": "user", "parts": [{"functionResponse": fr}]}),
    )]
}

fn content_of(v: Value) -> Content {
    serde_json::from_value(v).unwrap()
}

/// genai v1.77.0 `functionResponsePartToVertex`: a function-response part's
/// `mediaProcessing` is sent on the Gemini API and rejected on Vertex AI.
#[test]
fn function_response_part_media_processing_is_gemini_api_only() {
    let fr = json!({
        "id": "c1", "name": "f", "response": {"ok": true},
        "parts": [{"inlineData": {"data": "dg==", "mimeType": "video/mp4"}, "mediaProcessing": "AGENTIC"}],
    });
    let contents = function_response_content(fr.clone());
    let typed = &contents[0].parts[0]
        .function_response
        .as_ref()
        .unwrap()
        .parts[0];
    assert_eq!(typed.media_processing, Some(MediaProcessing::Agentic));
    assert_eq!(
        body(
            Backend::GeminiApi,
            &contents,
            &GenerateContentConfig::default()
        ),
        json!({"contents": [{"parts": [{"functionResponse": fr}], "role": "user"}], "generationConfig": {}})
    );
    assert_eq!(
        err(
            Backend::VertexAi,
            &contents,
            &GenerateContentConfig::default()
        ),
        format!("mediaProcessing {GEMINI_ONLY}")
    );
}

/// genai v1.77.0 `functionResponseToVertex`: `willContinue` is rejected on Vertex AI before
/// the parts are converted; the Gemini API copies the function response as is.
#[test]
fn function_response_will_continue_is_gemini_api_only() {
    let fr = json!({"name": "f", "parts": [{"mediaProcessing": "STATIC"}], "willContinue": true});
    let contents = function_response_content(fr.clone());
    assert_eq!(
        body(
            Backend::GeminiApi,
            &contents,
            &GenerateContentConfig::default()
        ),
        json!({"contents": [{"parts": [{"functionResponse": fr}], "role": "user"}], "generationConfig": {}})
    );
    assert_eq!(
        err(
            Backend::VertexAi,
            &contents,
            &GenerateContentConfig::default()
        ),
        format!("willContinue {GEMINI_ONLY}")
    );
}

/// A function response without Gemini-only fields is sent unchanged by both backends.
#[test]
fn function_response_plain_is_unchanged() {
    let fr = json!({
        "id": "c1", "name": "f", "response": {"ok": true}, "scheduling": "WHEN_IDLE",
        "parts": [{"fileData": {"fileUri": "gs://b/v.mp4", "mimeType": "video/mp4"}}],
    });
    let contents = function_response_content(fr.clone());
    for backend in [Backend::GeminiApi, Backend::VertexAi] {
        assert_eq!(
            body(backend, &contents, &GenerateContentConfig::default()),
            json!({"contents": [{"parts": [{"functionResponse": fr}], "role": "user"}], "generationConfig": {}}),
            "{backend:?}"
        );
    }
}
