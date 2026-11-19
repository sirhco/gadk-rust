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
//! The `async_stream_query` method.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use bytes::Bytes;
use futures::StreamExt as _;
use gadk_agent::{EventStream, RunConfig, StreamingMode};
use gadk_engine::runner::{RunOptions, Runner, RunnerConfig};
use gadk_genai::Content;
use gadk_session::Event;
use http::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE};
use http::{HeaderValue, StatusCode};
use serde_json::{Map, Value};

use super::MethodHandler;
use crate::agentengine::encode::{emit_json, emit_json_error};
use crate::agentengine::encode_shapes;
use crate::agentengine::models::{StreamQueryInput, StreamQueryRequest, StreamQueryTextRequest};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::agentengine::{AgentEngineConfig, Log};
use crate::mux::Response;

/// Handler for `async_stream_query`.
pub(crate) struct StreamQueryHandler {
    /// Services, loader, plugins, compaction and platform.
    pub(crate) config: AgentEngineConfig,
    /// The runner's app name.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
    /// Where log lines go (from `config.log`).
    pub(crate) log: Log,
}

impl StreamQueryHandler {
    /// A handler serving `method_name` in `api_mode`, running the root agent of `config`.
    pub(crate) fn new(
        config: AgentEngineConfig,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        let log = Log(config.log.clone());
        StreamQueryHandler {
            config,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
            log,
        }
    }

    /// Decodes `payload` as `StreamQueryRequest` then
    /// `StreamQueryTextRequest` (both through `go_unmarshal`; both failing →
    /// an `Err` naming both request types with both decode errors, also
    /// logged; a text message becomes a user
    /// `Content` with one text part), builds the runner (`Err("failed to run agent: failed to create
    /// runner: <err>")`, logged), and answers 200 with `Content-Type: application/json`,
    /// `Cache-Control: no-cache`, `Connection: keep-alive` and a streaming body: per runner
    /// item, log `Processing event: … err: …`; a compaction error is logged `agentengine: <err>`
    /// and skipped; any other error is logged `error in events: <err>\n`, written as one
    /// `emit_json_error` line, and ends the stream; a `None`/content-less event is skipped; an
    /// event is one `emit_json(&event)` line (an emit failure is logged
    /// with the encoder's error text, written as an error line, and ends the stream).
    pub(crate) async fn stream_jsonl(&self, payload: Bytes) -> Result<Response, String> {
        // try to unmarshal StreamQueryRequest first
        let req = match go_unmarshal::<StreamQueryRequest>(
            &payload,
            &encode_shapes::MODELS_STREAM_QUERY_REQUEST,
        ) {
            Ok(req) => req,
            Err(err) => {
                // try to unmarshal StreamQueryTextRequest
                match go_unmarshal::<StreamQueryTextRequest>(
                    &payload,
                    &encode_shapes::MODELS_STREAM_QUERY_TEXT_REQUEST,
                ) {
                    // got text, create a full content based on that text
                    Ok(req_text) => StreamQueryRequest {
                        class_method: req_text.class_method,
                        input: StreamQueryInput {
                            user_id: req_text.input.user_id,
                            session_id: req_text.input.session_id,
                            message: Content::from_text(req_text.input.message, "user"),
                        },
                    },
                    Err(err_text) => {
                        let err = format!(
                            "could not decode the request as StreamQueryRequest ({err}) or StreamQueryTextRequest ({err_text})"
                        );
                        self.log.print(&err);
                        return Err(err);
                    }
                }
            }
        };

        let events = self.run(&req).map_err(|err| {
            let err = format!("failed to run agent: {err}");
            self.log.print(&err);
            err
        })?;

        Ok(ndjson_response(events, self.log.clone(), |event, log| {
            emit_json(event, log)
        }))
    }

    /// Runs a runner (`app_name` = the agent engine ID, the loader's root agent, the config's
    /// services, plugins, compaction and platform, `auto_create_session: true`) for
    /// `req.input` with `StreamingMode::Sse`.
    pub(crate) fn run(&self, req: &StreamQueryRequest) -> Result<EventStream, String> {
        let runner = new_runner(&self.config, &self.agent_engine_id)?;
        Ok(runner.run(
            &req.input.user_id,
            &req.input.session_id,
            Some(req.input.message.clone()),
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
            RunOptions::default(),
        ))
    }
}

/// Builds the runner both stream methods use: `app_name` = the agent engine ID, the loader's
/// root agent, the config's services, plugins, compaction and platform, and
/// `auto_create_session: true`. An error is `failed to create runner: <err>`.
pub(crate) fn new_runner(
    config: &AgentEngineConfig,
    agent_engine_id: &str,
) -> Result<Runner, String> {
    Runner::new(RunnerConfig {
        app_name: agent_engine_id.to_owned(),
        agent: config.agent_loader.root_agent(),
        session_service: Arc::clone(&config.session_service),
        artifact_service: config.artifact_service.clone(),
        memory_service: config.memory_service.clone(),
        plugin_config: config.plugin_config.clone(),
        auto_create_session: true,
        platform: config.platform.clone(),
        compaction: config.compaction.clone(),
    })
    .map_err(|err| format!("failed to create runner: {err}"))
}

