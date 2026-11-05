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

//! Port of ADK-Go `model/gemini/continuation_test.go`, run against both backends, plus
//! tests of genai's stream reconnect rules.
//!
//! The Go tests answer each request from an `httptest` server keyed by its continuation
//! token; here [`Scripted`] answers with the same bodies in request order, and
//! [`check_requests`] checks the token each request carried.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::fake::Scripted;
use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::{
    Citation, CitationMetadata, Content, FinishReason, GenerateContentConfig,
    GenerateContentResponseUsageMetadata, GroundingMetadata,
};
use gadk_model::{Backend, HttpClient, HttpOptions, Llm, LlmRequest, LlmResponse, ModelError};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Map, Value, json};

const CONTINUATION_PROMPT: &str = "Count to three.";

type Response = (u16, &'static str, String);

/// The client config for `backend` on a fake server (Go `newContinuationModel`: API key
/// `key`, the test server as base URL). Vertex AI gets its project, location and
/// anonymous credentials on the config itself.
fn client_config(backend: Backend, http: Arc<dyn HttpClient>) -> ClientConfig {
    let http_options = HttpOptions {
        base_url: Some("http://fake/".into()),
        ..Default::default()
    };
    let base = ClientConfig {
        backend: Some(backend),
        http_options,
        http_client: Some(http),
        env: Arc::new(MapEnv(Default::default())),
        ..Default::default()
    };
    match backend {
        Backend::GeminiApi => ClientConfig {
            api_key: Some("key".into()),
            ..base
        },
        Backend::VertexAi => ClientConfig {
            project: Some("p".into()),
            location: Some("l".into()),
            credentials: Some(google_cloud_auth::credentials::anonymous::Builder::new().build()),
            ..base
        },
    }
}

async fn new_continuation_model(cfg: ClientConfig) -> Arc<dyn Llm> {
    GeminiModel::new("m", cfg).await.expect("NewModel")
}

/// Port of Go `candidate`.
fn candidate(text: &str, finish_reason: &str, token: &str) -> Map<String, Value> {
    let mut c = Map::new();
    c.insert(
        "content".into(),
        json!({"role": "model", "parts": [{"text": text}]}),
    );
    if !finish_reason.is_empty() {
        c.insert("finishReason".into(), json!(finish_reason));
    }
    if !token.is_empty() {
        c.insert("continuationToken".into(), json!(STANDARD.encode(token)));
    }
    c
}

fn usage() -> Value {
    json!({"promptTokenCount": 10, "candidatesTokenCount": 2, "totalTokenCount": 12})
}

/// Port of Go `fakeContinuationAPI.serve` for one token's chunks: streamGenerateContent
/// gets all of them as SSE events (usage on the last), generateContent the first.
fn serve(chunks: &[Map<String, Value>], stream: bool) -> Response {
    if stream {
        let mut body = String::new();
        for (i, c) in chunks.iter().enumerate() {
            let mut resp = json!({"candidates": [c]});
            if i == chunks.len() - 1 {
                resp["usageMetadata"] = usage();
            }
            body.push_str(&format!("data: {resp}\n\n"));
        }
        return (200, "text/event-stream", body);
    }
    let resp = json!({"candidates": [chunks[0]], "usageMetadata": usage()});
    (200, "application/json", resp.to_string())
}

fn continuation_request() -> LlmRequest {
    LlmRequest {
        contents: vec![Content::from_text(CONTINUATION_PROMPT, "user")],
        ..Default::default()
    }
}

/// The decoded continuation token of a request body, "" if none.
fn token_of(body: &Value) -> String {
    match body.get("continuationToken").and_then(Value::as_str) {
        Some(t) => String::from_utf8(STANDARD.decode(t).unwrap()).unwrap(),
        None => String::new(),
    }
}

/// Port of Go `checkRequests`: the continuation tokens of the requests sent, in order,
/// and that each carried the original contents unchanged.
fn check_requests(api: &Scripted, want_tokens: &[&str]) {
    let bodies = api.bodies.lock();
    let tokens: Vec<String> = bodies.iter().map(token_of).collect();
    assert_eq!(
        tokens, want_tokens,
        "continuation tokens of the requests sent"
    );
    for (i, body) in bodies.iter().enumerate() {
        let contents: Vec<String> = body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let text: String = c["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["text"].as_str().unwrap_or_default())
                    .collect();
                format!("{}: {text}", c["role"].as_str().unwrap_or_default())
            })
            .collect();
        assert_eq!(
            contents.join(" | "),
            format!("user: {CONTINUATION_PROMPT}"),
            "request {i} contents"
        );
    }
}

