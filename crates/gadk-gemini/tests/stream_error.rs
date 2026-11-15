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

//! A streamed SSE chunk carrying `"error"`: port of genai v1.77.0 `iterateResponseStream`
//! (`respRaw["error"] != nil` yields the chunk's `APIError`), consumed as ADK-Go
//! `model/gemini/gemini.go` does (the first error ends the stream). The expected items are
//! what genai v1.77.0 yields for the same SSE body.

mod common;

use std::sync::Arc;

use common::{FakeHttp, vertex_config};
use futures::StreamExt;
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::Content;
use gadk_model::{Backend, HttpClient, LlmRequest, ModelError};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;

const CHUNK_A: &str = r#"{"candidates":[{"content":{"parts":[{"text":"a"}],"role":"model"}}]}"#;
const CHUNK_B: &str = r#"{"candidates":[{"content":{"parts":[{"text":"b"}],"role":"model"}}]}"#;

fn config(backend: Backend, http: Arc<dyn HttpClient>) -> ClientConfig {
    match backend {
        Backend::VertexAi => vertex_config(http),
        Backend::GeminiApi => ClientConfig {
            backend: Some(Backend::GeminiApi),
            api_key: Some("k".into()),
            http_client: Some(http),
            env: Arc::new(MapEnv(Default::default())),
            ..Default::default()
        },
    }
}

async fn stream_items(
    backend: Backend,
    sse: &str,
) -> Vec<Result<gadk_model::LlmResponse, ModelError>> {
    let http = FakeHttp::sse(sse);
    let model = GeminiModel::new("gemini-2.5-flash", config(backend, http))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![Content::from_text("hi", "user")],
        ..Default::default()
    };
    model.generate_content(req, true).collect().await
}

/// genai yields `APIError{Code: 503, Message: "overloaded", Status: "UNAVAILABLE"}` after the
/// first chunk; ADK-Go stops there, so the later chunk is never read.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn stream_error_chunk_yields_api_error(#[case] backend: Backend) {
    let sse = format!(
        "data: {CHUNK_A}\n\ndata: {}\n\ndata: {CHUNK_B}\n\n",
        r#"{"error":{"code":503,"message":"overloaded","status":"UNAVAILABLE","details":[{"reason":"r"}]}}"#
    );
    let items = stream_items(backend, &sse).await;
    assert_eq!(items.len(), 2, "items: {items:?}");
    let first = items[0].as_ref().unwrap();
    assert!(first.partial);
    assert_eq!(first.content.as_ref().unwrap().parts[0].text, "a");
    match &items[1] {
        Err(ModelError::Api {
            code,
            status,
            message,
            details,
        }) => {
            assert_eq!(*code, 503);
            assert_eq!(status, "UNAVAILABLE");
            assert_eq!(message, "overloaded");
            assert_eq!(details, &vec![json!({"reason": "r"})]);
        }
        other => panic!("want ModelError::Api, got {other:?}"),
    }
}

/// genai's `APIError.Code` comes from the chunk only: an error without `code` has code 0.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn stream_error_chunk_without_code_has_code_zero(#[case] backend: Backend) {
    let items = stream_items(backend, "data: {\"error\":{\"message\":\"no code\"}}\n\n").await;
    assert_eq!(items.len(), 1, "items: {items:?}");
    assert!(
        matches!(&items[0], Err(ModelError::Api { code: 0, status, message, .. }) if status.is_empty() && message == "no code"),
        "item: {:?}",
        items[0]
    );
}

/// The `"error"` check is stream-only: genai v1.77.0 `deserializeUnaryResponse` decodes a unary
/// 200 body as is (the `"error"` key is ignored), so `generateContent` answers a response without
/// candidates, which ADK-Go `Genai2LLMResponse` turns into an empty model content.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn unary_ok_body_with_error_is_decoded_not_an_error(#[case] backend: Backend) {
    let http =
        FakeHttp::json(r#"{"error":{"code":503,"message":"overloaded","status":"UNAVAILABLE"}}"#);
    let model = GeminiModel::new("gemini-2.5-flash", config(backend, http))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![Content::from_text("hi", "user")],
        ..Default::default()
    };
    let items: Vec<_> = model.generate_content(req, false).collect().await;
    assert_eq!(items.len(), 1, "items: {items:?}");
    let resp = items[0]
        .as_ref()
        .expect("a unary 200 body is not an API error");
    assert!(
        resp.error_code.is_empty(),
        "error_code: {:?}",
        resp.error_code
    );
    let content = resp.content.as_ref().expect("empty model content");
    assert_eq!(content.role, "model");
    assert!(content.parts.is_empty(), "parts: {:?}", content.parts);
}
