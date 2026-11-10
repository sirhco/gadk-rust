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

//! Vertex AI request URLs and authentication, pinned to genai v1.75.0: `tModel`
//! (`transformer.go`), `createAPIURL` (`api_client.go`) and the endpoint ladder in
//! `NewClient` (`client.go`).

mod common;

use std::sync::Arc;

use common::Recorder;
use common::fake::Scripted;
use futures::StreamExt;
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::Content;
use gadk_model::rest::test_util::fixed_header_credentials;
use gadk_model::{Backend, HttpClient, LlmRequest, LlmResponse, ModelError};
use google_cloud_auth::credentials::Credentials;
use http::{HeaderMap, HeaderValue};
use pretty_assertions::assert_eq;
use serde_json::json;

const OK_BODY: &str = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#;

/// A Vertex AI config on `location` with anonymous credentials, set on the config itself.
fn vertex(location: &str, http: Arc<dyn HttpClient>) -> ClientConfig {
    vertex_with(location, http, anonymous())
}

fn vertex_with(
    location: &str,
    http: Arc<dyn HttpClient>,
    credentials: Credentials,
) -> ClientConfig {
    ClientConfig {
        backend: Some(Backend::VertexAi),
        project: Some("p".into()),
        location: Some(location.into()),
        credentials: Some(credentials),
        http_client: Some(http),
        env: Arc::new(MapEnv(Default::default())),
        ..Default::default()
    }
}

fn anonymous() -> Credentials {
    google_cloud_auth::credentials::anonymous::Builder::new().build()
}

fn user_request() -> LlmRequest {
    LlmRequest {
        contents: vec![
            serde_json::from_value::<Content>(json!({"role": "user", "parts": [{"text": "hi"}]}))
                .unwrap(),
        ],
        ..Default::default()
    }
}

/// A scripted server answering `n` requests with `body`, behind a [`Recorder`].
fn server(n: usize, content_type: &'static str, body: &str) -> Arc<Recorder> {
    let script = (0..n)
        .map(|_| (200, content_type, body.to_string()))
        .collect();
    Recorder::new(Arc::new(Scripted::new(script)))
}

async fn run(model: &str, cfg: ClientConfig, stream: bool) -> Vec<Result<LlmResponse, ModelError>> {
    let llm = GeminiModel::new(model, cfg).await.expect("NewModel");
    llm.generate_content(user_request(), stream).collect().await
}

fn uris(rec: &Recorder) -> Vec<String> {
    rec.seen
        .lock()
        .iter()
        .map(|r| r.uri().to_string())
        .collect()
}

/// genai `tModel` (Vertex branch) followed by the `projects/{p}/locations/{l}/` prefix
/// `createAPIURL` adds to every path not already under `projects/`.
#[tokio::test]
async fn vertex_model_path_variants() {
    let base = "https://us-central1-aiplatform.googleapis.com/v1beta1";
    let full = "projects/p/locations/us-central1";
    let cases: &[(&str, String)] = &[
        // Bare name: publishers/google/models/{model}.
        (
            "gemini-2.5-flash",
            format!("{base}/{full}/publishers/google/models/gemini-2.5-flash"),
        ),
        // publishers/ prefix: kept, then prefixed.
        (
            "publishers/google/models/gemini-2.5-flash",
            format!("{base}/{full}/publishers/google/models/gemini-2.5-flash"),
        ),
        // projects/ prefix: kept as-is, not prefixed (other project and location too).
        (
            "projects/p/locations/l/publishers/google/models/x",
            format!("{base}/projects/p/locations/l/publishers/google/models/x"),
        ),
        (
            "projects/q/locations/m/endpoints/123",
            format!("{base}/projects/q/locations/m/endpoints/123"),
        ),
        // models/ prefix: kept, then prefixed (tModel, not tModelFullName).
        ("models/abc", format!("{base}/{full}/models/abc")),
        // Any other name with '/': split once into publisher and model.
        (
            "tunedModels/abc",
            format!("{base}/{full}/publishers/tunedModels/models/abc"),
        ),
        (
            "anthropic/claude-x",
            format!("{base}/{full}/publishers/anthropic/models/claude-x"),
        ),
        ("a/b/c", format!("{base}/{full}/publishers/a/models/b/c")),
    ];
    for (model, want) in cases {
        let rec = server(1, "application/json", OK_BODY);
        let got = run(model, vertex("us-central1", rec.clone()), false).await;
        assert!(got[0].is_ok(), "{model}: {:?}", got[0]);
        assert_eq!(
            uris(&rec),
            vec![format!("{want}:generateContent")],
            "model {model}"
        );
    }

    // tModel rejections: empty, '?', '&', '..'; no request is sent.
    for model in ["", "a?b", "a&b", "../x", "publishers/google/models/x..y"] {
        let rec = server(0, "application/json", OK_BODY);
        let got = run(model, vertex("us-central1", rec.clone()), false).await;
        assert!(
            matches!(got.as_slice(), [Err(ModelError::Config(_))]),
            "{model:?}: {got:?}"
        );
        assert!(uris(&rec).is_empty(), "{model:?}");
    }
}

