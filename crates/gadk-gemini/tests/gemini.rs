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

//! Port of ADK-Go `model/gemini/gemini_test.go`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use common::*;
use futures::StreamExt;
use gadk_gemini::internal::{build_request_body, preserve_empty_text_thought_signature_parts};
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::*;
use gadk_model::*;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

const OK_JSON: &str = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;

const CONTINUE_TEXT: &str = "Continue processing previous requests as instructed. Exit or provide a summary if no more outputs are needed.";

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn env(pairs: &[(&str, &str)]) -> Arc<MapEnv> {
    Arc::new(MapEnv(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
    ))
}

/// Port of Go readResponse: (partial text, final text).
async fn read_response(
    s: futures::stream::BoxStream<'static, Result<LlmResponse, ModelError>>,
) -> Result<(String, String), ModelError> {
    let mut partial = String::new();
    let mut fin = String::new();
    let mut s = s;
    while let Some(item) = s.next().await {
        let resp = item?;
        let content = resp.content.expect("encountered an empty response");
        assert!(!content.parts.is_empty(), "encountered an empty response");
        let text = &content.parts[0].text;
        if resp.partial {
            partial.push_str(text);
        } else {
            fin.push_str(text);
        }
    }
    Ok((partial, fin))
}

fn sig_part(sig: &'static [u8]) -> Part {
    Part {
        thought_signature: Bytes::from_static(sig),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_model_generate() {
    let file = "TestModel_Generate_ok.httprr";
    let model = GeminiModel::new("gemini-2.5-flash", gemini_test_config(file, rr(file)))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: text("What is the capital of France? One word."),
        config: Some(GenerateContentConfig {
            temperature: Some(0.0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out: Vec<_> = model.generate_content(req, false).collect().await;
    assert_eq!(out.len(), 1);
    let mut got = out.into_iter().next().unwrap().unwrap();
    got.avg_logprobs = 0.0;
    let want = json!({
        "content": {"parts": [{"text": "Paris"}], "role": "model"},
        "usageMetadata": {
            "candidatesTokenCount": 1,
            "promptTokenCount": 11,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 11}],
            "thoughtsTokenCount": 34,
            "totalTokenCount": 46
        },
        "modelVersion": "gemini-2.5-flash",
        "finishReason": "STOP"
    });
    assert_eq!(serde_json::to_value(&got).unwrap(), want);
}

#[tokio::test]
async fn test_model_generate_stream() {
    let file = "TestModel_GenerateStream_ok.httprr";
    let model = GeminiModel::new("gemini-2.5-flash", gemini_test_config(file, rr(file)))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: text("What is the capital of France? One word."),
        config: Some(GenerateContentConfig {
            temperature: Some(0.0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (partial, fin) = read_response(model.generate_content(req, true))
        .await
        .unwrap();
    assert_eq!(partial, "Paris");
    // Since GenerateStream aggregates partial events, the final text is the same.
    assert_eq!(fin, "Paris");
}

#[tokio::test]
async fn test_model_generate_preserves_empty_text_for_trailing_thought_signature() {
    let signature: &'static [u8] = b"trailing-signature";
    let mut aggregator = StreamingResponseAggregator::new();
    let chunk = |parts: Vec<Part>, finish: Option<FinishReason>| GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content::from_parts(parts, "model")),
            finish_reason: finish,
            ..Default::default()
        }],
        ..Default::default()
    };
    let chunks = vec![
        chunk(vec![Part::from_text("Hel")], None),
        chunk(vec![Part::from_text("lo")], None),
        chunk(vec![sig_part(signature)], Some(FinishReason::Stop)),
    ];
    for c in chunks {
        for r in aggregator.process_response(c) {
            r.unwrap();
        }
    }
    let aggregated = aggregator.close().expect("Close() returned no content");
    let content = aggregated.content.expect("Close() returned no content");
    assert_eq!(content.parts.len(), 2);
    assert_eq!(content.parts[0].text, "Hello");
    assert_eq!(content.parts[1].thought_signature.as_ref(), signature);

    let http = FakeHttp::json(OK_JSON);
    let model = GeminiModel::new("gemini-3-flash-preview", vertex_config(http.clone()))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![content, Content::from_text("Continue", "user")],
        ..Default::default()
    };
    for r in model.generate_content(req, false).collect::<Vec<_>>().await {
        r.unwrap();
    }

    let body = http.last_body();
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 2);
    let parts = contents[0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], json!("Hello"));
    assert_eq!(
        parts[1].get("text"),
        Some(&json!("")),
        "want present empty text"
    );
    assert!(
        parts[1].get("thoughtSignature").is_some(),
        "serialized trailing part lost thoughtSignature"
    );
}

