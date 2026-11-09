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

//! The test harness of the openaimodel ports (D6): Rust counterparts of the helpers ADK-Go's
//! `internal/completions/model_test.go` and `internal/responses/model_test.go` share
//! (`newLoopbackServer`, `newTestModel`, `newTestRig`, `serveJSON`, `serveSSE`, `ask`,
//! `collectResponses`, `runStream`, `runBlocking`, `responseText`, `onlyCall`, `allText`,
//! `functionCalls`, `partKinds`), built on [`gadk_model::rest::test_util`].
//!
//! - Go's `httptest` server is [`FakeServer`] (127.0.0.1, real time); a handler returns a
//!   [`FakeResponse`] (Go's blocking `<-r.Context().Done()` is [`FakeResponse::never`]).
//! - Go's `server.Client()` is a custom [`ReqwestClient`], so a test model has no
//!   response-header timeout, as Go's has none.
//! - A test model's [`ClientConfig::env`] is empty: tests never read the process environment
//!   (Go's `openai.NewClient` would read `OPENAI_*`).
//! - Tests that need paused time use [`ScriptedClient`] with [`test_model`] instead of a
//!   server.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use futures::StreamExt;
use gadk_genai::{Content, FunctionCall};
use gadk_model::{HttpClient, Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use gadk_platform::env::MapEnv;

#[allow(unused_imports)] // re-exported for the ports (Tasks 3–9)
pub(crate) use gadk_model::rest::test_util::{
    BodyEnd, BodyStep, FakeResponse, FakeServer, Outcome, RecordedRequest, ScriptedClient,
    WAIT_TIMEOUT, sse_event, sse_lines,
};

use crate::client::OpenAiClient;
use crate::config::{Api, ClientConfig};
use crate::error::OpenAiError;
use crate::{completions, responses};

/// The model name Go's tests use (`openai.ChatModelGPT4oMini`).
pub(crate) const TEST_MODEL: &str = "gpt-4o-mini";

/// Go `testClientConfig`: the `ClientConfig` fields the tests set.
#[derive(Clone, Default)]
pub(crate) struct TestClientConfig {
    /// Go `APIKey`.
    pub(crate) api_key: String,
    /// Go `BaseURL`.
    pub(crate) base_url: String,
    /// Go `HTTPClient` (`server.Client()`); `None` builds gadk's default client.
    pub(crate) http_client: Option<Arc<dyn HttpClient>>,
}

impl TestClientConfig {
    /// What Go's model tests pass for `server`: key `test`, base `server.URL + "/v1"`,
    /// `server.Client()`.
    pub(crate) fn for_server(server: &FakeServer) -> Self {
        Self {
            api_key: "test".into(),
            base_url: format!("{}/v1", server.url()),
            http_client: Some(Arc::new(ReqwestClient::default())),
        }
    }

    /// The same for a [`ScriptedClient`] (base `http://127.0.0.1:1/v1`, never dialled).
    pub(crate) fn for_scripted(client: &Arc<ScriptedClient>) -> Self {
        Self {
            api_key: "test".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            http_client: Some(client.clone()),
        }
    }

    /// The [`ClientConfig`] for `api`, with an empty environment.
    pub(crate) fn client_config(&self, api: Api) -> ClientConfig {
        ClientConfig {
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            api,
            http_client: self.http_client.clone(),
            headers: http::HeaderMap::new(),
            env: Arc::new(MapEnv::default()),
        }
    }
}

/// The [`OpenAiClient`] a test model of `api` would use.
pub(crate) fn test_client(api: Api, cfg: &TestClientConfig) -> Arc<OpenAiClient> {
    Arc::new(OpenAiClient::new(&cfg.client_config(api)))
}

/// A model of `api` named `model_name` on `cfg` (Go's `New(&client, modelName)` after
/// `openai.NewClient(option.WithAPIKey…, option.WithBaseURL…, option.WithHTTPClient…)`).
pub(crate) fn test_model(api: Api, model_name: &str, cfg: &TestClientConfig) -> Arc<dyn Llm> {
    let client = test_client(api, cfg);
    match api {
        Api::Responses => Arc::new(responses::model::Model::new(client, model_name)),
        Api::ChatCompletions => Arc::new(completions::model::Model::new(client, model_name)),
    }
}

/// Go responses `newTestModel(modelName, cfg)`: a Responses model.
pub(crate) fn new_test_model(model_name: &str, cfg: &TestClientConfig) -> Arc<dyn Llm> {
    test_model(Api::Responses, model_name, cfg)
}

/// Go completions `newTestModel(t, server)`: a Chat Completions model named
/// [`TEST_MODEL`] on `server`.
pub(crate) fn new_completions_test_model(server: &FakeServer) -> Arc<dyn Llm> {
    test_model(
        Api::ChatCompletions,
        TEST_MODEL,
        &TestClientConfig::for_server(server),
    )
}

/// Go `newLoopbackServer(t, handler)`: a 127.0.0.1 server (stopped when dropped).
pub(crate) fn new_loopback_server<F>(handler: F) -> FakeServer
where
    F: Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
{
    FakeServer::start(handler)
}

/// Go `serveJSON(body)`: 200, `Content-Type: application/json`, `body`.
pub(crate) fn serve_json(body: &str) -> FakeResponse {
    FakeResponse::json(body)
}

/// Each event as Go's handlers write it: `data: <event>\n\n`.
pub(crate) fn sse_data(events: &[&str]) -> String {
    events.iter().map(|e| format!("data: {e}\n\n")).collect()
}

/// Go completions `serveSSE(frames...)`: 200 `text/event-stream`, each frame as
/// `data: <frame>\n\n`, then `data: [DONE]\n\n` (one chunk).
pub(crate) fn serve_sse(frames: &[&str]) -> FakeResponse {
    FakeResponse::sse(format!("{}data: [DONE]\n\n", sse_data(frames)))
}

/// Go responses `runStream`'s and `runBlocking`'s responses: `events` then `[DONE]` as SSE.
pub(crate) fn serve_events(events: &[&str]) -> FakeResponse {
    serve_sse(events)
}

/// Go completions `testRig`: a server answering with `handler` that records each request's
/// path and body ([`paths`](Self::paths), [`requests`](Self::requests)).
pub(crate) struct TestRig {
    /// The server.
    pub(crate) server: FakeServer,
}

impl TestRig {
    /// Go `newTestRig(t, handler)`.
    pub(crate) fn new<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
    {
        Self {
            server: new_loopback_server(handler),
        }
    }

    /// Go `rig.paths`: each request's path, in order.
    pub(crate) fn paths(&self) -> Vec<String> {
        self.server.requests().into_iter().map(|r| r.path).collect()
    }

    /// Go `rig.requests`: each request's body, in order.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.server
            .requests()
            .into_iter()
            .map(|r| r.body_text())
            .collect()
    }

    /// Go `rig.model(t)`: a Chat Completions model on this server.
    pub(crate) fn model(&self) -> Arc<dyn Llm> {
        new_completions_test_model(&self.server)
    }
}

