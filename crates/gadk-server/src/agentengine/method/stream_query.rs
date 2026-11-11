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
//! Go `server/agentengine/controllers/method/stream_query.go` (`async_stream_query`).

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
use crate::agentengine::unmarshal::{go_unmarshal, needs_deep_stack, on_deep_stack};
use crate::agentengine::{AgentEngineConfig, Log};
use crate::mux::Response;

/// Go `NewStreamQueryHandler`'s handler.
pub(crate) struct StreamQueryHandler {
    /// Go `config`: services, loader, plugins, compaction, platform.
    pub(crate) config: AgentEngineConfig,
    /// Go `agentEngineID`: the runner's app name.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
    /// Go `log` (from `config.log`).
    pub(crate) log: Log,
}

impl StreamQueryHandler {
    /// Go `NewStreamQueryHandler(config, agentEngineID, methodName, apiMode)`.
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

    /// Go `streamJSONL`: decodes `payload` as `models.StreamQueryRequest` then
    /// `models.StreamQueryTextRequest` (both through `go_unmarshal`; both failing →
    /// `Err("json.Unmarshal() failed both for models.StreamQueryRequest (<e1>) and
    /// models.StreamQueryTextRequest (<e2>)")`, also logged; a text message becomes a user
    /// `Content` with one text part), builds the runner (`Err("s.run() failed: failed to create
    /// runner: <err>")`, logged), and answers 200 with `Content-Type: application/json`,
    /// `Cache-Control: no-cache`, `Connection: keep-alive` and a streaming body: per runner
    /// item, log `Processing event: … err: …`; a compaction error is logged `agentengine: <err>`
    /// and skipped; any other error is logged `error in events: <err>\n`, written as one
    /// `emit_json_error` line, and ends the stream; a `None`/content-less event is skipped; an
    /// event is one `emit_json(&event)` line (an emit failure is logged
    /// `helper.EmitJSON() failed: <err>`, written as an error line, and ends the stream).
    pub(crate) async fn stream_jsonl(&self, payload: Bytes) -> Result<Response, String> {
        // try to unmarshal models.StreamQueryRequest first
        let req = match go_unmarshal::<StreamQueryRequest>(
            &payload,
            &encode_shapes::MODELS_STREAM_QUERY_REQUEST,
        ) {
            Ok(req) => req,
            Err(err) => {
                // try to unmarshal models.StreamQueryTextRequest
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
                            "json.Unmarshal() failed both for models.StreamQueryRequest ({err}) and models.StreamQueryTextRequest ({err_text})"
                        );
                        self.log.print(&err);
                        return Err(err);
                    }
                }
            }
        };

        // A deep message is cloned into the run and dropped with `req`, both recursively.
        let deep = needs_deep_stack(&payload);
        let events = run_request(deep, req, |req| self.run(req)).map_err(|err| {
            let err = format!("s.run() failed: {err}");
            self.log.print(&err);
            err
        })?;

        Ok(ndjson_response(events, self.log.clone(), |event, log| {
            emit_json(event, log)
        }))
    }

    /// Go `run`: a runner (`app_name` = the agent engine ID, the loader's root agent, the config's
    /// services, plugins, compaction and platform, `auto_create_session: true`) run for
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

/// `run(&req)`, then `req` dropped: when `deep` (its payload was nested past what the caller's
/// stack clones and drops safely, [`needs_deep_stack`]) both happen on [`on_deep_stack`] (no Go
/// counterpart: Go's stacks grow; that thread being unavailable is the run's error).
///
/// On the deep path `Runner::new` and `runner.run` run on that scoped OS thread, not the
/// caller's tokio task: whatever they read from the current thread (no tokio runtime handle,
/// `instrument_invocation`'s `in_workflow_scope()`, `tracing::Span::current()` parenting) sees
/// the scoped thread. The returned stream is polled on the caller's task as usual.
pub(crate) fn run_request<T, F>(deep: bool, req: T, run: F) -> Result<EventStream, String>
where
    T: Send,
    F: FnOnce(&T) -> Result<EventStream, String> + Send,
{
    if !deep {
        return run(&req);
    }
    on_deep_stack(req, |req| run(&req))?
}

