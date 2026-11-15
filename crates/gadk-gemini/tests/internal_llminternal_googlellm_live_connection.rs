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

//! Port of ADK-Go `internal/llminternal/googlellm/live_connection_test.go` (gadk's
//! `googlellm.LiveConnection` is `gadk_gemini::LiveConnection`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::ws::WsServer;
use gadk_gemini::{ClientConfig, GeminiModel, LiveConnection, MapEnv, RealtimeInput};
use gadk_genai::{
    Blob, Content, FileData, GenerateContentResponseUsageMetadata, LiveConnectConfig,
    MediaModality, ModalityTokenCount, Part, ROLE_MODEL, ROLE_USER, TrafficType, Transcription,
};
use gadk_model::{Backend, HttpOptions, LlmResponse};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// A Gemini API or (anonymous-credential) Vertex AI client config pointing at `base_url`.
fn config(backend: Backend, base_url: String) -> ClientConfig {
    let http_options = HttpOptions {
        base_url: Some(base_url),
        ..Default::default()
    };
    let env = Arc::new(MapEnv(Default::default()));
    match backend {
        Backend::GeminiApi => ClientConfig {
            backend: Some(Backend::GeminiApi),
            api_key: Some("test-api-key".into()),
            http_options,
            env,
            ..Default::default()
        },
        Backend::VertexAi => ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some("p".into()),
            location: Some("l".into()),
            credentials: Some(google_cloud_auth::credentials::anonymous::Builder::new().build()),
            http_options,
            env,
            ..Default::default()
        },
    }
}

/// Go connectFakeLive: a LiveConnection for `model` on a fake Live API server; the server's
/// second frame is the first one the client sends after the setup exchange.
async fn connect_fake_live(backend: Backend, model: &str) -> (WsServer, LiveConnection) {
    let server = WsServer::start(vec![]).await;
    let m = GeminiModel::new_concrete(model, config(backend, server.base_url()))
        .await
        .unwrap();
    let conn = m.live_connect(LiveConnectConfig::default()).await.unwrap();
    (server, conn)
}

/// The first frame the server reads after the setup exchange.
async fn input_frame(server: &WsServer) -> Value {
    server.frames(2).await[1].clone()
}

fn blob(data: &[u8], mime: &str) -> Blob {
    Blob {
        data: Bytes::copy_from_slice(data),
        mime_type: mime.into(),
        ..Default::default()
    }
}

enum Input {
    Realtime(Blob),
    Content(Content),
}

