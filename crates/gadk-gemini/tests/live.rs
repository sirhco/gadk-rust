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

//! Live (bidi WebSocket) connection, pinned to genai v1.75.0 `live.go` / `live_converters.go`
//! (Connect, the setup message, Session send/receive) and ADK-Go
//! `internal/llminternal/googlellm/live_connection.go` (SendHistory, SendContent,
//! SendRealtime, Recv). Every test runs against an in-process server, for both backends.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use common::ws::WsServer;
use gadk_gemini::{ClientConfig, GeminiError, GeminiModel, LiveConnection, MapEnv, RealtimeInput};
use gadk_genai::{Blob, Content, FunctionResponse, LiveConnectConfig};
use gadk_model::{Backend, HttpOptions, LlmResponse};
use google_cloud_auth::credentials::{
    CacheableResource, Credentials, CredentialsProvider, EntityTag,
};
use google_cloud_auth::errors::CredentialsError;
use http::{HeaderMap, HeaderValue};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

const MODEL: &str = "gemini-live-x";
const PNG: [u8; 9] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00];

/// Credentials returning fixed headers.
#[derive(Debug)]
struct FixedHeaders(HeaderMap);

impl CredentialsProvider for FixedHeaders {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
        Ok(CacheableResource::New {
            entity_tag: EntityTag::new(),
            data: self.0.clone(),
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

fn anonymous() -> Credentials {
    google_cloud_auth::credentials::anonymous::Builder::new().build()
}

fn bearer() -> Credentials {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer t"),
    );
    Credentials::from(FixedHeaders(headers))
}

/// A client config pointing at `base_url`. Vertex AI sets project, location and (anonymous)
/// credentials on the config itself: genai clears env-derived project/location when a
/// custom base URL is used.
fn config(backend: Backend, base_url: String) -> ClientConfig {
    let http_options = HttpOptions {
        base_url: Some(base_url),
        ..Default::default()
    };
    let env = Arc::new(MapEnv(Default::default()));
    match backend {
        Backend::GeminiApi => ClientConfig {
            backend: Some(Backend::GeminiApi),
            api_key: Some("k".into()),
            http_options,
            env,
            ..Default::default()
        },
        Backend::VertexAi => ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some("p".into()),
            location: Some("l".into()),
            credentials: Some(anonymous()),
            http_options,
            env,
            ..Default::default()
        },
    }
}

/// The setup `model` genai sends (`tModelFullName`).
fn full_model(backend: Backend, model: &str) -> String {
    match backend {
        Backend::GeminiApi => format!("models/{model}"),
        Backend::VertexAi => format!("projects/p/locations/l/publishers/google/models/{model}"),
    }
}

async fn connect_with(
    server: &WsServer,
    backend: Backend,
    model: &str,
    cfg: LiveConnectConfig,
) -> LiveConnection {
    let m = GeminiModel::new_concrete(model, config(backend, server.base_url()))
        .await
        .unwrap();
    m.live_connect(cfg).await.unwrap()
}

async fn connect(server: &WsServer, backend: Backend) -> LiveConnection {
    connect_with(server, backend, MODEL, LiveConnectConfig::default()).await
}

fn content(v: Value) -> Content {
    serde_json::from_value(v).unwrap()
}

fn blob(data: &[u8], mime: &str) -> Blob {
    Blob {
        data: Bytes::copy_from_slice(data),
        mime_type: mime.into(),
        ..Default::default()
    }
}

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

async fn recv(conn: &mut LiveConnection) -> LlmResponse {
    conn.recv().await.unwrap().expect("connection closed")
}

#[tokio::test]
async fn live_setup_frame_and_url_gemini_api() {
    let server = WsServer::start(vec![]).await;
    let mut cfg = config(Backend::GeminiApi, server.base_url());
    cfg.http_options
        .headers
        .insert("x-custom", HeaderValue::from_static("v"));
    let m = GeminiModel::new_concrete(MODEL, cfg).await.unwrap();
    let live_cfg: LiveConnectConfig = serde_json::from_value(json!({
        "responseModalities": ["AUDIO"],
        "temperature": 0.5,
        "systemInstruction": {"parts": [{"text": "be brief"}]},
        "tools": [{"functionDeclarations": [{"name": "f", "description": "d"}]}],
        "inputAudioTranscription": {},
    }))
    .unwrap();
    let _conn = m.live_connect(live_cfg).await.unwrap();

    assert_eq!(
        server.path().as_deref(),
        Some("/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent")
    );
    let headers = server.headers();
    assert_eq!(headers.get("x-goog-api-key").unwrap(), "k");
    assert_eq!(headers.get("x-custom").unwrap(), "v");
    assert!(headers.get(http::header::AUTHORIZATION).is_none());
    let frames = server.frames(1).await;
    assert_eq!(
        frames[0],
        json!({"setup": {
            "model": "models/gemini-live-x",
            "generationConfig": {"responseModalities": ["AUDIO"], "temperature": 0.5},
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "tools": [{"functionDeclarations": [{"name": "f", "description": "d"}]}],
            "inputAudioTranscription": {},
        }})
    );
}

#[tokio::test]
async fn live_setup_frame_and_url_vertex() {
    let server = WsServer::start(vec![]).await;
    let mut cfg = config(Backend::VertexAi, server.base_url());
    cfg.credentials = Some(bearer());
    cfg.http_options
        .headers
        .insert("x-custom", HeaderValue::from_static("v"));
    let m = GeminiModel::new_concrete(MODEL, cfg).await.unwrap();
    let live_cfg: LiveConnectConfig = serde_json::from_value(json!({
        "responseModalities": ["AUDIO"],
        "temperature": 0.5,
        "systemInstruction": {"parts": [{"text": "be brief"}]},
        "tools": [{"functionDeclarations": [{"name": "f", "description": "d"}]}],
        "explicitVadSignal": true,
    }))
    .unwrap();
    let _conn = m.live_connect(live_cfg).await.unwrap();

    assert_eq!(
        server.path().as_deref(),
        Some("/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent")
    );
    let headers = server.headers();
    assert_eq!(
        headers.get(http::header::AUTHORIZATION).unwrap(),
        "Bearer t"
    );
    assert_eq!(headers.get("x-custom").unwrap(), "v");
    assert!(headers.get("x-goog-api-key").is_none());
    let frames = server.frames(1).await;
    assert_eq!(
        frames[0],
        json!({"setup": {
            "model": "projects/p/locations/l/publishers/google/models/gemini-live-x",
            "generationConfig": {"responseModalities": ["AUDIO"], "temperature": 0.5},
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "tools": [{"functionDeclarations": [{"name": "f", "description": "d"}]}],
            "explicitVadSignal": true,
        }})
    );
}

/// genai `liveConnectConfigToMldev` rejects fields the Gemini API does not support.
#[tokio::test]
async fn live_setup_rejects_vertex_only_fields_on_gemini_api() {
    let server = WsServer::start(vec![]).await;
    let m = GeminiModel::new_concrete(MODEL, config(Backend::GeminiApi, server.base_url()))
        .await
        .unwrap();
    for cfg in [
        json!({"explicitVadSignal": true}),
        json!({"sessionResumption": {"handle": "h", "transparent": true}}),
    ] {
        let cfg: LiveConnectConfig = serde_json::from_value(cfg).unwrap();
        let err = m.live_connect(cfg).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("only supported in Gemini Enterprise Agent Platform mode"),
            "{err}"
        );
    }
}

