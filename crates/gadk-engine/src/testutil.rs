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

//! Test support: [`MockModel`], the `collect_*` stream helpers, Gemini replay helpers and
//! [`TestAgentRunner`].

use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{AgentError, EventStream};
use gadk_genai::{Candidate, Content, FinishReason, GenerateContentResponse, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError, StreamingResponseAggregator};
use gadk_session::Event;

enum Script {
    Responses(VecDeque<Result<LlmResponse, ModelError>>),
    Streams(VecDeque<Vec<Result<LlmResponse, ModelError>>>),
    /// Queued contents, replayed as responses; `stream_responses_count` chunks per streaming call.
    Contents {
        contents: VecDeque<Content>,
        stream_responses_count: usize,
    },
}

/// Scripted [`Llm`]: records every request and replays queued responses.
pub struct MockModel {
    name: String,
    script: parking_lot::Mutex<Script>,
    requests: parking_lot::Mutex<Vec<LlmRequest>>,
}

/// The error a model returns once its queue is empty.
fn no_data() -> ModelError {
    ModelError::Other("no data".into())
}

impl MockModel {
    fn with_script(script: Script) -> Arc<Self> {
        Arc::new(Self {
            name: "mock".to_string(),
            script: parking_lot::Mutex::new(script),
            requests: parking_lot::Mutex::new(Vec::new()),
        })
    }

    /// A model whose every call yields exactly the next queued item, or `"no data"` once empty.
    pub fn new(responses: Vec<Result<LlmResponse, ModelError>>) -> Arc<Self> {
        Self::with_script(Script::Responses(responses.into()))
    }

    /// A model whose every call yields all items of the next queued chunk list, or one `"no data"`
    /// error once empty.
    pub fn with_stream(chunks: Vec<Vec<Result<LlmResponse, ModelError>>>) -> Arc<Self> {
        Self::with_script(Script::Streams(chunks.into()))
    }

    /// A model replaying `contents` as responses.
    pub fn from_contents(contents: Vec<Content>) -> Arc<Self> {
        Self::from_contents_streaming(contents, 0)
    }

    /// A model replaying `contents` as responses, streaming `n` chunks per call.
    pub fn from_contents_streaming(
        contents: Vec<Content>,
        stream_responses_count: usize,
    ) -> Arc<Self> {
        Self::with_script(Script::Contents {
            contents: contents.into(),
            stream_responses_count,
        })
    }

    /// The requests received so far.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().clone()
    }

    /// Renames the model; must be called before the model is shared.
    pub fn name_as(self: Arc<Self>, name: &str) -> Arc<Self> {
        let mut m = Arc::try_unwrap(self).unwrap_or_else(|_| {
            panic!("MockModel::name_as must be called before the model is shared")
        });
        m.name = name.to_string();
        Arc::new(m)
    }
}

/// The streamed chunks for the next queued contents.
fn go_stream(
    contents: &mut VecDeque<Content>,
    count: usize,
) -> Vec<Result<LlmResponse, ModelError>> {
    let n = if count == 0 { 1 } else { count };
    let mut agg = StreamingResponseAggregator::new();
    let mut out = Vec::new();
    for i in 0..n {
        let Some(c) = contents.front().cloned() else {
            break;
        };
        // Only the chunk that ends the turn reports why, as a real stream does.
        let finish_reason = (i == n - 1 || contents.len() == 1).then_some(FinishReason::Stop);
        contents.pop_front();
        let resp = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(c),
                finish_reason,
                ..Default::default()
            }],
            ..Default::default()
        };
        out.extend(agg.process_response(resp));
    }
    if let Some(r) = agg.close() {
        out.push(Ok(r));
    }
    out
}

impl Llm for MockModel {
    fn name(&self) -> &str {
        &self.name
    }