/// The [`OpenAiError`] a model yielded (every model error is one, boxed in
/// [`ModelError::Other`]); panics on any other error.
pub(crate) fn openai_error(err: ModelError) -> OpenAiError {
    match err {
        ModelError::Other(boxed) => match boxed.downcast::<OpenAiError>() {
            Ok(e) => *e,
            Err(other) => panic!("model error is not an OpenAiError: {other}"),
        },
        other => panic!("model error is not an OpenAiError: {other}"),
    }
}

/// A request with one user turn of `text` (Go `genai.NewContentFromText(text, RoleUser)`).
pub(crate) fn user_request(text: &str) -> LlmRequest {
    LlmRequest {
        contents: vec![Content::from_text(text, "user")],
        ..LlmRequest::default()
    }
}

/// Go's collection loop: every response `llm` yields for `req`, and the first error (Go's
/// loop `continue`s past an error, so later responses are still collected).
pub(crate) async fn collect(
    llm: &dyn Llm,
    req: LlmRequest,
    stream: bool,
) -> (Vec<LlmResponse>, Option<OpenAiError>) {
    let mut got = Vec::new();
    let mut first_err = None;
    let mut items = llm.generate_content(req, stream);
    while let Some(item) = items.next().await {
        match item {
            Ok(resp) => got.push(resp),
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(openai_error(e));
                }
            }
        }
    }
    (got, first_err)
}

/// Go completions `ask(t, llm, stream)`: one user turn `weather?`.
pub(crate) async fn ask(llm: &dyn Llm, stream: bool) -> (Vec<LlmResponse>, Option<OpenAiError>) {
    collect(llm, user_request("weather?"), stream).await
}

