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

//! Port of ADK-Go `model/openaimodel/openaimodel_test.go`, through the public API
//! only.
//!
//! - Go's `httptest` loopback server is [`FakeServer`] (127.0.0.1, real time); Go's in-memory
//!   `roundTripFunc` client is a [`ScriptedClient`].
//! - Go's `ClientConfig.Options` (`option.WithHeader`) is [`ClientConfig::headers`].
//! - Every `ClientConfig` carries an empty [`MapEnv`]: tests never read the process
//!   environment (Go's `openai.NewClient` would read `OPENAI_*`).
//! - Each Go `t.Run` subtest is a named case; failures name it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_genai::{Content, ROLE_USER};
use gadk_model::rest::test_util::{FakeResponse, FakeServer, ScriptedClient};
use gadk_model::{HttpClient, HttpOptions, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_openai::{
    Api, ClientConfig, ErrorKind, FINISH_MESSAGE_KEY, MapEnv, OpenAiError, OpenAiErrorKind,
    new_model,
};
use http::{HeaderMap, HeaderValue};
use pretty_assertions::assert_eq;

/// Go `completedBodies`: a minimal completed blocking response per API.
fn completed_body(api: Api) -> &'static str {
    match api {
        Api::Responses => concat!(
            r#"{"id":"r","object":"response","status":"completed","#,
            r#""output":[{"type":"message","id":"m","role":"assistant","status":"completed","#,
            r#""content":[{"type":"output_text","text":"hi","annotations":[]}]}]}"#
        ),
        Api::ChatCompletions => concat!(
            r#"{"id":"c","object":"chat.completion","model":"m","#,
            r#""choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}]}"#
        ),
    }
}

/// A `ClientConfig` for `api` with an empty environment.
fn config(api_key: &str, base_url: &str, api: Api) -> ClientConfig {
    ClientConfig {
        api_key: api_key.into(),
        base_url: base_url.into(),
        api,
        env: Arc::new(MapEnv::default()),
        ..ClientConfig::default()
    }
}

/// Go `&model.LLMRequest{Contents: []*genai.Content{genai.NewContentFromText("hi", genai.RoleUser)}}`.
fn hi_request() -> LlmRequest {
    LlmRequest {
        contents: vec![Content::from_text("hi", ROLE_USER)],
        ..LlmRequest::default()
    }
}

/// Go `for _, err := range llm.GenerateContent(ctx, req, stream)`: every item the call yields.
async fn generate(
    llm: &Arc<dyn Llm>,
    req: LlmRequest,
    stream: bool,
) -> Vec<Result<LlmResponse, ModelError>> {
    llm.generate_content(req, stream).collect().await
}

/// Go `errors.As` into the provider error a model error carries.
fn openai_error(err: &ModelError) -> &OpenAiError {
    match err {
        ModelError::Other(inner) => inner
            .downcast_ref::<OpenAiError>()
            .unwrap_or_else(|| panic!("error {err} is not an OpenAiError")),
        other => panic!("error {other} is not ModelError::Other"),
    }
}

/// Go `redact`: describes a header value without printing it.
fn redact(v: &str) -> String {
    if v.is_empty() {
        "empty".into()
    } else {
        format!("<{}-byte value>", v.len())
    }
}

// Go's loopback server answers 500, which openai-go retries twice with backoff; a
// ScriptedClient under paused time records the same path without waiting the backoff out.
// Go's zero-value API "" is `"".parse::<Api>()`.
// parity: adapted: in-memory ScriptedClient under paused time instead of a loopback server
#[tokio::test(start_paused = true)]
async fn test_new_model_selects_api() {
    let cases = [
        (
            "zero value keeps Responses",
            "".parse::<Api>().unwrap(),
            "/v1/responses",
        ),
        ("explicit Responses", Api::Responses, "/v1/responses"),
        (
            "chat completions",
            Api::ChatCompletions,
            "/v1/chat/completions",
        ),
    ];
    for (name, api, want_path) in cases {
        let client = ScriptedClient::always(FakeResponse::new(500));
        let http_client: Arc<dyn HttpClient> = client.clone();
        let llm = new_model(
            "gpt-4o-mini",
            ClientConfig {
                http_client: Some(http_client),
                ..config("test", "http://127.0.0.1:1/v1", api)
            },
        )
        .unwrap_or_else(|e| panic!("{name}: NewModel() err = {e}"));
        generate(&llm, hi_request(), false).await;
        let got = client.last_request().path;
        assert_eq!(got, want_path, "{name}: path");
    }
}