#[tokio::test]
#[ignore = "8a bug: LiveConnection routes realtime input and single-text content only for \"gemini-3.1\" models on the Gemini API; Go d8e47448 routes every Gemini 3.x Live model (not Live Translate, on both backends) to audio/video/text, and Live Translate audio to audio"]
async fn test_live_connection_routes_input_by_model() {
    let audio = || blob(&[1], "audio/pcm;rate=16000");
    let image = || blob(&[2], "image/jpeg");
    let text = || Content::from_text("hi", ROLE_USER);
    let two_texts =
        || Content::from_parts(vec![Part::from_text("a"), Part::from_text("b")], ROLE_USER);
    let inline_image = || {
        Content::from_parts(
            vec![Part::from_bytes(Bytes::from_static(&[2]), "image/jpeg")],
            ROLE_USER,
        )
    };
    let (gemini, vertex) = (Backend::GeminiApi, Backend::VertexAi);
    let audio_frame =
        json!({"realtimeInput": {"audio": {"data": "AQ==", "mimeType": "audio/pcm;rate=16000"}}});
    let text_turn = json!({"clientContent": {"turns": [{"parts": [{"text": "hi"}], "role": "user"}], "turnComplete": true}});
    // (name, backend, model, input, want)
    let cases: Vec<(&str, Backend, &str, Input, Value)> = vec![
        (
            "3.x audio",
            gemini,
            "gemini-3.8-live",
            Input::Realtime(audio()),
            audio_frame.clone(),
        ),
        (
            "3.x image",
            gemini,
            "gemini-3.8-live",
            Input::Realtime(image()),
            json!({"realtimeInput": {"video": {"data": "Ag==", "mimeType": "image/jpeg"}}}),
        ),
        (
            "3.x text",
            gemini,
            "gemini-3.8-live",
            Input::Content(text()),
            json!({"realtimeInput": {"text": "hi"}}),
        ),
        (
            "live translate audio",
            gemini,
            "gemini-3.5-live-translate-preview",
            Input::Realtime(audio()),
            audio_frame.clone(),
        ),
        (
            "live translate text",
            gemini,
            "gemini-3.5-live-translate-preview",
            Input::Content(text()),
            text_turn.clone(),
        ),
        (
            "2.5 audio",
            gemini,
            "gemini-2.5-flash-native-audio-latest",
            Input::Realtime(audio()),
            json!({"realtimeInput": {"mediaChunks": [{"data": "AQ==", "mimeType": "audio/pcm;rate=16000"}]}}),
        ),
        (
            "2.5 image",
            gemini,
            "gemini-2.5-flash-native-audio-latest",
            Input::Realtime(image()),
            json!({"realtimeInput": {"mediaChunks": [{"data": "Ag==", "mimeType": "image/jpeg"}]}}),
        ),
        (
            "2.5 text",
            gemini,
            "gemini-2.5-flash-native-audio-latest",
            Input::Content(text()),
            text_turn.clone(),
        ),
        (
            "3.x two text parts",
            gemini,
            "gemini-3.8-live",
            Input::Content(two_texts()),
            json!({"clientContent": {"turns": [{"parts": [{"text": "a"}, {"text": "b"}], "role": "user"}], "turnComplete": true}}),
        ),
        (
            "3.x inline image part",
            gemini,
            "gemini-3.8-live",
            Input::Content(inline_image()),
            json!({"clientContent": {"turns": [{"parts": [{"inlineData": {"data": "Ag==", "mimeType": "image/jpeg"}}], "role": "user"}], "turnComplete": true}}),
        ),
        (
            "3.1 audio",
            gemini,
            "gemini-3.1-flash-live-preview",
            Input::Realtime(audio()),
            audio_frame.clone(),
        ),
        (
            "3.1 text",
            gemini,
            "gemini-3.1-flash-live-preview",
            Input::Content(text()),
            json!({"realtimeInput": {"text": "hi"}}),
        ),
        (
            "3.1 audio on Vertex AI",
            vertex,
            "gemini-3.1-flash-live-preview",
            Input::Realtime(audio()),
            audio_frame.clone(),
        ),
        (
            "3.1 text on Vertex AI",
            vertex,
            "gemini-3.1-flash-live-preview",
            Input::Content(text()),
            json!({"realtimeInput": {"text": "hi"}}),
        ),
    ];
    for (name, backend, model, input, want) in cases {
        let (server, conn) = connect_fake_live(backend, model).await;
        let sent = match input {
            Input::Content(c) => conn.send_content(c).await,
            Input::Realtime(b) => conn.send_realtime(RealtimeInput::Blob(b)).await,
        };
        sent.unwrap_or_else(|e| panic!("{name}: send failed: {e}"));
        assert_eq!(want, input_frame(&server).await, "{name}: sent frame");
    }
}

/// Gemini 3.x Live takes realtime blobs on the audio and video fields, which carry audio/*
/// and image/* only, so a blob of another type is an error rather than a send on the media
/// field.
#[tokio::test]
#[ignore = "8a bug: LiveConnection::send_realtime sends a non-audio, non-image blob to a Gemini 3.x Live model as mediaChunks; Go d8e47448 sends it on the audio field, which rejects it"]
async fn test_live_connection_rejects_other_mime_types_on3x_live() {
    let (server, conn) = connect_fake_live(Backend::GeminiApi, "gemini-3.8-live").await;
    assert!(
        conn.send_realtime(RealtimeInput::Blob(blob(&[3], "video/mp4")))
            .await
            .is_err(),
        "SendRealtime() of a video/mp4 blob succeeded, want an error"
    );
    // The audio blob is the first frame the server reads only if the rejected blob sent
    // nothing and left the connection usable.
    conn.send_realtime(RealtimeInput::Blob(blob(&[1], "audio/pcm;rate=16000")))
        .await
        .expect("SendRealtime() of an audio blob after the rejected one failed");
    assert_eq!(
        json!({"realtimeInput": {"audio": {"data": "AQ==", "mimeType": "audio/pcm;rate=16000"}}}),
        input_frame(&server).await,
        "first frame after the rejected blob"
    );
}