    /// Records `req` and yields the scripted items. The items are computed eagerly: the stream must
    /// be `'static` and cannot borrow `self`.
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.requests.lock().push(req);
        let items = {
            let mut script = self.script.lock();
            match &mut *script {
                Script::Responses(q) => vec![q.pop_front().unwrap_or_else(|| Err(no_data()))],
                Script::Streams(q) => q.pop_front().unwrap_or_else(|| vec![Err(no_data())]),
                Script::Contents {
                    contents,
                    stream_responses_count,
                } => {
                    if stream {
                        go_stream(contents, *stream_responses_count)
                    } else {
                        vec![contents.pop_front().map_or_else(
                            || Err(no_data()),
                            |c| {
                                Ok(LlmResponse {
                                    content: Some(c),
                                    finish_reason: Some(FinishReason::Stop),
                                    ..Default::default()
                                })
                            },
                        )]
                    }
                }
            }
        };
        futures::stream::iter(items).boxed()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Drives `s`, passing each event with content to `f`; fails on a stream error or an event without
/// content .
async fn collect_with(mut s: EventStream, mut f: impl FnMut(Event)) -> Result<(), AgentError> {
    while let Some(item) = s.next().await {
        let ev = item?;
        if ev.response.content.is_none() {
            return Err(AgentError::other(format!("unexpected empty event: {ev:?}")));
        }
        f(ev);
    }
    Ok(())
}

/// Every event of `s`, until an error.
pub async fn collect_events(s: EventStream) -> Result<Vec<Event>, AgentError> {
    let mut out = Vec::new();
    collect_with(s, |ev| out.push(ev)).await?;
    Ok(out)
}

/// Every content part of `s`, until an error.
pub async fn collect_parts(s: EventStream) -> Result<Vec<Part>, AgentError> {
    let mut out = Vec::new();
    collect_with(s, |ev| {
        if let Some(c) = ev.response.content {
            out.extend(c.parts);
        }
    })
    .await?;
    Ok(out)
}

/// Every non-empty text part of `s`, until an error.
pub async fn collect_text_parts(s: EventStream) -> Result<Vec<String>, AgentError> {
    let mut out = Vec::new();
    collect_with(s, |ev| {
        if let Some(c) = ev.response.content {
            out.extend(
                c.parts
                    .into_iter()
                    .map(|p| p.text)
                    .filter(|t| !t.is_empty()),
            );
        }
    })
    .await?;
    Ok(out)
}

/// Drops volatile headers, compact JSON bodies.
#[cfg(feature = "testutil")]
pub fn scrub_gemini_request(
    req: &mut http::Request<bytes::Bytes>,
) -> Result<(), gadk_httprr::HttprrError> {
    // http::HeaderMap is case-insensitive, so one remove covers every spelling.
    for h in ["x-goog-api-key", "x-goog-api-client", "user-agent"] {
        req.headers_mut().remove(h);
    }
    let ctype = req
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if (ctype == "application/json" || ctype.starts_with("application/json;"))
        && let Some(c) = compact_json(req.body())
    {
        *req.body_mut() = bytes::Bytes::from(c);
    }
    Ok(())
}

/// Elides insignificant whitespace from JSON; `None` if `src` is not valid JSON (the caller leaves
/// the body unchanged then). Key order and number text are preserved byte for byte.
#[cfg(feature = "testutil")]
fn compact_json(src: &[u8]) -> Option<Vec<u8>> {
    serde_json::from_slice::<serde::de::IgnoredAny>(src).ok()?;
    let mut out = Vec::with_capacity(src.len());
    let (mut in_str, mut esc) = (false, false);
    for &b in src {
        if in_str {
            out.push(b);
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if !matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            if b == b'"' {
                in_str = true;
            }
            out.push(b);
        }
    }
    Some(out)
}

/// A `RecordReplay` over the real client plus [`scrub_gemini_request`]. Panics when the recording
/// cannot be opened.
#[cfg(feature = "testutil")]
pub fn gemini_replay_client(path: impl AsRef<std::path::Path>) -> Arc<dyn gadk_model::HttpClient> {
    let path = path.as_ref();
    let mut rr =
        gadk_httprr::RecordReplay::open(path, Arc::new(gadk_model::ReqwestClient::default()))
            .unwrap_or_else(|e| panic!("httprr.Open({}) failed: {e}", path.display()));
    rr.scrub_req(Box::new(scrub_gemini_request));
    Arc::new(rr)
}

/// The Gemini client config for a recording: `"fakekey"` and an empty environment when replaying;
/// the real environment and key when recording.
#[cfg(feature = "testutil")]
pub fn gemini_test_config(path: impl AsRef<std::path::Path>) -> gadk_gemini::ClientConfig {
    let path = path.as_ref();
    let recording = gadk_httprr::RecordReplay::recording(path).unwrap_or(false);
    gadk_gemini::ClientConfig {
        api_key: if recording {
            None
        } else {
            Some("fakekey".to_string())
        },
        http_client: Some(gemini_replay_client(path)),
        env: if recording {
            Arc::new(gadk_gemini::ProcessEnv)
        } else {
            Arc::new(gadk_gemini::MapEnv(Default::default()))
        },
        ..Default::default()
    }
}

/// A Gemini model replaying the recording at `path`.
#[cfg(feature = "testutil")]
pub async fn gemini_test_model(model: &str, path: impl AsRef<std::path::Path>) -> Arc<dyn Llm> {
    gadk_gemini::GeminiModel::new(model, gemini_test_config(path))
        .await
        .unwrap_or_else(|e| panic!("GeminiModel::new({model}) failed: {e}"))
}

/// A runner over an in-memory session service with one session
/// (`"test_app"`/`"test_user"`/`"test_session"`), created up front.
pub struct TestAgentRunner {
    runner: crate::runner::Runner,
    session_service: Arc<dyn gadk_session::SessionService>,
    session_id: String,
}

impl TestAgentRunner {
    /// A runner for `agent`.
    pub async fn new(agent: Arc<dyn gadk_agent::Agent>) -> Self {
        Self::build(agent, crate::runner::PluginConfig::default(), None, None).await
    }