#[tokio::test]
async fn vertex_credentials_headers_sent() {
    // Anonymous credentials: no API key header and no authorization header.
    let rec = server(1, "application/json", OK_BODY);
    let got = run(
        "gemini-2.5-flash",
        vertex("us-central1", rec.clone()),
        false,
    )
    .await;
    assert!(got[0].is_ok(), "{:?}", got[0]);
    {
        let seen = rec.seen.lock();
        let h = seen[0].headers();
        assert!(h.get("x-goog-api-key").is_none());
        assert!(h.get(http::header::AUTHORIZATION).is_none());
        assert!(h.get("x-goog-user-project").is_none());
    }

    // A custom provider's headers are sent as-is, every value of a repeated header too.
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer t"),
    );
    headers.insert("x-goog-user-project", HeaderValue::from_static("q"));
    headers.append("x-goog-test", HeaderValue::from_static("one"));
    headers.append("x-goog-test", HeaderValue::from_static("two"));
    let rec = server(2, "application/json", OK_BODY);
    let creds = fixed_header_credentials(headers);
    let cfg = vertex_with("us-central1", rec.clone(), creds.clone());
    let got = run("gemini-2.5-flash", cfg, false).await;
    assert!(got[0].is_ok(), "{:?}", got[0]);
    let cfg = vertex_with("us-central1", rec.clone(), creds);
    let got = run("gemini-2.5-flash", cfg, true).await;
    assert!(got.iter().all(Result::is_ok), "{got:?}");
    let seen = rec.seen.lock();
    assert_eq!(seen.len(), 2);
    for req in seen.iter() {
        let h = req.headers();
        assert!(h.get("x-goog-api-key").is_none());
        assert_eq!(
            h.get_all(http::header::AUTHORIZATION)
                .iter()
                .collect::<Vec<_>>(),
            vec!["Bearer t"]
        );
        assert_eq!(
            h.get_all("x-goog-user-project").iter().collect::<Vec<_>>(),
            vec!["q"]
        );
        assert_eq!(
            h.get_all("x-goog-test").iter().collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }
}

/// The endpoint ladder in genai `NewClient`: `global` → `aiplatform.googleapis.com`, the
/// multi-regions `us`/`eu` → `aiplatform.{l}.rep.googleapis.com`, else `{l}-aiplatform`.
#[tokio::test]
async fn vertex_global_and_rep_urls() {
    let model = "publishers/google/models/gemini-2.5-flash:generateContent";
    let cases = [
        (
            "global",
            format!(
                "https://aiplatform.googleapis.com/v1beta1/projects/p/locations/global/{model}"
            ),
        ),
        (
            "us",
            format!(
                "https://aiplatform.us.rep.googleapis.com/v1beta1/projects/p/locations/us/{model}"
            ),
        ),
        (
            "eu",
            format!(
                "https://aiplatform.eu.rep.googleapis.com/v1beta1/projects/p/locations/eu/{model}"
            ),
        ),
        (
            "europe-west4",
            format!(
                "https://europe-west4-aiplatform.googleapis.com/v1beta1/projects/p/locations/europe-west4/{model}"
            ),
        ),
    ];
    for (location, want) in cases {
        let rec = server(1, "application/json", OK_BODY);
        let got = run("gemini-2.5-flash", vertex(location, rec.clone()), false).await;
        assert!(got[0].is_ok(), "{location}: {:?}", got[0]);
        assert_eq!(uris(&rec), vec![want], "location {location}");
    }
}

#[tokio::test]
async fn vertex_stream_uses_sse() {
    let rec = server(1, "text/event-stream", &format!("data: {OK_BODY}\n\n"));
    let got = run("gemini-2.5-flash", vertex("global", rec.clone()), true).await;
    assert!(!got.is_empty() && got.iter().all(Result::is_ok), "{got:?}");
    assert_eq!(
        uris(&rec),
        vec![
            "https://aiplatform.googleapis.com/v1beta1/projects/p/locations/global/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        ]
    );
}