fn file_part(uri: &str, mime_type: &str) -> Part {
    Part {
        file_data: Some(FileData {
            file_uri: uri.into(),
            mime_type: mime_type.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// SendHistory strips the parts the live API must not be primed with (internal _adk_live
/// artifact references, written by the audio cache manager, and cached audio blobs) while
/// leaving text and ordinary file references untouched.
#[tokio::test]
#[ignore = "8a bug: LiveConnection::send_history replays internal artifact://…/_adk_live/… references (Go 22ff1839 drops them)"]
async fn test_live_connection_send_history_filters_internal_parts() {
    let report = || {
        file_part(
            "artifact://app/user/session/report.pdf#1",
            "application/pdf",
        )
    };
    let live_ref = || {
        file_part(
            "artifact://app/user/session/_adk_live/audio_1.pcm#1",
            "audio/pcm",
        )
    };
    // (name, history, want turns, want turnComplete)
    let cases: Vec<(&str, Vec<Content>, Vec<Content>, bool)> = vec![
        (
            "DropsInternalArtifactRefsAndKeepsRest",
            vec![
                Content::from_parts(
                    vec![
                        Part::from_text("what is in this file?"),
                        report(),
                        live_ref(),
                    ],
                    ROLE_USER,
                ),
                Content::from_text("a quarterly report", ROLE_MODEL),
            ],
            vec![
                Content::from_parts(
                    vec![Part::from_text("what is in this file?"), report()],
                    ROLE_USER,
                ),
                Content::from_text("a quarterly report", ROLE_MODEL),
            ],
            false,
        ),
        (
            "DropsContentLeftEmptyByFiltering",
            vec![
                Content::from_parts(vec![live_ref()], ROLE_USER),
                Content::from_text("hello", ROLE_USER),
            ],
            vec![Content::from_text("hello", ROLE_USER)],
            true,
        ),
        (
            // turnComplete has to follow the last surviving turn, not the last turn of the
            // history as it arrived: here the history ends in a model turn that is filtered
            // away, leaving a user turn.
            "TurnCompleteFollowsLastSurvivingTurn",
            vec![
                Content::from_text("hello", ROLE_USER),
                Content::from_parts(
                    vec![file_part(
                        "artifact://app/user/session/_adk_live/adk_live_audio_storage_output_audio_1.pcm#0",
                        "audio/pcm",
                    )],
                    ROLE_MODEL,
                ),
            ],
            vec![Content::from_text("hello", ROLE_USER)],
            true,
        ),
        (
            "DropsInlineAudioButKeepsNonLiveArtifactAudio",
            vec![Content::from_parts(
                vec![
                    Part::from_bytes(Bytes::from_static(&[0x01, 0x02]), "audio/pcm"),
                    file_part("artifact://app/user/session/uploaded.wav#1", "audio/wav"),
                    Part::from_text("transcribe this"),
                ],
                ROLE_USER,
            )],
            vec![Content::from_parts(
                vec![
                    file_part("artifact://app/user/session/uploaded.wav#1", "audio/wav"),
                    Part::from_text("transcribe this"),
                ],
                ROLE_USER,
            )],
            true,
        ),
    ];
    for (name, history, want, want_turn_complete) in cases {
        let (server, conn) =
            connect_fake_live(Backend::GeminiApi, "gemini-2.0-flash-live-001").await;
        conn.send_history(&history)
            .await
            .unwrap_or_else(|e| panic!("{name}: SendHistory failed: {e}"));
        let frame = input_frame(&server).await;
        let client_content = frame.get("clientContent").unwrap_or_else(|| {
            panic!("{name}: SendHistory sent {frame}, want a clientContent message")
        });
        let turns: Vec<Content> = serde_json::from_value(client_content["turns"].clone()).unwrap();
        assert_eq!(want, turns, "{name}: SendHistory turns");
        assert_eq!(
            client_content
                .get("turnComplete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            want_turn_complete,
            "{name}: SendHistory turnComplete"
        );
    }
}

/// A history made up entirely of internal parts sends nothing at all, rather than an empty
/// turn.
// parity: adapted: Go's history also holds a nil content and a nil part, which Rust cannot
// express; the rest of the history is kept.
#[tokio::test]
#[ignore = "8a bug: LiveConnection::send_history sends a history made only of internal _adk_live references (Go 22ff1839 sends nothing)"]
async fn test_live_connection_send_history_all_filtered() {
    let (server, conn) = connect_fake_live(Backend::GeminiApi, "gemini-2.0-flash-live-001").await;
    let history = vec![Content::from_parts(
        vec![
            file_part(
                "artifact://app/user/session/_adk_live/audio_1.pcm#1",
                "audio/pcm",
            ),
            Part::from_bytes(Bytes::from_static(&[0x01]), "audio/pcm"),
        ],
        ROLE_USER,
    )];
    conn.send_history(&history)
        .await
        .expect("SendHistory failed");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let frames = server.seen.lock().frames.clone();
    assert!(
        frames.len() <= 1,
        "SendHistory sent {:?}, want no frame",
        frames.get(1)
    );
}

/// Go connectFakeLiveServing: a LiveConnection on a fake server that sends `messages`, in
/// order, right after the setup exchange; the setupComplete response is already received.
async fn connect_fake_live_serving(messages: Vec<Value>) -> (WsServer, LiveConnection) {
    let server = WsServer::start(messages).await;
    let m = GeminiModel::new_concrete(
        "gemini-live-2.5-flash-preview",
        config(Backend::GeminiApi, server.base_url()),
    )
    .await
    .unwrap();
    let conn = m.live_connect(LiveConnectConfig::default()).await.unwrap();
    // Connect does not consume setupComplete, so Recv returns it first as an empty response.
    conn.recv()
        .await
        .expect("Recv of setupComplete failed")
        .expect("connection closed");
    (server, conn)
}

fn live_usage_json() -> Value {
    json!({
        "promptTokenCount": 174, "cachedContentTokenCount": 20, "responseTokenCount": 146,
        "toolUsePromptTokenCount": 7, "thoughtsTokenCount": 31, "totalTokenCount": 358,
        "promptTokensDetails": [{"modality": "AUDIO", "tokenCount": 174}],
        "cacheTokensDetails": [{"modality": "TEXT", "tokenCount": 20}],
        "responseTokensDetails": [{"modality": "AUDIO", "tokenCount": 146}],
        "toolUsePromptTokensDetails": [{"modality": "TEXT", "tokenCount": 7}],
        "trafficType": "ON_DEMAND",
    })
}

/// Go wantLiveUsage: live_usage_json as GenerateContent usage, the Live API's response*
/// fields as candidates*.
fn want_live_usage() -> GenerateContentResponseUsageMetadata {
    let count = |modality, token_count| ModalityTokenCount {
        modality: Some(modality),
        token_count,
    };
    GenerateContentResponseUsageMetadata {
        prompt_token_count: 174,
        cached_content_token_count: 20,
        candidates_token_count: 146,
        tool_use_prompt_token_count: 7,
        thoughts_token_count: 31,
        total_token_count: 358,
        prompt_tokens_details: vec![count(MediaModality::Audio, 174)],
        cache_tokens_details: vec![count(MediaModality::Text, 20)],
        candidates_tokens_details: vec![count(MediaModality::Audio, 146)],
        tool_use_prompt_tokens_details: vec![count(MediaModality::Text, 7)],
        traffic_type: Some(TrafficType::OnDemand),
    }
}

/// `msg` with the live usage added.
fn with_usage(mut msg: Value) -> Value {
    msg["usageMetadata"] = live_usage_json();
    msg
}

#[tokio::test]
#[ignore = "8a bug: LiveConnection::recv drops a server message's usageMetadata (Go f2dc5d3a maps it to the response, responseTokenCount/Details to candidatesTokenCount/Details)"]
async fn test_live_connection_recv_carries_usage_metadata() {
    let cases: Vec<(&str, Value, LlmResponse)> = vec![
        ("usage only", json!({}), LlmResponse::default()),
        (
            "usage with turn complete",
            json!({"serverContent": {"turnComplete": true}}),
            LlmResponse {
                turn_complete: true,
                ..Default::default()
            },
        ),
        (
            "usage with session resumption update",
            json!({"sessionResumptionUpdate": {"newHandle": "h1", "resumable": true}}),
            LlmResponse {
                session_resumption_handle: "h1".into(),
                ..Default::default()
            },
        ),
    ];
    for (name, msg, want) in cases {
        let (_server, conn) = connect_fake_live_serving(vec![with_usage(msg)]).await;
        let got = conn
            .recv()
            .await
            .unwrap_or_else(|e| panic!("{name}: Recv failed: {e}"))
            .expect("connection closed");
        let want = LlmResponse {
            usage_metadata: Some(want_live_usage()),
            ..want
        };
        assert_eq!(want, got, "{name}: Recv");
    }
}

/// When a turn ends with buffered transcriptions, the usage stays on the turn-complete
/// response, which Recv delivers after the transcriptions.
#[tokio::test]
#[ignore = "8a bug: LiveConnection::recv drops a server message's usageMetadata, also on the buffered turn-complete response (Go f2dc5d3a)"]
async fn test_live_connection_recv_keeps_usage_on_buffered_turn_complete() {
    let (_server, conn) = connect_fake_live_serving(vec![
        json!({"serverContent": {"outputTranscription": {"text": "hello"}}}),
        with_usage(json!({"serverContent": {"turnComplete": true}})),
    ])
    .await;
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(
            conn.recv()
                .await
                .expect("Recv failed")
                .expect("connection closed"),
        );
    }
    let want = vec![
        LlmResponse {
            output_transcription: Some(Transcription {
                text: "hello".into(),
                ..Default::default()
            }),
            partial: true,
            ..Default::default()
        },
        LlmResponse {
            output_transcription: Some(Transcription {
                text: "hello".into(),
                finished: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        LlmResponse {
            turn_complete: true,
            usage_metadata: Some(want_live_usage()),
            ..Default::default()
        },
    ];
    assert_eq!(want, got, "Recv sequence");
}