async fn collect(
    s: BoxStream<'static, Result<LlmResponse, ModelError>>,
) -> Vec<Result<LlmResponse, ModelError>> {
    s.collect().await
}

fn texts(content: &Option<Content>) -> Vec<String> {
    content
        .as_ref()
        .map(|c| c.parts.iter().map(|p| p.text.clone()).collect())
        .unwrap_or_default()
}

fn want_usage() -> Option<GenerateContentResponseUsageMetadata> {
    Some(GenerateContentResponseUsageMetadata {
        prompt_token_count: 20,
        candidates_token_count: 4,
        total_token_count: 24,
        ..Default::default()
    })
}

/// Go `mapsFromSlice` over a JSON value.
fn maps_from_slice(v: &mut Value) -> Vec<&mut Map<String, Value>> {
    match v {
        Value::Array(items) => items.iter_mut().filter_map(Value::as_object_mut).collect(),
        _ => Vec::new(),
    }
}

#[rstest]
#[case::gemini_api_unary(Backend::GeminiApi, false)]
#[case::gemini_api_stream(Backend::GeminiApi, true)]
#[case::vertex_ai_unary(Backend::VertexAi, false)]
#[case::vertex_ai_stream(Backend::VertexAi, true)]
#[tokio::test]
async fn test_continuation_preserves_client_provider_on_every_request(
    #[case] backend: Backend,
    #[case] stream: bool,
) {
    let api = Arc::new(Scripted::new(vec![
        serve(&[candidate("one ", "CONTINUATION", "t1")], stream),
        serve(&[candidate("two", "STOP", "")], stream),
    ]));
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let calls = provider_calls.clone();
    let mut cfg = client_config(backend, api.clone());
    cfg.http_options.extras_request_provider = Some(Arc::new(move |mut body| {
        calls.fetch_add(1, Ordering::SeqCst);
        body.insert("clientMarker".into(), json!("preserved"));
        if let Some(contents) = body.get_mut("contents") {
            let mut contents = maps_from_slice(contents);
            if let Some(Value::Array(parts)) = contents[0].get_mut("parts") {
                parts.push(json!({"thoughtSignature": "provider-signature"}));
            }
        }
        body
    }));
    let llm = new_continuation_model(cfg).await;
    for r in collect(llm.generate_content(continuation_request(), stream)).await {
        r.expect("GenerateContent");
    }

    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        2,
        "client provider calls"
    );
    let bodies = api.bodies.lock().clone();
    assert_eq!(bodies.len(), 2, "request count");
    for (i, mut body) in bodies.into_iter().enumerate() {
        assert_eq!(
            body.get("clientMarker"),
            Some(&json!("preserved")),
            "request {i}: client provider marker"
        );
        let mut contents = maps_from_slice(&mut body["contents"]);
        let parts = maps_from_slice(contents[0].get_mut("parts").unwrap());
        let provider_part = parts.last().unwrap();
        assert_eq!(
            provider_part.get("text"),
            Some(&json!("")),
            "request {i}: provider part text; want present empty text"
        );
    }
}