/// genai `Connect`: Vertex AI with a custom base URL but no project/location uses the base
/// URL's own path (a proxy); the Gemini API joins its path below the base URL's.
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_custom_base_path(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let base = format!("ws://127.0.0.1:{}/proxy/bidi", server.port);
    let mut cfg = config(backend, base);
    if backend == Backend::VertexAi {
        cfg.project = None;
        cfg.location = None;
        cfg.credentials = None;
    }
    let m = GeminiModel::new_concrete(MODEL, cfg).await.unwrap();
    let _conn = m.live_connect(LiveConnectConfig::default()).await.unwrap();
    let (path, model) = match backend {
        Backend::GeminiApi => (
            "/proxy/bidi/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
            "models/gemini-live-x",
        ),
        Backend::VertexAi => ("/proxy/bidi/", "publishers/google/models/gemini-live-x"),
    };
    assert_eq!(server.path().as_deref(), Some(path));
    let frames = server.frames(1).await;
    assert_eq!(frames[0], json!({"setup": {"model": model}}));
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_send_methods_frames(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let conn = connect(&server, backend).await;

    conn.send_history(&[
        content(json!({"role": "user", "parts": [{"text": "hi"}]})),
        content(json!({"role": "model", "parts": [{"text": "hello"}]})),
    ])
    .await
    .unwrap();
    conn.send_content(content(json!({"role": "user", "parts": [{"text": "q"}]})))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::Blob(blob(&[1, 2], "audio/pcm")))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::ActivityStart)
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::ActivityEnd)
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::AudioStreamEnd)
        .await
        .unwrap();
    let fr: FunctionResponse =
        serde_json::from_value(json!({"id": "1", "name": "f", "response": {"r": 1}})).unwrap();
    conn.send_tool_response(vec![fr]).await.unwrap();

    let frames = server.frames(8).await;
    assert_eq!(
        frames[0],
        json!({"setup": {"model": full_model(backend, MODEL)}})
    );
    assert_eq!(
        frames[1..].to_vec(),
        vec![
            // SendHistory: last turn is the model's, so turnComplete is false (omitted).
            json!({"clientContent": {"turns": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "hello"}]},
            ]}}),
            json!({"clientContent": {
                "turns": [{"role": "user", "parts": [{"text": "q"}]}],
                "turnComplete": true,
            }}),
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&[1, 2]), "mimeType": "audio/pcm"}]}}),
            json!({"realtimeInput": {"activityStart": {}}}),
            json!({"realtimeInput": {"activityEnd": {}}}),
            json!({"realtimeInput": {"audioStreamEnd": true}}),
            json!({"toolResponse": {"functionResponses": [{"id": "1", "name": "f", "response": {"r": 1}}]}}),
        ]
    );
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_send_content_routes_function_responses(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let conn = connect(&server, backend).await;
    conn.send_content(content(json!({"role": "user", "parts": [
        {"functionResponse": {"id": "1", "name": "f", "response": {"a": 1}}},
        {"text": "ignored"},
        {"functionResponse": {"id": "2", "name": "g", "response": {"b": 2}}},
    ]})))
    .await
    .unwrap();
    let err = conn
        .send_content(content(json!({"role": "user"})))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("empty content"), "{err}");

    let frames = server.frames(2).await;
    assert_eq!(
        frames[1],
        json!({"toolResponse": {"functionResponses": [
            {"id": "1", "name": "f", "response": {"a": 1}},
            {"id": "2", "name": "g", "response": {"b": 2}},
        ]}})
    );
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_send_history_filters_audio(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let conn = connect(&server, backend).await;
    conn.send_history(&[
        content(json!({"role": "user", "parts": [
            {"text": "a"},
            {"inlineData": {"data": b64(&[1]), "mimeType": "audio/pcm"}},
        ]})),
        content(json!({"role": "model", "parts": [
            {"inlineData": {"data": b64(&[2]), "mimeType": "audio/wav"}},
        ]})),
        content(json!({"role": "user", "parts": [
            {"inlineData": {"data": b64(&[3]), "mimeType": "image/png"}},
        ]})),
    ])
    .await
    .unwrap();
    // A history left empty by the filter sends nothing.
    conn.send_history(&[content(json!({"role": "user", "parts": [
        {"inlineData": {"data": b64(&[4]), "mimeType": "audio/pcm"}},
    ]}))])
    .await
    .unwrap();
    conn.send_realtime(RealtimeInput::AudioStreamEnd)
        .await
        .unwrap();

    let frames = server.frames(3).await;
    assert_eq!(
        frames[1],
        json!({"clientContent": {
            "turns": [
                {"role": "user", "parts": [{"text": "a"}]},
                {"role": "user", "parts": [{"inlineData": {"data": b64(&[3]), "mimeType": "image/png"}}]},
            ],
            "turnComplete": true,
        }})
    );
    assert_eq!(
        frames[2],
        json!({"realtimeInput": {"audioStreamEnd": true}})
    );
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_send_realtime_mime_detection(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let conn = connect(&server, backend).await;
    conn.send_realtime(RealtimeInput::Blob(blob(&PNG, "")))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::Blob(blob(&[1, 2, 3], "")))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::Blob(blob(&[1], "image/jpeg")))
        .await
        .unwrap();
    let frames = server.frames(4).await;
    assert_eq!(
        frames[1..].to_vec(),
        vec![
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&PNG), "mimeType": "image/png"}]}}),
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&[1, 2, 3]), "mimeType": "audio/pcm"}]}}),
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&[1]), "mimeType": "image/jpeg"}]}}),
        ]
    );
}