    /// A runner for `agent` with the given plugins.
    pub async fn with_plugins(
        agent: Arc<dyn gadk_agent::Agent>,
        cfg: crate::runner::PluginConfig,
    ) -> Self {
        Self::build(agent, cfg, None, None).await
    }

    /// A runner for `agent` whose session starts with `state`.
    pub async fn with_state(
        agent: Arc<dyn gadk_agent::Agent>,
        state: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::build(
            agent,
            crate::runner::PluginConfig::default(),
            Some(state),
            None,
        )
        .await
    }

    /// A runner with context compaction enabled.
    pub async fn with_compaction(
        agent: Arc<dyn gadk_agent::Agent>,
        cfg: gadk_session::compaction::Config,
    ) -> Self {
        Self::build(
            agent,
            crate::runner::PluginConfig::default(),
            None,
            Some(cfg),
        )
        .await
    }

    async fn build(
        agent: Arc<dyn gadk_agent::Agent>,
        cfg: crate::runner::PluginConfig,
        state: Option<serde_json::Map<String, serde_json::Value>>,
        compaction: Option<gadk_session::compaction::Config>,
    ) -> Self {
        let session_service: Arc<dyn gadk_session::SessionService> =
            Arc::new(gadk_session::InMemorySessionService::new());
        let runner = crate::runner::Runner::new(crate::runner::RunnerConfig {
            app_name: "test_app".into(),
            agent,
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: cfg,
            auto_create_session: false,
            platform: gadk_platform::Platform::default(),
            compaction,
        })
        .expect("runner::Runner::new");
        // `run` takes no session ID and the runner does not auto-create, so the session is created
        // here with the initial state.
        let session = session_service
            .create(gadk_session::CreateRequest {
                app_name: "test_app".into(),
                user_id: "test_user".into(),
                session_id: Some("test_session".into()),
                state,
            })
            .await
            .expect("create test session");
        Self {
            runner,
            session_service,
            session_id: session.id().to_string(),
        }
    }

    /// Runs a user text message; an empty message runs with no content.
    pub fn run(&self, msg: &str) -> EventStream {
        let msg = (!msg.is_empty()).then(|| Content::from_text(msg, gadk_genai::ROLE_USER));
        self.runner.run(
            "test_user",
            &self.session_id,
            msg,
            gadk_agent::RunConfig::default(),
            crate::runner::RunOptions::default(),
        )
    }

    /// Runs `msg`.
    pub fn run_content(&self, msg: Content) -> EventStream {
        self.run_with_config(msg, gadk_agent::RunConfig::default())
    }

    /// Runs `msg` with `cfg`.
    pub fn run_with_config(&self, msg: Content, cfg: gadk_agent::RunConfig) -> EventStream {
        self.runner.run(
            "test_user",
            &self.session_id,
            Some(msg),
            cfg,
            crate::runner::RunOptions::default(),
        )
    }

    /// The test session's ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The runner's session service, for inspecting stored events.
    pub fn session_service(&self) -> Arc<dyn gadk_session::SessionService> {
        self.session_service.clone()
    }
}

/// The scripted in-memory live model and the responses its scripted server messages decode to.
pub mod live;

/// Telemetry test helpers: the span and log digests, the `tracing` capture behind them, and the
/// lock that serialises tests changing the telemetry environment.
pub mod telemetry {
    pub use crate::telemetry::log_digest::{LogDigest, LogRecord, build_log_digest, capture_logs};
    pub use crate::telemetry::span_digest::{
        Digest, PRESENT, SpanDigest, SpanStub, build_digest, capture_spans,
    };