#[test]
fn test_config_preserving_empty_text_thought_signatures() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let calls = provider_calls.clone();
    let original_provider: ExtrasRequestProvider = Arc::new(move |mut body| {
        calls.fetch_add(1, Ordering::SeqCst);
        body.insert("providerMarker".into(), json!(true));
        // The Go test hands the parts below to the wrapped provider directly; here the
        // provider supplies them when the built body has no contents.
        if body.get("contents").is_none_or(|c| c == &json!([])) {
            body.insert(
                "contents".into(),
                json!([{"role": "model", "parts": [
                    {"text": "Hello", "thoughtSignature": "first"},
                    {"thoughtSignature": "second"},
                    {"functionCall": {"name": "tool"}, "thoughtSignature": "tool-signature"},
                    {"text": "ordinary"},
                    {"thought": true, "thoughtSignature": "thought-only"},
                    {"thought": true},
                    {"speechMetadata": {"speaker": "speaker-1"}, "thoughtSignature": "speech"}
                ]}]),
            );
        }
        body
    });

    let got = build_request_body(
        Backend::GeminiApi,
        &[],
        &GenerateContentConfig::default(),
        Some(&original_provider),
    )
    .unwrap();

    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "original ExtrasRequestProvider calls"
    );
    assert_eq!(
        got.get("providerMarker"),
        Some(&json!(true)),
        "original ExtrasRequestProvider result was not preserved"
    );
    let parts = got["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], json!("Hello"), "signed text part changed");
    assert_eq!(parts[0]["thoughtSignature"], json!("first"));
    assert_eq!(
        parts[1].get("text"),
        Some(&json!("")),
        "signature-only part"
    );
    assert_eq!(parts[1]["thoughtSignature"], json!("second"));
    assert!(
        parts[2].get("text").is_none(),
        "function-call part gained a second data field"
    );
    assert_eq!(parts[3]["text"], json!("ordinary"));
    assert_eq!(parts[4].get("text"), Some(&json!("")), "thought-only part");
    assert!(
        parts[5].get("text").is_none(),
        "metadata-only part without a thought signature gained a text field"
    );
    assert_eq!(
        parts[6].get("text"),
        Some(&json!("")),
        "speech-metadata part"
    );

    // The caller's provider itself is not replaced: calling it directly does not
    // normalize signature-only parts.
    let direct = original_provider(obj(
        json!({"contents": [{"parts": [{"thoughtSignature": "original"}]}]}),
    ));
    assert!(
        direct["contents"][0]["parts"][0].get("text").is_none(),
        "the caller's provider was replaced"
    );
}

#[test]
fn test_config_preserving_empty_text_thought_signatures_nil() {
    let contents = vec![Content::from_parts(vec![sig_part(b"signature")], "")];
    let got = build_request_body(
        Backend::GeminiApi,
        &contents,
        &GenerateContentConfig::default(),
        None,
    )
    .unwrap();
    let part = &got["contents"][0]["parts"][0];
    assert_eq!(
        part.get("text"),
        Some(&json!("")),
        "signature-only part text; want present empty text"
    );
}