/// The gemini-3.1 special cases of SendHistory / SendContent / SendRealtime: on the Gemini
/// API, blobs go to `audio`/`video` and single-text content to realtime `text`; history is
/// never sent (both backends).
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_gemini_31_routing(#[case] backend: Backend) {
    let model = "gemini-3.1-flash-live";
    let server = WsServer::start(vec![]).await;
    let conn = connect_with(&server, backend, model, LiveConnectConfig::default()).await;
    conn.send_history(&[content(json!({"role": "user", "parts": [{"text": "h"}]}))])
        .await
        .unwrap();
    conn.send_content(content(json!({"role": "user", "parts": [{"text": "q"}]})))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::Blob(blob(&PNG, "")))
        .await
        .unwrap();
    conn.send_realtime(RealtimeInput::Blob(blob(&[1], "")))
        .await
        .unwrap();
    let frames = server.frames(4).await;
    assert_eq!(
        frames[0],
        json!({"setup": {"model": full_model(backend, model)}})
    );
    let expected = match backend {
        Backend::GeminiApi => vec![
            json!({"realtimeInput": {"text": "q"}}),
            json!({"realtimeInput": {"video": {"data": b64(&PNG), "mimeType": "image/png"}}}),
            json!({"realtimeInput": {"audio": {"data": b64(&[1]), "mimeType": "audio/pcm"}}}),
        ],
        Backend::VertexAi => vec![
            json!({"clientContent": {"turns": [{"role": "user", "parts": [{"text": "q"}]}], "turnComplete": true}}),
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&PNG), "mimeType": "image/png"}]}}),
            json!({"realtimeInput": {"mediaChunks": [{"data": b64(&[1]), "mimeType": "audio/pcm"}]}}),
        ],
    };
    assert_eq!(frames[1..].to_vec(), expected);
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_first_recv_is_setup_complete(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let mut conn = connect(&server, backend).await;
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_setup_requires_setup_complete(#[case] backend: Backend) {
    let server = WsServer::start_raw(vec![json!({"serverContent": {"turnComplete": true}})]).await;
    let m = GeminiModel::new_concrete(MODEL, config(backend, server.base_url()))
        .await
        .unwrap();
    let err = m
        .live_connect(LiveConnectConfig::default())
        .await
        .unwrap_err();
    assert!(matches!(err, GeminiError::Live(_)), "{err:?}");
    assert!(err.to_string().contains("expected SetupComplete"), "{err}");
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_recv_maps_server_content_and_tool_call(#[case] backend: Backend) {
    let server = WsServer::start(vec![
        json!({"serverContent": {"modelTurn": {"role": "model", "parts": [{"text": "hi"}]}, "turnComplete": true}}),
        json!({"serverContent": {"interrupted": true}}),
        json!({"toolCall": {"functionCalls": [{"id": "c1", "name": "f", "args": {"x": 1}}]}}),
        json!({"error": {"code": 500, "message": "boom"}}),
    ])
    .await;
    let mut conn = connect(&server, backend).await;
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            content: Some(content(json!({"role": "model", "parts": [{"text": "hi"}]}))),
            turn_complete: true,
            ..Default::default()
        }
    );
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            interrupted: true,
            ..Default::default()
        }
    );
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            content: Some(content(json!({"role": "model", "parts": [
                {"functionCall": {"id": "c1", "name": "f", "args": {"x": 1}}},
            ]}))),
            ..Default::default()
        }
    );
    let err = conn.recv().await.unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_recv_go_away_yields_empty_response(#[case] backend: Backend) {
    let server = WsServer::start(vec![
        json!({"goAway": {"timeLeft": "5s"}}),
        json!({"serverContent": {"modelTurn": {"role": "model", "parts": [{"text": "after"}]}}}),
    ])
    .await;
    let mut conn = connect(&server, backend).await;
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            content: Some(content(
                json!({"role": "model", "parts": [{"text": "after"}]})
            )),
            ..Default::default()
        }
    );
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_recv_buffers_transcriptions_until_turn_complete(#[case] backend: Backend) {
    let server = WsServer::start(vec![
        json!({"serverContent": {"inputTranscription": {"text": "hel"}}}),
        json!({"serverContent": {"inputTranscription": {"text": "lo", "finished": true}}}),
        json!({"serverContent": {"outputTranscription": {"text": "hi", "finished": true}}}),
        json!({"serverContent": {"turnComplete": true}}),
        json!({"serverContent": {"turnComplete": true}}),
    ])
    .await;
    let mut conn = connect(&server, backend).await;
    let t = |text: &str, finished: bool| -> gadk_genai::Transcription {
        serde_json::from_value(json!({"text": text, "finished": finished})).unwrap()
    };
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            input_transcription: Some(t("hel", false)),
            partial: true,
            ..Default::default()
        }
    );
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            input_transcription: Some(t("lo", true)),
            partial: true,
            ..Default::default()
        }
    );
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            output_transcription: Some(t("hi", true)),
            partial: true,
            ..Default::default()
        }
    );
    // turnComplete: the accumulated transcriptions first, then the turnComplete response.
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            input_transcription: Some(t("hello", true)),
            ..Default::default()
        }
    );
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            output_transcription: Some(t("hi", true)),
            ..Default::default()
        }
    );
    let done = LlmResponse {
        turn_complete: true,
        ..Default::default()
    };
    assert_eq!(recv(&mut conn).await, done);
    // The buffers were reset: the next turnComplete carries no transcriptions.
    assert_eq!(recv(&mut conn).await, done);
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_session_resumption_handle(#[case] backend: Backend) {
    let server = WsServer::start(vec![
        json!({"sessionResumptionUpdate": {"newHandle": "h1", "resumable": true}}),
    ])
    .await;
    let cfg: LiveConnectConfig =
        serde_json::from_value(json!({"sessionResumption": {"handle": "h0"}})).unwrap();
    let mut conn = connect_with(&server, backend, MODEL, cfg).await;
    let frames = server.frames(1).await;
    assert_eq!(
        frames[0],
        json!({"setup": {"model": full_model(backend, MODEL), "sessionResumption": {"handle": "h0"}}})
    );
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert_eq!(
        recv(&mut conn).await,
        LlmResponse {
            session_resumption_handle: "h1".into(),
            ..Default::default()
        }
    );
}

/// Ephemeral `auth_tokens/` keys are rejected before connecting (on Vertex AI any API key
/// is).
#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_auth_tokens_rejected(#[case] backend: Backend) {
    let mut cfg = config(backend, "ws://127.0.0.1:9/".into());
    cfg.api_key = Some("auth_tokens/x".into());
    cfg.credentials = None;
    let err = GeminiModel::new_concrete(MODEL, cfg).await.unwrap_err();
    assert!(matches!(err, GeminiError::Config(_)), "{err:?}");
}

#[rstest]
#[case::gemini_api(Backend::GeminiApi)]
#[case::vertex_ai(Backend::VertexAi)]
#[tokio::test]
async fn live_close_sends_close_frame(#[case] backend: Backend) {
    let server = WsServer::start(vec![]).await;
    let mut conn = connect(&server, backend).await;
    conn.close().await.unwrap();
    assert!(server.wait_closed().await);
    // The server answers the close; the buffered setupComplete is still delivered, then
    // the connection reports closed.
    assert_eq!(recv(&mut conn).await, LlmResponse::default());
    assert!(conn.recv().await.unwrap().is_none());
}
