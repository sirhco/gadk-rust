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

//! `responses/request_test.go`: the model-backed tests (D8; see `mod.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gadk_genai::GenerateContentConfig;
use gadk_model::{HttpOptions, Llm, LlmRequest};
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use pretty_assertions::assert_eq;

use crate::client::OpenAiClient;
use crate::config::Api;
use crate::parity_tests::support::{
    FakeResponse, TestClientConfig, collect, new_loopback_server, new_test_model, user_request,
};
use crate::responses::model::Model;

/// The body these tests' servers answer with (a completed response saying `hi`).
const HI_BODY: &str = r#"{"id":"r","object":"response","status":"completed","output":[{"type":"message","id":"m","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hi","annotations":[]}]}]}"#;

/// Go `redact`: describes a header value without reproducing it.
fn redact(v: &str) -> String {
    if v.is_empty() {
        return "empty".to_string();
    }
    format!("<{}-byte value>", v.len())
}

/// A one-turn `hi` request whose `http_options.timeout` is `timeout` (Go
/// `Config: &genai.GenerateContentConfig{HTTPOptions: &genai.HTTPOptions{Timeout: &timeout}}`).
fn hi_request(timeout: Duration) -> LlmRequest {
    LlmRequest {
        config: Some(GenerateContentConfig::default()),
        http_options: Some(HttpOptions {
            timeout: Some(timeout),
            ..HttpOptions::default()
        }),
        ..user_request("hi")
    }
}

/// openai-go records any case-insensitive `Authorization` as an override and then skips the
/// configured API key, so forwarding a caller's header replaces the real credential rather
/// than accompanying it.
// parity: adapted: Go drives openai-go's `Responses.New` with a per-request `option.WithHeaderAdd`; gadk has no per-request options, so the caller's header is `ClientConfig::headers` and the call goes through the Responses model (D8).
#[tokio::test]
async fn test_caller_authorization_header_displaces_the_api_key() {
    let server = new_loopback_server(|_| {
        FakeResponse::json(r#"{"id":"r","object":"response","status":"completed","output":[]}"#)
    });
    let model_with = |headers: HeaderMap| -> Arc<dyn Llm> {
        let cfg = TestClientConfig {
            api_key: "real-key".to_string(),
            base_url: server.url(),
            http_client: None,
        };
        let client_cfg = crate::config::ClientConfig {
            headers,
            ..cfg.client_config(Api::Responses)
        };
        Arc::new(Model::new(Arc::new(OpenAiClient::new(&client_cfg)), "m"))
    };

    // Only the header the stub saw matters here, never the decoded response.
    let _ = collect(
        model_with(HeaderMap::new()).as_ref(),
        user_request("hi"),
        false,
    )
    .await;
    let got = server.last_request().header("authorization");
    assert!(
        got == "Bearer real-key",
        "baseline Authorization = {}, want the configured key",
        redact(&got)
    );

    let mut caller = HeaderMap::new();
    caller.insert(AUTHORIZATION, HeaderValue::from_static("Bearer caller"));
    let _ = collect(model_with(caller).as_ref(), user_request("hi"), false).await;
    let got = server.last_request().header("authorization");
    assert!(
        got != "Bearer real-key",
        "the client now keeps the configured key alongside a caller Authorization header; the reason HTTPOptions.Headers is rejected no longer holds, so revisit it"
    );
    assert!(
        got == "Bearer caller",
        "Authorization = {}, want the caller header to have displaced the key",
        redact(&got)
    );
}

/// The timeout has to reach the request, not merely be computed. The server answers slowly
/// but successfully, so a wired timeout fails the call quickly while an unwired one waits and
/// succeeds.
#[tokio::test]
async fn test_http_options_timeout_reaches_the_request() {
    const SERVER_DELAY: Duration = Duration::from_secs(3);
    for (name, stream) in [("blocking", false), ("streaming", true)] {
        let server =
            new_loopback_server(|_| FakeResponse::json(HI_BODY).delay_headers(SERVER_DELAY));
        let timeout = Duration::from_millis(100);
        let m = new_test_model(
            "gpt-4o-mini",
            &TestClientConfig {
                api_key: "test".to_string(),
                base_url: server.url(),
                http_client: None,
            },
        );

        let start = Instant::now();
        let mut got = None;
        let mut items = m.generate_content(hi_request(timeout), stream);
        while let Some(item) = items.next().await {
            if let Err(err) = item {
                got = Some(err);
                break;
            }
        }
        let elapsed = start.elapsed();

        assert!(
            got.is_some(),
            "{name}: call succeeded after {elapsed:?} with a {timeout:?} timeout configured: the timeout never reached the request"
        );
        // Secondary: loose enough not to flake on a loaded machine while still catching a
        // wildly wrong bound.
        assert!(
            elapsed < SERVER_DELAY,
            "{name}: call took {elapsed:?}, i.e. it waited for the server rather than for its {timeout:?} timeout"
        );
    }
}

/// Each call is its own request with its own deadline: the timeout derives from when the call
/// starts, so a second call does not start from the deadline the first one already used.
// parity: adapted: Go ranges one `iter.Seq2` twice; a gadk response stream is consumed once, so the same model is asked twice with the same request, each call computing its deadline when it starts (D7).
#[tokio::test]
async fn test_generate_content_is_re_rangeable_with_a_timeout() {
    for (name, stream) in [("blocking", false), ("streaming", true)] {
        let server = new_loopback_server(|_| FakeResponse::json(HI_BODY));
        let timeout = Duration::from_secs(30);
        let m = new_test_model(
            "gpt-4o-mini",
            &TestClientConfig {
                api_key: "test".to_string(),
                base_url: server.url(),
                http_client: None,
            },
        );
        let req = hi_request(timeout);
        for pass in 1..=2 {
            let mut got = None;
            let mut items = m.generate_content(req.clone(), stream);
            while let Some(item) = items.next().await {
                if let Err(err) = item {
                    got = Some(err);
                    break;
                }
            }
            if let Some(err) = got {
                panic!(
                    "{name}: pass {pass}: error = {err}, want nil: the sequence is not reusable, which happens when the timeout overwrites the captured context"
                );
            }
        }
        assert_eq!(
            server.hits(),
            2,
            "{name}: server saw calls, want 2: the second range did not reach it"
        );
    }
}