#[tokio::test]
async fn test_config_preserving_empty_text_thought_signatures_provider_order() {
    let client_calls = Arc::new(AtomicUsize::new(0));
    let cc = client_calls.clone();
    let client_provider: ExtrasRequestProvider = Arc::new(move |_body| {
        cc.fetch_add(1, Ordering::SeqCst);
        obj(json!({"contents": [{"parts": [{"thoughtSignature": "client-signature"}]}]}))
    });
    let request_provider: ExtrasRequestProvider = Arc::new(|_body| {
        obj(json!({"contents": [{"parts": [{"thoughtSignature": "signature"}]}]}))
    });

    let http = FakeHttp::json(OK_JSON);
    let mut cfg = vertex_config(http.clone());
    cfg.http_options.extras_request_provider = Some(client_provider);
    let model = GeminiModel::new("gemini-3-flash-preview", cfg)
        .await
        .unwrap();
    let req = LlmRequest {
        contents: text("hello"),
        http_options: Some(HttpOptions {
            extras_request_provider: Some(request_provider),
            ..Default::default()
        }),
        ..Default::default()
    };
    for r in model.generate_content(req, false).collect::<Vec<_>>().await {
        r.unwrap();
    }
    assert_eq!(
        client_calls.load(Ordering::SeqCst),
        0,
        "client provider must not run when a request provider is set"
    );
    let body = http.last_body();
    let part = &body["contents"][0]["parts"][0];
    assert_eq!(part["thoughtSignature"], json!("signature"));
    assert_eq!(
        part.get("text"),
        Some(&json!("")),
        "provider-created signature-only part; want present empty text"
    );
}

#[test]
fn test_normalize_content_free_thought_signature_part() {
    let mut body = obj(json!({
        "contents": [{"role": "model", "parts": [
            {"text": "server-side media result"},
            {"thoughtSignature": "call-context"}
        ]}]
    }));
    preserve_empty_text_thought_signature_parts(&mut body);
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], json!("server-side media result"));
    assert_eq!(parts[1].get("text"), Some(&json!("")));
    assert_eq!(parts[1]["thoughtSignature"], json!("call-context"));
}

#[test]
fn test_preserve_empty_text_thought_signature_parts_map_slices() {
    // Go distinguishes []map[string]any from []any; serde_json has a single array type.
    let mut body = obj(json!({"contents": [{"parts": [{"thoughtSignature": "signature"}]}]}));
    preserve_empty_text_thought_signature_parts(&mut body);
    assert_eq!(
        body["contents"][0]["parts"][0].get("text"),
        Some(&json!(""))
    );
    // Non-object entries and non-array values are skipped, not errors.
    let mut odd = obj(
        json!({"contents": [1, "x", {"parts": "nope"}, {"parts": [2, {"thoughtSignature": "s"}]}]}),
    );
    preserve_empty_text_thought_signature_parts(&mut odd);
    assert_eq!(odd["contents"][3]["parts"][1].get("text"), Some(&json!("")));
    let mut none = obj(json!({"contents": {"parts": []}}));
    preserve_empty_text_thought_signature_parts(&mut none);
}

#[tokio::test]
async fn test_model_preserves_client_extras_request_provider() {
    let http = FakeHttp::json(OK_JSON);
    let mut cfg = vertex_config(http.clone());
    cfg.http_options.extras_request_provider = Some(Arc::new(|mut body| {
        body.insert("clientMarker".into(), json!("preserved"));
        body
    }));
    let model = GeminiModel::new("gemini-3-flash-preview", cfg)
        .await
        .unwrap();
    let req = LlmRequest {
        contents: text("hello"),
        ..Default::default()
    };
    for r in model.generate_content(req, false).collect::<Vec<_>>().await {
        r.unwrap();
    }
    assert_eq!(http.last_body()["clientMarker"], json!("preserved"));
}