/// A generation the model pauses with CONTINUATION is resumed by automatic continuation,
/// which resends the original contents with the token and returns the responses merged
/// into one.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn test_continuation_generate_resumed_by_sdk(#[case] backend: Backend) {
    let api = Arc::new(Scripted::new(vec![
        serve(&[candidate("one ", "CONTINUATION", "t1")], false),
        serve(&[candidate("two", "STOP", "")], false),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let mut got = None;
    for r in collect(llm.generate_content(continuation_request(), false)).await {
        got = Some(r.expect("GenerateContent"));
    }
    let got = got.expect("no response");
    let content = got.content.as_ref().expect("no content");
    assert_eq!(content.role, "model");
    assert_eq!(texts(&got.content), ["one ", "two"]);
    assert_eq!(got.finish_reason, Some(FinishReason::Stop));
    assert_eq!(got.usage_metadata, want_usage());
    check_requests(&api, &["", "t1"]);
}

/// A streamed generation the model pauses with CONTINUATION goes on in the same stream,
/// without the paused chunk completing the turn.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn test_continuation_generate_stream_resumed_by_sdk(#[case] backend: Backend) {
    let api = Arc::new(Scripted::new(vec![
        serve(
            &[
                candidate("one ", "", ""),
                candidate("two ", "CONTINUATION", "t1"),
            ],
            true,
        ),
        serve(&[candidate("three", "STOP", "")], true),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let mut partials = Vec::new();
    let mut fin = None;
    for r in collect(llm.generate_content(continuation_request(), true)).await {
        let resp = r.expect("GenerateContent");
        if !resp.partial {
            fin = Some(resp);
            continue;
        }
        let mut desc = resp.content.as_ref().unwrap().parts[0].text.clone();
        if resp.turn_complete {
            desc.push_str(" (turn complete)");
        }
        partials.push(desc);
    }
    assert_eq!(partials, ["one ", "two ", "three (turn complete)"]);
    let fin = fin.expect("no final response");
    assert_eq!(fin.content.as_ref().unwrap().role, "model");
    assert_eq!(texts(&fin.content), ["one two three"]);
    assert_eq!(fin.finish_reason, Some(FinishReason::Stop));
    assert_eq!(fin.usage_metadata, want_usage());
    check_requests(&api, &["", "t1"]);
}

/// A request that turns off automatic continuation reaches the SDK with it off, so a
/// pause comes back as it is.
#[rstest]
#[case::gemini_api_unary(Backend::GeminiApi, false)]
#[case::gemini_api_stream(Backend::GeminiApi, true)]
#[case::vertex_ai_unary(Backend::VertexAi, false)]
#[case::vertex_ai_stream(Backend::VertexAi, true)]
#[tokio::test]
async fn test_continuation_automatic_continuation_off(
    #[case] backend: Backend,
    #[case] stream: bool,
) {
    let api = Arc::new(Scripted::new(vec![
        serve(&[candidate("one ", "CONTINUATION", "t1")], stream),
        serve(&[candidate("two", "STOP", "")], stream),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let mut req = continuation_request();
    req.config = Some(GenerateContentConfig {
        automatic_continuation: Some(false),
        ..Default::default()
    });
    let mut fin = None;
    for r in collect(llm.generate_content(req, stream)).await {
        let resp = r.expect("GenerateContent");
        if !resp.partial {
            fin = Some(resp);
        }
    }
    let fin = fin.expect("no final response");
    assert_eq!(fin.finish_reason, Some(FinishReason::Continuation));
    check_requests(&api, &[""]);
}

/// The final response of a resumed stream keeps the last grounding and citation metadata
/// any chunk reported, even when the last request reports none.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn test_continuation_generate_stream_keeps_metadata(#[case] backend: Backend) {
    // The Gemini API reports citations as `citationSources`, Vertex AI as `citations`.
    let citations_key = match backend {
        Backend::GeminiApi => "citationSources",
        Backend::VertexAi => "citations",
    };
    let with_metadata = |mut c: Map<String, Value>, query: &str, uri: &str| {
        if !query.is_empty() {
            c.insert(
                "groundingMetadata".into(),
                json!({"webSearchQueries": [query]}),
            );
        }
        if !uri.is_empty() {
            c.insert(
                "citationMetadata".into(),
                json!({citations_key: [{"uri": uri}]}),
            );
        }
        c
    };
    let api = Arc::new(Scripted::new(vec![
        serve(
            &[with_metadata(
                candidate("one ", "CONTINUATION", "t1"),
                "q1",
                "https://example.com/a",
            )],
            true,
        ),
        serve(
            &[with_metadata(
                candidate("two ", "CONTINUATION", "t2"),
                "q2",
                "https://example.com/b",
            )],
            true,
        ),
        serve(&[candidate("three", "STOP", "")], true),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let mut fin = None;
    for r in collect(llm.generate_content(continuation_request(), true)).await {
        let resp = r.expect("GenerateContent");
        if !resp.partial {
            fin = Some(resp);
        }
    }
    let fin = fin.expect("no final response");
    assert_eq!(
        fin.grounding_metadata,
        Some(GroundingMetadata {
            web_search_queries: vec!["q2".into()],
            ..Default::default()
        }),
        "final grounding metadata"
    );
    assert_eq!(
        fin.citation_metadata,
        Some(CitationMetadata {
            citations: vec![Citation {
                uri: "https://example.com/b".into(),
                ..Default::default()
            }],
        }),
        "final citation metadata"
    );
    check_requests(&api, &["", "t1", "t2"]);
}

/// A MAX_TOKENS stop that carries a token is returned as it is, even after a
/// CONTINUATION pause was resumed, since resuming it makes the API return a 400.
#[rstest]
#[case::gemini_api_unary(Backend::GeminiApi, false)]
#[case::gemini_api_stream(Backend::GeminiApi, true)]
#[case::vertex_ai_unary(Backend::VertexAi, false)]
#[case::vertex_ai_stream(Backend::VertexAi, true)]
#[tokio::test]
async fn test_continuation_max_tokens_not_resumed(#[case] backend: Backend, #[case] stream: bool) {
    let api = Arc::new(Scripted::new(vec![
        serve(&[candidate("one ", "CONTINUATION", "t1")], stream),
        serve(&[candidate("two", "MAX_TOKENS", "t2")], stream),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let mut fin = None;
    for r in collect(llm.generate_content(continuation_request(), stream)).await {
        let resp = r.expect("GenerateContent");
        if !resp.partial {
            fin = Some(resp);
        }
    }
    let fin = fin.expect("no final response");
    assert_eq!(fin.finish_reason, Some(FinishReason::MaxTokens));
    check_requests(&api, &["", "t1"]);
}

/// A first response without a finish reason but with a token is resumed, and the
/// responses are merged.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn continuation_unspecified_reason_resumes(#[case] backend: Backend) {
    let api = Arc::new(Scripted::new(vec![
        serve(&[candidate("one ", "", "t1")], false),
        serve(&[candidate("two", "STOP", "")], false),
    ]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let got = collect(llm.generate_content(continuation_request(), false)).await;
    assert_eq!(got.len(), 1);
    let got = got.into_iter().next().unwrap().expect("GenerateContent");
    assert_eq!(texts(&got.content), ["one ", "two"]);
    assert_eq!(got.finish_reason, Some(FinishReason::Stop));
    let bodies = api.bodies.lock();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].get("continuationToken").is_none());
    assert_eq!(
        bodies[1].get("continuationToken"),
        Some(&json!(STANDARD.encode("t1")))
    );
}

/// A stream chunk with prompt feedback is not resumed, whatever its token and reason.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn continuation_prompt_feedback_stops(#[case] backend: Backend) {
    let chunk = json!({
        "candidates": [candidate("one ", "CONTINUATION", "t1")],
        "promptFeedback": {"blockReason": "SAFETY"},
    });
    let api = Arc::new(Scripted::new(vec![(
        200,
        "text/event-stream",
        format!("data: {chunk}\n\n"),
    )]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    for r in collect(llm.generate_content(continuation_request(), true)).await {
        r.expect("GenerateContent");
    }
    assert_eq!(api.bodies.lock().len(), 1, "request count");
}

/// A stream that keeps failing after its last checkpoint is reconnected at most
/// `maxStreamReconnectsWithoutProgress` (3) times, then surfaces the transport error.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn continuation_stream_reconnect_limit(#[case] backend: Backend) {
    let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"a"}],"role":"model"},"continuationToken":"dG9r"}]}"#;
    let mut script: Vec<Response> = vec![(200, "text/event-stream", format!("data: {chunk}\n\n"))];
    for _ in 0..4 {
        script.push((0, "", String::new()));
    }
    let api = Arc::new(Scripted::new(script));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let got = collect(llm.generate_content(continuation_request(), true)).await;

    let bodies = api.bodies.lock();
    assert_eq!(bodies.len(), 5, "request count");
    assert!(bodies[0].get("continuationToken").is_none());
    for (i, body) in bodies.iter().enumerate().skip(1) {
        assert_eq!(
            body.get("continuationToken"),
            Some(&json!("dG9r")),
            "request {i} continuation token"
        );
    }
    assert_eq!(got.len(), 2, "items: {got:?}");
    let first = got[0].as_ref().expect("first item");
    assert!(first.partial);
    assert_eq!(texts(&first.content), ["a"]);
    assert!(
        matches!(&got[1], Err(ModelError::Transport(_))),
        "last item: {:?}",
        got[1]
    );
}

/// An API error before the stream's first chunk is surfaced at once, without a
/// continuation retry.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn continuation_stream_api_error_before_first_chunk(#[case] backend: Backend) {
    let api = Arc::new(Scripted::new(vec![(
        500,
        "application/json",
        r#"{"error":{"code":500,"status":"INTERNAL","message":"x"}}"#.into(),
    )]));
    let llm = new_continuation_model(client_config(backend, api.clone())).await;
    let got = collect(llm.generate_content(continuation_request(), true)).await;
    assert_eq!(api.bodies.lock().len(), 1, "request count");
    assert_eq!(got.len(), 1, "items: {got:?}");
    assert!(
        matches!(&got[0], Err(ModelError::Api { code: 500, .. })),
        "item: {:?}",
        got[0]
    );
}