    /// Serialises tests that change the process-wide telemetry environment
    /// ([`set_env_provider`](crate::telemetry::set_env_provider)); hold it for the whole test and
    /// restore `MapEnv::default()` before releasing it.
    pub static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_model_pops_responses_and_records_requests() {
        let m = MockModel::new(vec![Ok(LlmResponse {
            model_version: "a".into(),
            ..Default::default()
        })]);
        let req = LlmRequest {
            model: "x".into(),
            ..Default::default()
        };
        let got: Vec<_> = m.generate_content(req.clone(), false).collect().await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap().model_version, "a");
        let err: Vec<_> = m.generate_content(req, false).collect().await;
        assert_eq!(err[0].as_ref().unwrap_err().to_string(), "no data");
        assert_eq!(m.requests().len(), 2);
        assert_eq!(m.name(), "mock");
        assert_eq!(MockModel::new(vec![]).name_as("other").name(), "other");
    }

    #[tokio::test]
    async fn mock_model_from_contents_matches_go() {
        let m = MockModel::from_contents(vec![Content::from_text("hi", gadk_genai::ROLE_MODEL)]);
        let got: Vec<_> = m
            .generate_content(LlmRequest::default(), false)
            .collect()
            .await;
        let r = got[0].as_ref().unwrap();
        assert_eq!(r.finish_reason, Some(gadk_genai::FinishReason::Stop));
        assert_eq!(r.content.as_ref().unwrap().parts[0].text, "hi");
        // exhausted: Go errNoModelData
        let got: Vec<_> = m
            .generate_content(LlmRequest::default(), false)
            .collect()
            .await;
        assert_eq!(got[0].as_ref().unwrap_err().to_string(), "no data");

        // streaming: 2 chunks → 2 partials + aggregated close
        let m = MockModel::from_contents_streaming(
            vec![
                Content::from_text("a", gadk_genai::ROLE_MODEL),
                Content::from_text("b", gadk_genai::ROLE_MODEL),
            ],
            2,
        );
        let got: Vec<_> = m
            .generate_content(LlmRequest::default(), true)
            .collect()
            .await;
        let last = got.last().unwrap().as_ref().unwrap();
        assert!(!last.partial);
        assert_eq!(last.content.as_ref().unwrap().parts[0].text, "ab");
    }

    #[cfg(feature = "testutil")]
    #[test]
    fn scrub_gemini_request_removes_headers_and_compacts() {
        let mut req = http::Request::post("https://x/v1beta/models/m:generateContent")
            .header("x-goog-api-key", "k")
            .header("x-goog-api-client", "v")
            .header("user-agent", "ua")
            .header("content-type", "application/json")
            .body(bytes::Bytes::from_static(
                b"{ \"a\": [1, 2],\n \"s\": \"x y\" }",
            ))
            .unwrap();
        scrub_gemini_request(&mut req).unwrap();
        assert!(req.headers().get("x-goog-api-key").is_none());
        assert!(req.headers().get("x-goog-api-client").is_none());
        assert!(req.headers().get("user-agent").is_none());
        assert_eq!(req.body().as_ref(), br#"{"a":[1,2],"s":"x y"}"#);
        // invalid JSON is left as is; non-JSON content type is left as is
        let mut bad = http::Request::post("https://x")
            .header("content-type", "application/json; charset=utf-8")
            .body(bytes::Bytes::from_static(b"{ not json"))
            .unwrap();
        scrub_gemini_request(&mut bad).unwrap();
        assert_eq!(bad.body().as_ref(), b"{ not json");
        let mut text = http::Request::post("https://x")
            .header("content-type", "text/plain")
            .body(bytes::Bytes::from_static(b"{ }"))
            .unwrap();
        scrub_gemini_request(&mut text).unwrap();
        assert_eq!(text.body().as_ref(), b"{ }");
    }

    #[cfg(feature = "testutil")]
    #[test]
    fn gemini_test_config_uses_fake_key_when_replaying() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/testdata/tool/functiontool/testdata/TestFunctionTool_Simple.httprr");
        let cfg = gemini_test_config(&path);
        assert_eq!(cfg.api_key.as_deref(), Some("fakekey"));
        assert!(cfg.http_client.is_some());
    }
}