/// The response both stream methods answer once the run has started: 200 with three headers
/// (`Content-Type: application/json`, `Cache-Control: no-cache`, `Connection: keep-alive`) and
/// an NDJSON body streamed from `events`, one body chunk per line (each line is flushed). Per
/// runner item:
///
/// - log `Processing event: <event> err: <err>` (see [`processing_event`]);
/// - a compaction error: log `agentengine: <err>` and go on;
/// - another error: log `error in events: <err>\n`, write one [`emit_json_error`] line, end;
/// - an event without content: skip it;
/// - an event: the line `line(&event, &log)`; an `Err(e)` is logged
///   with the encoder's error text, written as an error line, and ends the stream.
///
/// An error line that cannot be encoded is logged with the encoder's error text and
/// ends the stream without a line.
pub(crate) fn ndjson_response<F>(events: EventStream, log: Log, line: F) -> Response
where
    F: Fn(&Event, &Log) -> Result<Bytes, String> + Send + 'static,
{
    let lines = futures::stream::unfold(Some((events, log, line)), |state| async move {
        let (mut events, log, line) = state?;
        loop {
            let item = events.next().await?;
            log.print(&processing_event(&item));
            let event = match item {
                // A compaction failure is bookkeeping, not the turn: log, go on.
                Err(err) if err.is_compaction() => {
                    log.print(&format!("agentengine: {err}"));
                    continue;
                }
                Err(err) => {
                    log.print(&format!("error in events: {err}\n"));
                    let chunk = error_line(&err.to_string(), &log)?;
                    return Some((Ok::<Bytes, Infallible>(chunk), None));
                }
                Ok(event) => event,
            };
            if event.response.content.is_none() {
                continue;
            }
            return match line(&event, &log) {
                Ok(chunk) => Some((Ok(chunk), Some((events, log, line)))),
                Err(err) => {
                    let e = format!("failed to emit JSON chunk: {err}");
                    log.print(&e);
                    let chunk = error_line(&e, &log)?;
                    Some((Ok(chunk), None))
                }
            };
        }
    });
    let mut resp = Response::new(Body::from_stream(lines));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    resp
}

/// The error line written in the stream loop, or `None` after logging
/// the encoder's error text.
fn error_line(text: &str, log: &Log) -> Option<Bytes> {
    match emit_json_error(text, log) {
        Ok(chunk) => Some(chunk),
        Err(e) => {
            log.print(&format!("failed to emit JSON error chunk: {e}"));
            None
        }
    }
}

/// The `Processing event: <event> err: <err>` log line: the event as Rust `Debug` and the error
/// as its text, `<nil>` for the absent one. The event dump is Rust's `Debug` layout, so the
/// replay fixtures drop these lines.
fn processing_event(item: &Result<Event, gadk_agent::AgentError>) -> String {
    match item {
        Ok(event) => format!("Processing event: {event:?} err: none\n"),
        Err(err) => format!("Processing event: none err: {err}\n"),
    }
}

/// The method's `description` in its metadata.
const DESCRIPTION: &str = "Streams responses asynchronously from the ADK application.\nArgs:\n    message (genai.Content):\n        Required. The message to stream responses for.\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Optional. The ID of the session. If not provided, a new session will be created for the user.\n\nYields:\n    Single lines with JSON encoded event each. Errors are also emitted as JSON.\n\n";

/// The method's metadata map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
                "message": {
                    "anyOf": [
                        {
                            "type": "string",
                        },
                        {
                            "additionalProperties": true,
                            "type": "object",
                        },
                    ],
                },
                "session_id": {
                    "nullable": true,
                    "type": "string",
                },
                "user_id": {
                    "type": "string",
                },
            },
            "required": [
                "message",
                "user_id",
            ],
            "type": "object",
        }),
        DESCRIPTION,
    )
}