// gadk's `ClientConfig::api` is the `Api` enum, so an unknown value can only come from parsing
// one.
// parity: adapted: Go's `NewModel` unknown-API error is `Api`'s `FromStr` error
#[test]
fn test_new_model_rejects_unknown_api() {
    let err = "grpc".parse::<Api>().unwrap_err();
    assert!(
        err.is(ErrorKind::UnsupportedApi),
        "err = {err}, want {}",
        ErrorKind::UnsupportedApi
    );
    assert!(
        err.to_string().contains("grpc"),
        "err = {err}, want it to name the value"
    );
    assert_eq!(err.to_string(), r#"openai: unsupported API: "grpc""#);
}

#[test]
fn test_new_model_requires_model_name() {
    let err = match new_model("", config("k", "", Api::Responses)) {
        Err(e) => e,
        Ok(_) => panic!("NewModel(\"\") succeeded, want an error"),
    };
    assert!(
        err.is(ErrorKind::ModelNameRequired),
        "err = {err}, want {}",
        ErrorKind::ModelNameRequired
    );
}

// gadk has one `ErrorKind` (the crate re-exports it, also as `OpenAiErrorKind`, from the
// module the endpoint code builds errors with), so aliasing holds by construction.
// parity: adapted: one ErrorKind enum; the table pins each Go sentinel name to its kind and text
#[tokio::test]
async fn test_sentinels_alias_the_internal_ones() {
    let table: [(&str, OpenAiErrorKind, &str); 26] = [
        (
            "ErrModelNameRequired",
            ErrorKind::ModelNameRequired,
            "openai: model name is required",
        ),
        (
            "ErrUnsupportedAPI",
            ErrorKind::UnsupportedApi,
            "openai: unsupported API",
        ),
        (
            "ErrNoChoices",
            ErrorKind::NoChoices,
            "openai: response included no choices",
        ),
        (
            "ErrRequestNil",
            ErrorKind::RequestNil,
            "openai: request is nil",
        ),
        (
            "ErrNoContents",
            ErrorKind::NoContents,
            "openai: LLM request has no contents to convert",
        ),
        (
            "ErrFunctionCallMissingName",
            ErrorKind::FunctionCallMissingName,
            "openai: function call missing name",
        ),
        (
            "ErrTopKNotSupported",
            ErrorKind::TopKNotSupported,
            "openai: topK is not supported",
        ),
        (
            "ErrStopSequencesNotSupported",
            ErrorKind::StopSequencesNotSupported,
            "openai: stop sequences are not supported",
        ),
        (
            "ErrMultipleCandidatesNotSupported",
            ErrorKind::MultipleCandidatesNotSupported,
            "openai: multiple candidates per request are not supported",
        ),
        (
            "ErrPenaltiesNotSupported",
            ErrorKind::PenaltiesNotSupported,
            "openai: frequency/presence penalties are not supported",
        ),
        (
            "ErrLabelsNotSupported",
            ErrorKind::LabelsNotSupported,
            "openai: request labels are not supported",
        ),
        (
            "ErrSafetySettingsNotSupported",
            ErrorKind::SafetySettingsNotSupported,
            "openai: gemini safety settings are not supported",
        ),
        (
            "ErrUnsupportedMIMEType",
            ErrorKind::UnsupportedMimeType,
            "openai: unsupported mime type",
        ),
        (
            "ErrMediaMIMETypeRequired",
            ErrorKind::MediaMimeTypeRequired,
            "openai: media part is missing a mime type",
        ),
        (
            "ErrMediaDataRequired",
            ErrorKind::MediaDataRequired,
            "openai: inline media part carries no data",
        ),
        (
            "ErrMediaURIRequired",
            ErrorKind::MediaUriRequired,
            "openai: file data part is missing a uri",
        ),
        (
            "ErrMediaOnAssistantTurn",
            ErrorKind::MediaOnAssistantTurn,
            "openai: media on a replayed assistant turn is not supported",
        ),
        (
            "ErrUnsupportedConfigField",
            ErrorKind::UnsupportedConfigField,
            "openai: unsupported generation config field",
        ),
        (
            "ErrEmptyJSONSchema",
            ErrorKind::EmptyJsonSchema,
            "openai: empty json schema",
        ),
        (
            "ErrEmptyResponse",
            ErrorKind::EmptyResponse,
            "openai: empty response",
        ),
        (
            "ErrNoOutputItems",
            ErrorKind::NoOutputItems,
            "openai: response included no output items",
        ),
        (
            "ErrUnsupportedMessageContentType",
            ErrorKind::UnsupportedMessageContentType,
            "openai: unsupported message content type",
        ),
        (
            "ErrUnsupportedOutputItemType",
            ErrorKind::UnsupportedOutputItemType,
            "openai: unsupported output item type",
        ),
        (
            "ErrFunctionCallArgs",
            ErrorKind::FunctionCallArgs,
            "openai: parse function call args",
        ),
        (
            "ErrNoTextOrToolContent",
            ErrorKind::NoTextOrToolContent,
            "openai: response output did not contain text or tool content",
        ),
        (
            "ErrResponseFailed",
            ErrorKind::ResponseFailed,
            "openai: response failed",
        ),
    ];
    for (name, kind, text) in table {
        assert!(
            ErrorKind::ALL.contains(&kind),
            "{name} is not one of the kinds the endpoint code returns"
        );
        assert_eq!(kind.sentinel_text(), text, "{name}");
        assert_eq!(kind.to_string(), text, "{name}");
    }
    // One Go sentinel per kind, none left over.
    let kinds: HashSet<OpenAiErrorKind> = table.iter().map(|(_, k, _)| *k).collect();
    assert_eq!(kinds.len(), ErrorKind::ALL.len());

    // And end to end, through an error an endpoint package actually built.
    let client = ScriptedClient::json(completed_body(Api::ChatCompletions));
    let http_client: Arc<dyn HttpClient> = client.clone();
    let llm = new_model(
        "m",
        ClientConfig {
            http_client: Some(http_client),
            ..config("k", "", Api::ChatCompletions)
        },
    )
    .unwrap_or_else(|e| panic!("NewModel() err = {e}"));
    let got = generate(&llm, LlmRequest::default(), false)
        .await
        .into_iter()
        .filter_map(Result::err)
        .last()
        .expect("GenerateContent yielded no error");
    let got = openai_error(&got);
    assert!(
        got.is(ErrorKind::NoContents),
        "err = {got}, want the aliased {} to match",
        ErrorKind::NoContents
    );
    assert_eq!(client.hits(), 0, "an invalid request reached the wire");
}

// Go compares the façade constant with the unexported `responses.FinishMessageKey`; a test
// outside the crate cannot name that, so it checks the literal and that a blocking Responses
// turn cut short by a content filter reports its wording under `FINISH_MESSAGE_KEY`.
// parity: adapted: checks the key the Responses path writes end to end (internal const unreachable)
#[tokio::test]
async fn test_finish_message_key_matches_the_internal_one() {
    assert_eq!(FINISH_MESSAGE_KEY, "openai_finish_message");

    let client = ScriptedClient::json(concat!(
        r#"{"id":"resp_1","model":"stream-model","status":"incomplete","#,
        r#""incomplete_details":{"reason":"content_filter"},"#,
        r#""output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}"#
    ));
    let http_client: Arc<dyn HttpClient> = client.clone();
    let llm = new_model(
        "gpt-4o-mini",
        ClientConfig {
            http_client: Some(http_client),
            ..config("test", "http://127.0.0.1:1/v1", Api::Responses)
        },
    )
    .unwrap();
    let got = generate(&llm, hi_request(), false).await;
    let resp = match got.as_slice() {
        [Ok(resp)] => resp,
        other => panic!("GenerateContent() = {other:?}, want one response"),
    };
    let written = resp
        .custom_metadata
        .get(FINISH_MESSAGE_KEY)
        .and_then(|v| v.as_str());
    assert_eq!(
        written,
        Some("content_filter"),
        "FinishMessageKey = {FINISH_MESSAGE_KEY:?}, but the Responses path wrote {:?}",
        resp.custom_metadata
    );
}

#[tokio::test]
async fn test_http_options_headers_never_reach_the_wire() {
    for api in [Api::Responses, Api::ChatCompletions] {
        for stream in [false, true] {
            let name = format!("{api}/stream={stream}");
            let server = FakeServer::json(completed_body(api));
            let llm = new_model(
                "gpt-4o-mini",
                config("real-key", &format!("{}/v1", server.url()), api),
            )
            .unwrap_or_else(|e| panic!("{name}: NewModel() err = {e}"));
            // A timeout is set deliberately. Without one the translation returns early, so
            // header forwarding reintroduced after that point would never run here and the
            // test would pass vacuously.
            let mut headers = HeaderMap::new();
            headers.insert("Authorization", HeaderValue::from_static("Bearer caller"));
            headers.insert("X-Goog-Api-Key", HeaderValue::from_static("gemini-key"));
            headers.insert("X-Trace-Id", HeaderValue::from_static("harmless"));
            let req = LlmRequest {
                http_options: Some(HttpOptions {
                    timeout: Some(Duration::from_secs(30)),
                    headers,
                    ..HttpOptions::default()
                }),
                ..hi_request()
            };
            // A streamed call fails on the JSON body; only its headers matter.
            for item in generate(&llm, req, stream).await {
                if let Err(err) = item
                    && !stream
                {
                    panic!("{name}: GenerateContent() err = {err}");
                }
            }
            assert!(server.hits() > 0, "{name}: no request reached the server");
            let got = server.last_request();
            let v = got.header("Authorization");
            assert!(
                v == "Bearer real-key",
                "{name}: Authorization = {}, want the configured key: a caller header displaced it",
                redact(&v)
            );
            let v = got.header("X-Goog-Api-Key");
            assert!(
                v.is_empty(),
                "{name}: X-Goog-Api-Key = {}, want absent: a Gemini credential reached the provider",
                redact(&v)
            );
            let v = got.header("X-Trace-Id");
            assert!(
                v.is_empty(),
                "{name}: X-Trace-Id = {}, want absent: headers are not forwarded",
                redact(&v)
            );
        }
    }
}

// Go's in-memory `roundTripFunc` client is a ScriptedClient.
// parity: adapted: Go's `ClientConfig.Options` (`option.WithHeader`) is `ClientConfig::headers`
#[tokio::test]
async fn test_new_model_uses_the_configured_client() {
    for api in [Api::Responses, Api::ChatCompletions] {
        let name = api.as_str();
        // Not a status the SDK retries.
        let server = FakeServer::always(FakeResponse::new(418));
        let client = ScriptedClient::json(completed_body(api));
        let http_client: Arc<dyn HttpClient> = client.clone();
        let mut headers = HeaderMap::new();
        headers.insert("X-Adk-Option", HeaderValue::from_static("set"));
        let llm = new_model(
            "gpt-4o-mini",
            ClientConfig {
                http_client: Some(http_client),
                headers,
                ..config("test", &format!("{}/v1", server.url()), api)
            },
        )
        .unwrap_or_else(|e| panic!("{name}: NewModel() err = {e}"));
        for item in generate(&llm, hi_request(), false).await {
            if let Err(err) = item {
                panic!("{name}: GenerateContent() err = {err}");
            }
        }
        assert!(
            client.hits() > 0,
            "{name}: the request did not go through ClientConfig.http_client"
        );
        assert_eq!(
            server.hits(),
            0,
            "{name}: the default client reached the server"
        );
        let option_header = client.last_request().header("X-Adk-Option");
        assert_eq!(
            option_header, "set",
            "{name}: X-Adk-Option = {option_header:?}, want the header ClientConfig.headers set"
        );
    }
}