#[tokio::test]
async fn test_model_direct_generate_with_nil_config() {
    for stream in [false, true] {
        let http = if stream {
            FakeHttp::sse(&format!("data: {OK_JSON}\n\n"))
        } else {
            FakeHttp::json(OK_JSON)
        };
        let model = GeminiModel::new("gemini-3-flash-preview", vertex_config(http.clone()))
            .await
            .unwrap();
        let req = LlmRequest {
            contents: vec![
                Content::from_parts(vec![sig_part(b"signature")], "model"),
                Content::from_text("Continue", "user"),
            ],
            ..Default::default()
        };
        let before = req.clone();
        for r in model
            .generate_content(req, stream)
            .collect::<Vec<_>>()
            .await
        {
            r.unwrap_or_else(|e| panic!("stream={stream}: {e}"));
        }
        let body = http.last_body();
        let part = &body["contents"][0]["parts"][0];
        assert_eq!(
            part.get("text"),
            Some(&json!("")),
            "stream={stream}: signature-only part; want present empty text"
        );
        assert!(
            before.config.is_none(),
            "direct generate mutated request config"
        );
    }
}

#[tokio::test]
async fn test_model_generate_stream_preserves_empty_text_for_thought_signature() {
    let http = FakeHttp::sse(&format!("data: {OK_JSON}\n\n"));
    let model = GeminiModel::new("gemini-3-flash-preview", vertex_config(http.clone()))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![
            Content::from_parts(vec![sig_part(b"stream-signature")], "model"),
            Content::from_text("Continue", "user"),
        ],
        ..Default::default()
    };
    for r in model.generate_content(req, true).collect::<Vec<_>>().await {
        r.unwrap();
    }
    let body = http.last_body();
    let part = &body["contents"][0]["parts"][0];
    assert_eq!(part.get("text"), Some(&json!("")));
    assert!(
        part.get("thoughtSignature").is_some(),
        "serialized streaming history part lost thoughtSignature"
    );
}

fn assert_tracking_headers(req: &http::Request<Bytes>) {
    for name in ["user-agent", "x-goog-api-client"] {
        let values: Vec<_> = req.headers().get_all(name).iter().collect();
        assert_eq!(
            values.len(),
            1,
            "{name} header should have exactly one value, got {values:?}"
        );
        let v = values[0].to_str().unwrap();
        assert!(
            v.contains("gadk-rust/") && v.contains(" rust/"),
            "{name} header should contain both 'gadk-rust/' and ' rust/', got {v:?}"
        );
    }
}

#[tokio::test]
async fn test_model_tracking_headers() {
    // (a) vertex_disabled: the Go fixture was recorded with gemini-2.5-flash while the
    // test asks for gemini-2.0-flash, so the replay misses exactly as in Go; the error is
    // tolerated (Go only logs it) and only the headers are checked.
    {
        let file = "TestModel_TrackingHeaders_verifies_headers_are_set_vertex_disabled.httprr";
        let recorder = Recorder::new(rr(file));
        let mut cfg = gemini_test_config(file, recorder.clone());
        cfg.env = env(&[("GOOGLE_GENAI_USE_VERTEXAI", "false")]);
        let model = GeminiModel::new("gemini-2.0-flash", cfg).await.unwrap();
        let req = LlmRequest {
            contents: text("ping"),
            ..Default::default()
        };
        for r in model.generate_content(req, false).collect::<Vec<_>>().await {
            if let Err(e) = r {
                eprintln!("GenerateContent finished with error (expected): {e}");
            }
        }
        let seen = recorder.seen.lock();
        assert_eq!(seen.len(), 1, "HTTP request was not intercepted");
        assert_tracking_headers(&seen[0]);
    }

    // (b) vertex_enabled (adapted: Go uses express mode, gadk uses credentials).
    {
        let file =
            "TestModel_TrackingHeaders_verifies_headers_are_set_vertex_enabled_credentials.httprr";
        let recorder = Recorder::new(rr(file));
        let cfg = ClientConfig {
            api_key: None,
            credentials: Some(google_cloud_auth::credentials::anonymous::Builder::new().build()),
            http_client: Some(recorder.clone()),
            env: env(&[
                ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
                ("GOOGLE_CLOUD_PROJECT", "test-project"),
                ("GOOGLE_CLOUD_LOCATION", "us-central1"),
            ]),
            ..Default::default()
        };
        let model = GeminiModel::new("gemini-2.0-flash", cfg).await.unwrap();
        let req = LlmRequest {
            contents: text("ping"),
            ..Default::default()
        };
        let out: Vec<_> = model.generate_content(req, false).collect().await;
        assert_eq!(out.len(), 1);
        let resp = out.into_iter().next().unwrap().unwrap();
        assert_eq!(resp.content.unwrap().parts[0].text, "pong");
        let seen = recorder.seen.lock();
        assert_eq!(seen.len(), 1, "HTTP request was not intercepted");
        assert_tracking_headers(&seen[0]);
        assert_eq!(
            seen[0].uri().to_string(),
            "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/test-project/locations/us-central1/publishers/google/models/gemini-2.0-flash:generateContent"
        );
    }
}