/// Go responses `collectResponses(t, server, stream)`: a Responses model on `server`, one
/// user turn `World?`.
pub(crate) async fn collect_responses(
    server: &FakeServer,
    stream: bool,
) -> (Vec<LlmResponse>, Option<OpenAiError>) {
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(server));
    collect(llm.as_ref(), user_request("World?"), stream).await
}

/// Go responses `runStream(t, events...)`: the Responses model over a stream of `events`
/// (then `[DONE]`).
pub(crate) async fn run_stream(events: &[&str]) -> (Vec<LlmResponse>, Option<OpenAiError>) {
    let resp = serve_events(events);
    let server = new_loopback_server(move |_| resp.clone());
    collect_responses(&server, true).await
}

/// Go responses `runBlocking(t, body)`: the Responses model answered with `body` as JSON.
pub(crate) async fn run_blocking(body: &str) -> (Vec<LlmResponse>, Option<OpenAiError>) {
    let resp = serve_json(body);
    let server = new_loopback_server(move |_| resp.clone());
    collect_responses(&server, false).await
}

/// Go completions `responseText`: the non-thought text of a response.
pub(crate) fn response_text(resp: &LlmResponse) -> String {
    resp.content
        .as_ref()
        .map(|c| {
            c.parts
                .iter()
                .filter(|p| !p.thought)
                .map(|p| p.text.as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// Go responses `allText`: every part's text, thoughts included.
pub(crate) fn all_text(content: Option<&Content>) -> String {
    content
        .map(|c| c.parts.iter().map(|p| p.text.as_str()).collect())
        .unwrap_or_default()
}

/// Go responses `functionCalls`: the response's function calls, in order.
pub(crate) fn function_calls(resp: &LlmResponse) -> Vec<FunctionCall> {
    resp.content
        .as_ref()
        .map(|c| {
            c.parts
                .iter()
                .filter_map(|p| p.function_call.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Go completions `onlyCall`: the response's first function call; panics when there is none.
pub(crate) fn only_call(resp: &LlmResponse) -> FunctionCall {
    function_calls(resp)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no function call on {:?}", resp.content))
}

/// Go responses `partKinds`: `"call"` or `"text"` per part, in order.
pub(crate) fn part_kinds(resp: &LlmResponse) -> Vec<&'static str> {
    resp.content
        .as_ref()
        .map(|c| {
            c.parts
                .iter()
                .map(|p| {
                    if p.function_call.is_some() {
                        "call"
                    } else {
                        "text"
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Go responses `assertTurnShape`: only the last response closes the turn (`turn_complete`),
/// every earlier one is partial with no finish reason, the last is not partial; returns it.
pub(crate) fn assert_turn_shape(got: &[LlmResponse]) -> &LlmResponse {
    let Some((final_resp, partials)) = got.split_last() else {
        panic!("stream emitted no responses, want at least the aggregated turn");
    };
    let terminal: Vec<usize> = got
        .iter()
        .enumerate()
        .filter(|(_, r)| r.turn_complete)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        terminal,
        vec![got.len() - 1],
        "indices of responses with turn_complete"
    );
    for (i, resp) in partials.iter().enumerate() {
        assert_eq!(
            resp.finish_reason, None,
            "partial response {i} has a finish reason, want none"
        );
        assert!(
            resp.partial,
            "partial response {i} has partial = false, want true"
        );
    }
    assert!(
        !final_resp.partial,
        "final response has partial = true, want false"
    );
    final_resp
}

/// Go responses `assertFinishSignal`: a turn that carries an answer reports `want_message`
/// under [`FINISH_MESSAGE_KEY`](crate::FINISH_MESSAGE_KEY) and leaves the error fields empty.
pub(crate) fn assert_finish_signal(resp: &LlmResponse, want_message: &str) {
    assert!(
        resp.content.as_ref().is_some_and(|c| !c.parts.is_empty()),
        "response carries no content, so this is not the case being asserted"
    );
    assert!(
        resp.error_code.is_empty() && resp.error_message.is_empty(),
        "response has error_code {:?} / error_message {:?}, want both empty on a turn that carries an answer",
        resp.error_code,
        resp.error_message
    );
    let got = resp
        .custom_metadata
        .get(crate::FINISH_MESSAGE_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(
        got,
        want_message,
        "custom_metadata[{:?}]",
        crate::FINISH_MESSAGE_KEY
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{CHAT_COMPLETIONS_PATH, RESPONSES_PATH, Reply};
    use bytes::Bytes;
    use gadk_genai::Part;
    use pretty_assertions::assert_eq;

    #[test]
    fn sse_helpers_write_gos_frames() {
        assert_eq!(sse_data(&["{}", "[DONE]"]), "data: {}\n\ndata: [DONE]\n\n");
        let resp = serve_sse(&[r#"{"a":1}"#]);
        assert_eq!(
            resp.body_bytes(),
            Bytes::from_static(b"data: {\"a\":1}\n\ndata: [DONE]\n\n")
        );
        assert!(
            resp.headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "text/event-stream")
        );
        let json = serve_json("{}");
        assert_eq!(json.status, 200);
    }

    /// The rig records what Go's `testRig` records, through the client a test model uses.
    #[tokio::test]
    async fn rig_records_paths_and_bodies_over_a_loopback_socket() {
        let rig = TestRig::new(|_| serve_json(r#"{"id":"c"}"#));
        let cfg = TestClientConfig::for_server(&rig.server);
        let client = test_client(Api::ChatCompletions, &cfg);
        let reply = client
            .post(
                CHAT_COMPLETIONS_PATH,
                Bytes::from_static(b"{\"model\":\"m\"}"),
                false,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(reply, Reply::Body { .. }));
        assert_eq!(rig.paths(), vec!["/v1/chat/completions".to_string()]);
        assert_eq!(rig.requests(), vec!["{\"model\":\"m\"}".to_string()]);
        let seen = rig.server.last_request();
        assert_eq!(seen.header("authorization"), "Bearer test");
        assert_eq!(seen.header("content-type"), "application/json");
    }

    #[tokio::test(start_paused = true)]
    async fn scripted_config_points_at_the_scripted_client() {
        let fake = ScriptedClient::json("{}");
        let client = test_client(Api::Responses, &TestClientConfig::for_scripted(&fake));
        client
            .post(RESPONSES_PATH, Bytes::from_static(b"{}"), false, None)
            .await
            .unwrap();
        assert_eq!(fake.last_request().url, "http://127.0.0.1:1/v1/responses");
    }

    #[test]
    fn test_models_carry_the_name() {
        let fake = ScriptedClient::json("{}");
        let cfg = TestClientConfig::for_scripted(&fake);
        assert_eq!(new_test_model("m1", &cfg).name(), "m1");
        assert_eq!(test_model(Api::ChatCompletions, "m2", &cfg).name(), "m2");
    }

    #[test]
    fn response_readers() {
        let resp = LlmResponse {
            content: Some(Content::from_parts(
                vec![
                    Part {
                        text: "think".into(),
                        thought: true,
                        ..Default::default()
                    },
                    Part::from_text("hi"),
                    Part {
                        function_call: Some(FunctionCall {
                            name: "f".into(),
                            id: "c1".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                "model",
            )),
            ..LlmResponse::default()
        };
        assert_eq!(response_text(&resp), "hi");
        assert_eq!(all_text(resp.content.as_ref()), "thinkhi");
        assert_eq!(part_kinds(&resp), vec!["text", "text", "call"]);
        assert_eq!(only_call(&resp).id, "c1");
        assert_eq!(function_calls(&resp).len(), 1);
        let req = user_request("weather?");
        assert_eq!(req.contents[0].role, "user");
        assert_eq!(req.contents[0].parts[0].text, "weather?");
    }

    #[test]
    fn turn_shape_and_finish_signal() {
        let partial = LlmResponse {
            partial: true,
            ..LlmResponse::default()
        };
        let mut last = LlmResponse {
            turn_complete: true,
            content: Some(Content::from_text("hello", "model")),
            ..LlmResponse::default()
        };
        last.custom_metadata.insert(
            crate::FINISH_MESSAGE_KEY.into(),
            serde_json::Value::String("content_filter".into()),
        );
        let got = vec![partial, last];
        let final_resp = assert_turn_shape(&got);
        assert_finish_signal(final_resp, "content_filter");
    }

    #[test]
    fn openai_error_unwraps_model_errors() {
        let err = ModelError::from(OpenAiError::plain("boom"));
        assert_eq!(openai_error(err).to_string(), "boom");
    }
}
