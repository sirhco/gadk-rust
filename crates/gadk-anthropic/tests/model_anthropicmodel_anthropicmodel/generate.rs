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

//! Ports of TestGenerate_Anthropic, TestGenerate_Vertex: the two Go recordings
//! replayed through `gadk_httprr` with Go's scrubbers. Each test also records the raw request
//! bodies the model sends and checks them byte for byte against the recorded ones (the httprr
//! lookup alone compares scrubbed, compacted bodies).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use gadk_anthropic::{Backend, ClientConfig, ProcessEnv};
use gadk_genai::{Content, FinishReason, GenerateContentConfig};
use gadk_httprr::RecordReplay;
use gadk_model::{BodyStream, HttpClient, LlmRequest, LlmResponse, ModelError};

use crate::harness::{
    empty_env, generate_ok, new_model, open_httprr, static_token_credentials, testdata_path,
};

/// Forwards to the httprr client and keeps every request's URL and raw body.
struct Spy {
    inner: Arc<RecordReplay>,
    seen: Mutex<Vec<(String, Bytes)>>,
}

impl Spy {
    fn new(inner: Arc<RecordReplay>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<(String, Bytes)> {
        self.seen.lock().unwrap().clone()
    }
}

// `HttpClient` is an `async_trait` trait; gadk-anthropic has no `async-trait` dependency, so
// this is the macro's expansion written out.
impl HttpClient for Spy {
    fn send<'life0, 'async_trait>(
        &'life0 self,
        req: http::Request<Bytes>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<http::Response<BodyStream>, ModelError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        self.seen
            .lock()
            .unwrap()
            .push((req.uri().to_string(), req.body().clone()));
        self.inner.send(req)
    }
}

/// The request URLs and bodies of an httprr trace, in order (`httprr trace v1`, then entries
/// `<req len> <resp len>\n<req><resp>`; a request's body follows its blank line).
fn recorded_requests(file: &str) -> Vec<(String, Vec<u8>)> {
    let data = std::fs::read(testdata_path(file)).unwrap();
    let mut rest = data
        .strip_prefix(b"httprr trace v1\n".as_slice())
        .expect("httprr header");
    let mut out = Vec::new();
    while !rest.is_empty() {
        let nl = rest.iter().position(|&b| b == b'\n').unwrap();
        let sizes = std::str::from_utf8(&rest[..nl]).unwrap();
        let (req_len, resp_len) = sizes.split_once(' ').unwrap();
        let (req_len, resp_len): (usize, usize) =
            (req_len.parse().unwrap(), resp_len.parse().unwrap());
        let req = &rest[nl + 1..nl + 1 + req_len];
        rest = &rest[nl + 1 + req_len + resp_len..];
        let request_line = req.split(|&b| b == b'\r').next().unwrap();
        let url = std::str::from_utf8(request_line)
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .to_string();
        let body_at = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        out.push((url, req[body_at..].to_vec()));
    }
    out
}

/// Asserts the spy saw exactly the recorded requests: same URLs, byte-identical bodies.
fn assert_recorded_requests(spy: &Spy, file: &str) {
    let want = recorded_requests(file);
    let got = spy.seen();
    assert_eq!(got.len(), want.len(), "request count");
    for ((got_url, got_body), (want_url, want_body)) in got.iter().zip(&want) {
        assert_eq!(got_url, want_url);
        assert_eq!(
            String::from_utf8_lossy(got_body),
            String::from_utf8_lossy(want_body)
        );
    }
}

/// Go `new(float32)`: `Some(0.0)`.
fn zero_temperature() -> Option<f32> {
    Some(0.0)
}

/// Go TestGenerate_Anthropic's request.
fn anthropic_request() -> LlmRequest {
    LlmRequest {
        contents: gadk_genai::text("What is the capital of France? Reply with one word."),
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content::from_text(
                "Reply with the city name only.",
                gadk_genai::ROLE_USER,
            )),
            temperature: zero_temperature(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_generate_anthropic() {
    let file = "TestGenerate_Anthropic.httprr";
    let rr = open_httprr(file);
    let recording = rr.is_recording();

    let process_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let api_key = if !recording || process_key.is_empty() {
        "test-anthropic-key".to_string()
    } else {
        process_key
    };
    let spy = Spy::new(rr.clone());
    let m = new_model(
        "claude-sonnet-4-5-20250929",
        ClientConfig {
            backend: Backend::Anthropic,
            api_key: Some(api_key),
            http_client: Some(spy.clone()),
            // Go `t.Setenv("ANTHROPIC_BASE_URL", "")` when replaying.
            env: if recording {
                Arc::new(ProcessEnv)
            } else {
                empty_env()
            },
            ..Default::default()
        },
    )
    .await;

    // non_streaming
    {
        let responses = generate_ok(m.as_ref(), anthropic_request(), false).await;
        assert_eq!(
            responses.len(),
            1,
            "got {} responses, want 1",
            responses.len()
        );
        let got = &responses[0];
        assert!(
            !got.partial && got.turn_complete,
            "got Partial={} TurnComplete={}, want Partial=false TurnComplete=true",
            got.partial,
            got.turn_complete
        );
        assert_eq!(got.finish_reason, Some(FinishReason::Stop));
        let content = got.content.as_ref().expect("Content");
        assert_eq!(content.parts.len(), 1, "unexpected Content = {content:?}");
        assert!(
            content.parts[0].text.contains("Paris"),
            "unexpected Content = {content:?}"
        );
        let usage = got.usage_metadata.as_ref().expect("UsageMetadata");
        assert!(
            usage.prompt_token_count > 0 && usage.candidates_token_count > 0,
            "unexpected UsageMetadata = {usage:?}"
        );
    }

    // streaming
    {
        let responses = generate_ok(m.as_ref(), anthropic_request(), true).await;
        let mut partial_texts: Vec<String> = Vec::new();
        let mut final_resp: Option<&LlmResponse> = None;
        for resp in &responses {
            if resp.partial {
                assert!(
                    !resp.turn_complete,
                    "partial response had TurnComplete=true"
                );
                for p in &resp.content.as_ref().expect("partial Content").parts {
                    partial_texts.push(p.text.clone());
                }
            } else {
                assert!(final_resp.is_none(), "received multiple final responses");
                final_resp = Some(resp);
            }
        }
        assert!(
            !partial_texts.is_empty(),
            "expected at least one partial streaming chunk"
        );
        let final_resp = final_resp.expect("expected a final aggregated response");
        assert!(
            final_resp.turn_complete && !final_resp.partial,
            "finalResp Partial={} TurnComplete={}, want false/true",
            final_resp.partial,
            final_resp.turn_complete
        );
        let joined = partial_texts.concat();
        let content = final_resp.content.as_ref().expect("final Content");
        assert_eq!(content.parts.len(), 1);
        assert_eq!(
            content.parts[0].text, joined,
            "final text does not match concatenated partial text"
        );
    }

    if !recording {
        assert_recorded_requests(&spy, file);
    }
}

#[tokio::test]
async fn test_generate_vertex() {
    let file = "TestGenerate_Vertex.httprr";
    let rr = open_httprr(file);
    let recording = rr.is_recording();
    let spy = Spy::new(rr.clone());

    let mut cfg = ClientConfig {
        backend: Backend::Vertex,
        project: Some("test-project".to_string()),
        location: Some("us-east5".to_string()),
        http_client: Some(spy.clone()),
        ..Default::default()
    };
    if !recording {
        // Go `t.Setenv("ANTHROPIC_BASE_URL", "")` and `staticTokenSource`.
        cfg.env = empty_env();
        cfg.credentials = Some(static_token_credentials("test-vertex-oauth-token"));
    } else {
        if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT")
            && !p.is_empty()
        {
            cfg.project = Some(p);
        }
        if let Ok(l) = std::env::var("GOOGLE_CLOUD_LOCATION")
            && !l.is_empty()
        {
            cfg.location = Some(l);
        }
    }

    let m = new_model("claude-sonnet-4-5-20250929", cfg).await;

    let req = LlmRequest {
        contents: gadk_genai::text("What is 2 + 2? Reply with the digit only."),
        config: Some(GenerateContentConfig {
            temperature: zero_temperature(),
            ..Default::default()
        }),
        ..Default::default()
    };

    let got = generate_ok(m.as_ref(), req, false)
        .await
        .pop()
        .expect("unexpected response: None");
    let content = got
        .content
        .as_ref()
        .expect("unexpected response: no Content");
    assert_eq!(content.parts.len(), 1, "unexpected response: {got:?}");
    let text = &content.parts[0].text;
    assert!(
        text.contains('4'),
        "got text {text:?}, want string containing \"4\""
    );
    assert_eq!(got.finish_reason, Some(FinishReason::Stop));

    if !recording {
        assert_recorded_requests(&spy, file);
    }
}