#[tokio::test]
async fn test_model_no_side_effects() {
    let http = FakeHttp::json(OK_JSON);
    let mut cfg = ClientConfig {
        api_key: Some("fake-api-key".into()),
        http_client: Some(http.clone()),
        env: env(&[]),
        ..Default::default()
    };
    cfg.http_options
        .headers
        .insert("x-custom", http::HeaderValue::from_static("caller"));
    let headers_before = cfg.http_options.headers.clone();
    let client_before = cfg.http_client.clone().unwrap();

    let model = GeminiModel::new("gemini-2.0-flash", cfg.clone())
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![
            Content::from_text("hi", "user"),
            Content::from_text("hello", "model"),
        ],
        ..Default::default()
    };
    let before = req.clone();
    for r in model.generate_content(req, false).collect::<Vec<_>>().await {
        r.unwrap();
    }

    assert_eq!(cfg.http_options.headers, headers_before);
    assert!(Arc::ptr_eq(
        cfg.http_client.as_ref().unwrap(),
        &client_before
    ));
    assert_eq!(before.contents.len(), 2, "caller's request was modified");
    assert!(before.config.is_none());
    // The sent request did get the continuation turn (on the model's private copy).
    assert_eq!(http.last_body()["contents"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_model_respects_request_model() {
    let cases = [
        ("gemini-2.5-flash", "", "models/gemini-2.5-flash:"),
        (
            "gemini-2.5-flash",
            "gemini-2.0-flash",
            "models/gemini-2.0-flash:",
        ),
        ("gemini-2.5-flash", "gemini-other", "models/gemini-other:"),
    ];
    for (constructor, req_model, want) in cases {
        let http = FakeHttp::json(OK_JSON);
        let cfg = ClientConfig {
            api_key: Some("fakekey".into()),
            http_client: Some(http.clone()),
            env: env(&[]),
            ..Default::default()
        };
        let model = GeminiModel::new(constructor, cfg).await.unwrap();
        let req = LlmRequest {
            model: req_model.into(),
            contents: text("ping"),
            ..Default::default()
        };
        let _ = model.generate_content(req, false).collect::<Vec<_>>().await;
        let uri = http.last_uri();
        assert!(uri.contains(want), "URL {uri:?} should contain {want:?}");
    }
}

// ---- extras (not in the Go suite) ----------------------------------------------------

#[tokio::test]
async fn model_empty_candidates_errors() {
    let http = FakeHttp::json("{}");
    let model = GeminiModel::new("gemini-2.5-flash", vertex_config(http))
        .await
        .unwrap();
    let out: Vec<_> = model
        .generate_content(
            LlmRequest {
                contents: text("hi"),
                ..Default::default()
            },
            false,
        )
        .collect()
        .await;
    assert_eq!(out.len(), 1);
    assert!(
        matches!(out[0], Err(ModelError::EmptyResponse)),
        "got {:?}",
        out[0]
    );
}

#[tokio::test]
async fn model_appends_continuation_user_turn() {
    let http = FakeHttp::json(OK_JSON);
    let model = GeminiModel::new("gemini-2.5-flash", vertex_config(http.clone()))
        .await
        .unwrap();
    let req = LlmRequest {
        contents: vec![
            Content::from_text("hi", "user"),
            Content::from_text("hello", "model"),
        ],
        ..Default::default()
    };
    for r in model.generate_content(req, false).collect::<Vec<_>>().await {
        r.unwrap();
    }
    let body = http.last_body();
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(
        contents.last().unwrap(),
        &json!({"parts": [{"text": CONTINUE_TEXT}], "role": "user"})
    );
}

#[tokio::test]
async fn model_does_not_append_continuation_to_empty_contents() {
    let http = FakeHttp::json(OK_JSON);
    let model = GeminiModel::new("gemini-2.5-flash", vertex_config(http.clone()))
        .await
        .unwrap();
    let _ = model
        .generate_content(LlmRequest::default(), false)
        .collect::<Vec<_>>()
        .await;
    assert!(
        http.last_body()
            .get("contents")
            .is_none_or(|c| c == &json!([]))
    );
}

#[tokio::test]
async fn model_api_error_is_reported() {
    let http = Arc::new(FakeHttp {
        status: 429,
        content_type: "application/json",
        body: r#"{"error":{"code":429,"message":"slow down","status":"RESOURCE_EXHAUSTED"}}"#
            .into(),
        seen: Default::default(),
    });
    let model = GeminiModel::new("gemini-2.5-flash", vertex_config(http))
        .await
        .unwrap();
    for stream in [false, true] {
        let out: Vec<_> = model
            .generate_content(
                LlmRequest {
                    contents: text("hi"),
                    ..Default::default()
                },
                stream,
            )
            .collect()
            .await;
        assert_eq!(out.len(), 1, "stream={stream}");
        match &out[0] {
            Err(ModelError::Api { code, status, .. }) => {
                assert_eq!(*code, 429);
                assert_eq!(status, "RESOURCE_EXHAUSTED");
            }
            other => panic!("stream={stream}: got {other:?}"),
        }
    }
}

#[tokio::test]
async fn model_merges_tracking_headers_with_existing_user_agent() {
    let http = FakeHttp::json(OK_JSON);
    let mut cfg = vertex_config(http.clone());
    cfg.http_options
        .headers
        .insert("user-agent", http::HeaderValue::from_static("my-app/1"));
    let model = GeminiModel::new("gemini-2.5-flash", cfg).await.unwrap();
    for r in model
        .generate_content(
            LlmRequest {
                contents: text("hi"),
                ..Default::default()
            },
            false,
        )
        .collect::<Vec<_>>()
        .await
    {
        r.unwrap();
    }
    let seen = http.seen.lock();
    let ua: Vec<_> = seen[0].headers().get_all("user-agent").iter().collect();
    assert_eq!(ua.len(), 1);
    let ua = ua[0].to_str().unwrap();
    assert!(ua.starts_with("my-app/1 gadk-rust/"), "got {ua:?}");
    assert_eq!(
        seen[0].headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn model_reports_google_variant() {
    let m = GeminiModel::new("gemini-2.5-flash", vertex_config(FakeHttp::json(OK_JSON)))
        .await
        .unwrap();
    assert_eq!(m.google_variant(), Some(Backend::VertexAi));
    assert_eq!(m.name(), "gemini-2.5-flash");
    let c = GeminiModel::new_concrete(
        "gemini-2.5-flash",
        ClientConfig {
            api_key: Some("k".into()),
            env: env(&[]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.resolved().backend, Backend::GeminiApi);
}