/// The `runner.New` call of both stream methods' `run`: `app_name` = the agent engine ID, the
/// loader's root agent, the config's services, plugins, compaction and platform, and
/// `auto_create_session: true`. An error is Go's `failed to create runner: <err>`.
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

/// The response both stream methods answer once the run has started: 200 with Go's three
/// headers (`Content-Type: application/json`, `Cache-Control: no-cache`, `Connection:
/// keep-alive`) and an NDJSON body streamed from `events`, one body chunk per line (Go flushes
/// each line). Go's loop, per runner item ([`step`]):
///
/// - log `Processing event: <event> err: <err>` (Go's `%+v` dumps; ledgered);
/// - a compaction error: log `agentengine: <err>` and go on;
/// - another error: log `error in events: <err>\n`, write one [`emit_json_error`] line, end;
/// - an event without content: skip it;
/// - an event: the line `line(&event, &log)`; an `Err(e)` is logged
///   `helper.EmitJSON() failed: <e>`, written as an error line, and ends the stream.
///
/// An error line that cannot be encoded is logged `helper.EmitJSONError() failed: <err>` and
/// ends the stream without a line.
///
/// Each item is handled, and dropped, on a large stack ([`on_deep_stack`]): its `Debug` dump,
/// encoding and drop recurse once per nesting level, and an event can be as deep as Go's decoder
/// allows (10000 levels, e.g. echoed request content or session state), which Go's growable
/// stacks survive. The item's log texts are collected there and printed on the caller's task, in
/// order. When that thread cannot be spawned (no Go counterpart) the item is leaked; when it
/// panics the item is dropped there; either way the stream ends with the error line
/// `helper.EmitJSON() failed: <err>`.
pub(crate) fn ndjson_response<F>(events: EventStream, log: Log, line: F) -> Response
where
    F: Fn(&Event, &Log) -> Result<Bytes, String> + Send + 'static,
{
    let lines = futures::stream::unfold(Some((events, log, line)), |state| async move {
        let (mut events, log, mut line) = state?;
        loop {
            let item = events.next().await?;
            // The log texts are built on the deep thread (an event's `Debug` dump recurses)
            // and printed here, on the caller's task: the `AgentEngineLog` hook (or `tracing`'s
            // thread-scoped dispatcher) runs where Go's `log.Print` would.
            let buffered: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
            let sink = Arc::clone(&buffered);
            let deep_log = Log(Some(Arc::new(move |m: &str| {
                sink.lock().push(m.to_owned())
            })));
            let handled = on_deep_stack((item, line), |(item, line)| {
                let step = step(item, &line, &deep_log);
                (step, line)
            });
            drop(deep_log);
            for text in std::mem::take(&mut *buffered.lock()) {
                log.print(&text);
            }
            let step = match handled {
                Ok((step, back)) => {
                    line = back;
                    step
                }
                Err(err) => {
                    let e = format!("helper.EmitJSON() failed: {err}");
                    log.print(&e);
                    return error_line(&e, &log).map(|chunk| (Ok(chunk), None));
                }
            };
            return match step {
                Step::Skip => continue,
                Step::Line(chunk) => {
                    Some((Ok::<Bytes, Infallible>(chunk), Some((events, log, line))))
                }
                Step::Last(chunk) => chunk.map(|chunk| (Ok(chunk), None)),
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

/// What Go's loop does with one runner item.
enum Step {
    /// `continue`.
    Skip,
    /// A line, then the next item.
    Line(Bytes),
    /// The last line (`None`: none could be encoded), then `break`.
    Last(Option<Bytes>),
}

/// One iteration of Go's loop over `item` (see [`ndjson_response`]); `item` is dropped here.
fn step<F>(item: Result<Event, gadk_agent::AgentError>, line: &F, log: &Log) -> Step
where
    F: Fn(&Event, &Log) -> Result<Bytes, String>,
{
    log.print(&processing_event(&item));
    let event = match item {
        // A compaction failure is bookkeeping, not the turn (Go's comment): log, go on.
        Err(err) if err.is_compaction() => {
            log.print(&format!("agentengine: {err}"));
            return Step::Skip;
        }
        Err(err) => {
            log.print(&format!("error in events: {err}\n"));
            return Step::Last(error_line(&err.to_string(), log));
        }
        Ok(event) => event,
    };
    if event.response.content.is_none() {
        return Step::Skip;
    }
    match line(&event, log) {
        Ok(chunk) => Step::Line(chunk),
        Err(err) => {
            let e = format!("helper.EmitJSON() failed: {err}");
            log.print(&e);
            Step::Last(error_line(&e, log))
        }
    }
}

/// Go `helper.EmitJSONError(rw, err)` in the stream loop: the line, or `None` after logging
/// `helper.EmitJSONError() failed: <err>`.
fn error_line(text: &str, log: &Log) -> Option<Bytes> {
    match emit_json_error(text, log) {
        Ok(chunk) => Some(chunk),
        Err(e) => {
            log.print(&format!("helper.EmitJSONError() failed: {e}"));
            None
        }
    }
}

/// Go `log.Printf("Processing event: %+v err: %+v\n", event, err)`: the event as Rust `Debug`
/// and the error as its text, `<nil>` for the absent one (Go's `%+v` struct dump is a ledgered
/// divergence; the replay fixtures drop these lines).
fn processing_event(item: &Result<Event, gadk_agent::AgentError>) -> String {
    match item {
        Ok(event) => format!("Processing event: {event:?} err: <nil>\n"),
        Err(err) => format!("Processing event: <nil> err: {err}\n"),
    }
}

/// Go `Metadata()`'s `description`.
const DESCRIPTION: &str = "Streams responses asynchronously from the ADK application.\nArgs:\n    message (genai.Content):\n        Required. The message to stream responses for.\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Optional. The ID of the session. If not provided, a new session will be created for the user.\n\nYields:\n    Single lines with JSON encoded event each. Errors are also emitted as JSON.\n\n";

/// Go `Metadata()`'s map for `method_name` / `api_mode` (description text verbatim).
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

    /// Go `Handle`: [`StreamQueryHandler::stream_jsonl`], an error wrapped as
    /// `s.streamJSONL() failed: <err>`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        // stream_jsonl fails only before streaming; the controller answers that with a status.
        self.stream_jsonl(payload)
            .await
            .map_err(|err| format!("s.streamJSONL() failed: {err}"))
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
    use gadk_agent::{AgentError, SingleLoader};
    use gadk_artifact::InMemoryArtifactService;
    use gadk_memory::InMemoryMemoryService;
    use gadk_platform::GoDuration;
    use gadk_session::compaction::CompactionError;
    use gadk_session::{CreateRequest, DeleteRequest, InMemorySessionService, SessionService};
    use http::Method;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::agentengine::new_handler;
    use crate::mux::{Request, WriteDeadline};
    use crate::rest::testutil::wire_scenario::{assistant_agent, fixed_platform};

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
            "{\"error\":\"helper.EmitJSON() failed: failed to encode SSE response chunk: bad\"}\n"
        );
        assert!(lines.lock().unwrap().contains(
            &"helper.EmitJSON() failed: failed to encode SSE response chunk: bad".to_owned()
        ));
    }

    /// A JSON value nested `levels` objects deep, built without recursion.
    fn deep_value(levels: usize) -> Value {
        let mut v = Value::Null;
        for _ in 0..levels {
            let mut m = Map::new();
            m.insert("a".to_owned(), v);
            v = Value::Object(m);
        }
        v
    }

    /// A function response ~10000 levels deep (what Go's decoder accepts).
    fn deep_content() -> Content {
        let mut response = Map::new();
        response.insert("r".to_owned(), deep_value(9_990));
        Content {
            role: "model".into(),
            parts: vec![gadk_genai::Part {
                function_response: Some(gadk_genai::FunctionResponse {
                    name: "f".into(),
                    response,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }
    }

    /// An event whose `custom_metadata` and function response are ~10000 levels deep (what Go's
    /// decoder accepts and an agent can echo back).
    fn deep_event() -> Event {
        let mut e = event(None);
        e.response.content = Some(deep_content());
        e.response
            .custom_metadata
            .insert("deep".to_owned(), deep_value(9_990));
        e
    }

    /// Go grows goroutine stacks, so a ~10000-deep event is one line there; here the encode (and
    /// the event's drop) runs on a large stack, so a 2 MiB caller (a tokio worker's default)
    /// does not abort. Both stream methods' line functions.
    #[test]
    fn deep_event_is_one_line_on_a_small_stack() {
        let worker = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let (log, _) = capture();
                    let resp = ndjson_response(
                        Box::pin(stream::iter(vec![Ok(deep_event())])),
                        log.clone(),
                        emit_json,
                    );
                    let plain = body_text(resp).await;
                    let resp = ndjson_response(
                        Box::pin(stream::iter(vec![Ok(deep_event())])),
                        log,
                        super::super::streaming_agent_run_with_events::envelope_line("s".into()),
                    );
                    let envelope = body_text(resp).await;
                    (plain, envelope)
                })
            })
            .unwrap();
        let (plain, envelope) = worker.join().unwrap();
        for (name, body, prefix) in [
            ("event", &plain, "{\"actions\":"),
            ("envelope", &envelope, "{\"events\":[{\"actions\":"),
        ] {
            assert_eq!(body.matches('\n').count(), 1, "{name}: one line");
            assert!(body.starts_with(prefix), "{name}: {}", &body[..80]);
            assert!(!body.contains("\"error\""), "{name}: an error line");
            assert!(
                body.contains("\"custom_metadata\":{\"deep\":{\"a\":{\"a\":"),
                "{name}: custom_metadata"
            );
            assert!(
                body.contains("\"response\":{\"r\":{\"a\":{\"a\":"),
                "{name}: function response"
            );
            assert!(body.ends_with("}\n"), "{name}");
        }
    }

    /// A deep request (decoded on a large stack by `go_unmarshal`) is cloned into the run and
    /// dropped on a large stack too, so a 2 MiB caller does not abort.
    #[test]
    fn deep_request_is_cloned_and_dropped_on_a_large_stack() {
        let worker = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                let req = deep_content();
                let events = run_request(true, req, |req| {
                    let message = req.clone();
                    Ok(Box::pin(stream::once(async move {
                        let mut e = event(None);
                        e.response.content = Some(message);
                        Ok(e)
                    })) as EventStream)
                });
                // The stream (holding the clone) is dropped with the run, on the large stack.
                run_request(true, events, |_| {
                    Ok(Box::pin(stream::empty()) as EventStream)
                })
                .is_ok()
            })
            .unwrap();
        assert!(worker.join().unwrap());
    }

    /// When the large-stack thread cannot be spawned (no Go counterpart), the item is lost and
    /// the stream ends with an error line instead of encoding on the caller's stack.
    #[tokio::test]
    async fn deep_stack_unavailable_is_an_error_line() {
        let (log, lines) = capture();
        let items: Vec<Result<Event, AgentError>> =
            vec![Ok(event(Some("one"))), Ok(event(Some("two")))];
        crate::agentengine::unmarshal::FAIL_DEEP_SPAWN.with(|fail| fail.set(true));
        let resp = ndjson_response(Box::pin(stream::iter(items)), log, |e, log| {
            emit_json(e, log)
        });
        let want = "helper.EmitJSON() failed: agentengine: deep-stack thread unavailable: injected spawn failure";
        assert_eq!(body_text(resp).await, format!("{{\"error\":\"{want}\"}}\n"));
        assert!(lines.lock().unwrap().contains(&want.to_owned()));
    }

    /// The `AgentEngineLog` hook runs on the caller's task (as Go's `log.Print` does), not on
    /// the deep-stack thread: a hook that needs the tokio runtime gets every line, in order, and
    /// the stream writes Go's normal lines.
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
            "Processing event: <nil> err: context compaction failed: squeeze\n"
        );
        assert_eq!(logged[2], "agentengine: context compaction failed: squeeze");
        assert!(logged[3].starts_with("Processing event: Event {"));
    }

    /// A panic on the deep-stack thread (no Go counterpart) drops the item there and ends the
    /// stream with an error line distinct from the spawn failure's.
    #[tokio::test]
    async fn deep_stack_panic_is_an_error_line() {
        let (log, lines) = capture();
        let items: Vec<Result<Event, AgentError>> =
            vec![Ok(event(Some("one"))), Ok(event(Some("two")))];
        let resp = ndjson_response(Box::pin(stream::iter(items)), log, |_, _| {
            panic!("line panicked")
        });
        let want =
            "helper.EmitJSON() failed: agentengine: deep-stack thread failed: the thread panicked";
        assert_eq!(body_text(resp).await, format!("{{\"error\":\"{want}\"}}\n"));
        let logged = lines.lock().unwrap().clone();
        assert!(
            logged[0].starts_with("Processing event: Event {"),
            "{logged:?}"
        );
        assert!(logged.contains(&want.to_owned()));
    }

    /// The Go wire fixtures of the stream routes (`testdata/agentengine/main_09`–`main_18`,
    /// C.11) replayed through `new_handler` with the probe's scenario: status, headers (without
    /// `Date`, `Content-Length`, `Transfer-Encoding`), body and log (without `Processing
    /// event: ` lines) byte for byte. `main_02`–`main_08` (the session methods) are replayed as
    /// the session-service calls they make, so the clock and UUID sequences match.
    #[tokio::test]
    async fn replays_the_stream_fixtures() {
        let platform = fixed_platform();
        let session_service: Arc<dyn SessionService> =
            Arc::new(InMemorySessionService::with_platform(platform.clone()));
        let mut cfg = AgentEngineConfig::new(
            Arc::clone(&session_service),
            Arc::new(SingleLoader::new(assistant_agent())),
        );
        cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
        cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::with_platform(
            platform.clone(),
        )));
        cfg.platform = platform;
        let (log, lines) = capture();
        cfg.log = log.0.clone();
        let handler = new_handler(cfg, GoDuration(0), 4096, "test-engine").unwrap();

        // main_02..main_08: create u1, create u1 with state, get, get missing, list, delete
        // main_03's session, delete missing (only the creates read the clock and UUIDs).
        let create = |state: Option<Map<String, Value>>| CreateRequest {
            app_name: "test-engine".into(),
            user_id: "u1".into(),
            session_id: None,
            state,
        };
        session_service.create(create(None)).await.unwrap();
        let state = serde_json::json!({"k": "v", "n": 1});
        let second = session_service
            .create(create(state.as_object().cloned()))
            .await
            .unwrap();
        session_service
            .delete(DeleteRequest {
                app_name: "test-engine".into(),
                user_id: "u1".into(),
                session_id: second.id().to_owned(),
            })
            .await
            .unwrap();

        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/agentengine");
        let names = [
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
        ];
        for name in names {
            let text = std::fs::read_to_string(format!("{dir}/{name}.json")).unwrap();
            let fx: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(fx["compare"], "exact", "{name}");
            let rq = &fx["request"];
            let mut req = Request::new(axum::body::Body::from(
                rq["body"].as_str().unwrap().to_owned(),
            ));
            *req.method_mut() =
                Method::from_bytes(rq["method"].as_str().unwrap().as_bytes()).unwrap();
            *req.uri_mut() = rq["target"].as_str().unwrap().parse().unwrap();
            for h in rq["headers"].as_array().unwrap() {
                req.headers_mut().append(
                    http::HeaderName::from_bytes(h[0].as_str().unwrap().as_bytes()).unwrap(),
                    HeaderValue::from_str(h[1].as_str().unwrap()).unwrap(),
                );
            }
            req.extensions_mut().insert(WriteDeadline::new(|_| {}));
            lines.lock().unwrap().clear();

            let resp = handler(req).await;

            let want = &fx["response"];
            assert_eq!(
                u64::from(resp.status().as_u16()),
                want["status"].as_u64().unwrap(),
                "{name}: status"
            );
            let mut got_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .filter(|(k, _)| {
                    !matches!(k.as_str(), "date" | "content-length" | "transfer-encoding")
                })
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
                .collect();
            got_headers.sort();
            let want_headers: Vec<(String, String)> = want["headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    (
                        h[0].as_str().unwrap().to_ascii_lowercase(),
                        h[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(got_headers, want_headers, "{name}: headers");
            assert_eq!(
                body_text(resp).await,
                want["body"].as_str().unwrap(),
                "{name}: body"
            );
            let got_log: String = lines
                .lock()
                .unwrap()
                .iter()
                .map(|l| {
                    if l.ends_with('\n') {
                        l.clone()
                    } else {
                        format!("{l}\n")
                    }
                })
                .filter(|l| !l.starts_with("Processing event: "))
                .collect();
            assert_eq!(got_log, fx["log"].as_str().unwrap(), "{name}: log");
        }
    }
}