#[async_trait::async_trait]
impl MethodHandler for StreamQueryHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Runs [`StreamQueryHandler::stream_jsonl`], an error wrapped as
    /// `streaming JSONL failed: <err>`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        // stream_jsonl fails only before streaming; the controller answers that with a status.
        self.stream_jsonl(payload)
            .await
            .map_err(|err| format!("streaming JSONL failed: {err}"))
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

    use axum::body::to_bytes;
    use futures::stream;
    use gadk_agent::AgentError;
    use gadk_session::compaction::CompactionError;
    use pretty_assertions::assert_eq;

    use super::*;

    type Lines = Arc<Mutex<Vec<String>>>;

    fn capture() -> (Log, Lines) {
        let lines: Lines = Arc::default();
        let sink = Arc::clone(&lines);
        let log = Log(Some(Arc::new(move |m: &str| {
            sink.lock().unwrap().push(m.to_owned());
        })));
        (log, lines)
    }

    async fn body_text(resp: Response) -> String {
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    fn event(text: Option<&str>) -> Event {
        let mut e = Event::default();
        e.response.content = text.map(|t| Content::from_text(t, "model"));
        e.author = "a".into();
        e
    }

    /// Go's loop: content-less events skipped, a compaction error logged and skipped, another
    /// error written as one `{"error"}` line that ends the stream.
    #[tokio::test]
    async fn ndjson_skips_and_ends_as_go() {
        let items: Vec<Result<Event, AgentError>> = vec![
            Ok(event(Some("one"))),
            Ok(event(None)),
            Err(AgentError::Compaction(CompactionError::new("squeeze"))),
            Ok(event(Some("two"))),
            Err(AgentError::other("boom")),
            Ok(event(Some("never"))),
        ];
        let (log, lines) = capture();
        let resp = ndjson_response(Box::pin(stream::iter(items)), log, |e, log| {
            emit_json(e, log)
        });
        assert_eq!(resp.status(), StatusCode::OK);
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
            .collect();
        assert_eq!(
            headers,
            vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                ("cache-control".to_owned(), "no-cache".to_owned()),
                ("connection".to_owned(), "keep-alive".to_owned()),
            ]
        );
        let body = body_text(resp).await;
        let got: Vec<Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(got.len(), 3, "{body}");
        assert_eq!(got[0]["content"]["parts"][0]["text"], "one");
        assert_eq!(got[1]["content"]["parts"][0]["text"], "two");
        assert_eq!(got[2], serde_json::json!({"error": "boom"}));
        assert!(body.ends_with('\n'));
        let logged: Vec<String> = lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| !l.starts_with("Processing event: "))
            .cloned()
            .collect();
        assert_eq!(
            logged,
            vec![
                "agentengine: context compaction failed: squeeze".to_owned(),
                "error in events: boom\n".to_owned()
            ]
        );
    }

    /// An event line that cannot be written is logged and becomes the last (error) line.
    #[tokio::test]
    async fn emit_failure_is_an_error_line() {
        let (log, lines) = capture();
        let items: Vec<Result<Event, AgentError>> =
            vec![Ok(event(Some("one"))), Ok(event(Some("two")))];
        let resp = ndjson_response(Box::pin(stream::iter(items)), log, |_, _| {
            Err("failed to encode SSE response chunk: bad".to_owned())
        });
        assert_eq!(
            body_text(resp).await,
            "{\"error\":\"failed to emit JSON chunk: failed to encode SSE response chunk: bad\"}\n"
        );
        assert!(lines.lock().unwrap().contains(
            &"failed to emit JSON chunk: failed to encode SSE response chunk: bad".to_owned()
        ));
    }

    /// The `AgentEngineLog` hook runs on the caller's task (as Go's `log.Print` does): a hook
    /// that needs the tokio runtime gets every line, in order, and the stream writes Go's normal
    /// lines.
    #[tokio::test]
    async fn log_hook_runs_on_the_runtime() {
        let lines: Lines = Arc::default();
        let sink = Arc::clone(&lines);
        let log = Log(Some(Arc::new(move |m: &str| {
            // Panics off a tokio runtime.
            let _handle = tokio::runtime::Handle::current();
            sink.lock().unwrap().push(m.to_owned());
        })));
        let items: Vec<Result<Event, AgentError>> = vec![
            Ok(event(Some("one"))),
            Err(AgentError::Compaction(CompactionError::new("squeeze"))),
            Ok(event(Some("two"))),
        ];
        let body = body_text(ndjson_response(
            Box::pin(stream::iter(items)),
            log,
            emit_json,
        ))
        .await;
        let got: Vec<Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(got.len(), 2, "{body}");
        assert_eq!(got[0]["content"]["parts"][0]["text"], "one");
        assert_eq!(got[1]["content"]["parts"][0]["text"], "two");
        let logged = lines.lock().unwrap().clone();
        assert_eq!(logged.len(), 4, "{logged:?}");
        assert!(logged[0].starts_with("Processing event: Event {"));
        assert_eq!(
            logged[1],
            "Processing event: none err: context compaction failed: squeeze\n"
        );
        assert_eq!(logged[2], "agentengine: context compaction failed: squeeze");
        assert!(logged[3].starts_with("Processing event: Event {"));
    }

    /// The Go wire fixtures of the stream routes (`testdata/agentengine/main_09`–`main_18`,
    /// C.11) replayed through `new_handler` with the probe's scenario, after the session
    /// fixtures `main_02`–`main_08` they build on (so the clock and UUID sequences match).
    #[tokio::test]
    async fn replays_the_stream_fixtures() {
        crate::agentengine::method::replay::replay_all(
            "main",
            &[
                "main_02_create_session",
                "main_03_create_session_with_state",
                "main_04_get_session",
                "main_05_get_session_missing",
                "main_06_list_sessions",
                "main_07_delete_session",
                "main_08_delete_session_missing",
                "main_09_stream_query_text",
                "main_10_stream_query_content",
                "main_11_stream_query_tool_call",
                "main_12_stream_query_model_error",
                "main_13_stream_query_bad_input",
                "main_14_run_with_events_new_session",
                "main_15_run_with_events_reuse_session",
                "main_16_run_with_events_bad_request_json",
                "main_17_session_method_on_stream_route",
                "main_18_stream_method_on_session_route",
            ],
        )
        .await;
    }
}
