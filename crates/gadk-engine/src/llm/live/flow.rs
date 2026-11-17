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

//! The live (bidi) flow, `run_live`. The
//! session task preprocesses the request, connects, and runs the connect/reconnect loop; each
//! connection has a reader task and a sender task feeding one consumer loop.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gadk_agent::fiber::{self, LIVE_SESSION_CREATOR};
use gadk_agent::{
    AgentError, EventStream, InvocationContext, LiveRequest, LiveRunConfig, LiveSession, Tool,
};
use gadk_genai::{LiveConnectConfig, SessionResumptionConfig};
use gadk_model::{
    Backend, LiveConnError, LiveConnection, Llm, LlmRequest, LlmResponse, ModelError,
    RealtimeInput, is_gemini_3x_live,
};
use gadk_session::Event;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::llm::flow::{Flow, find_long_running_function_call_ids};
use crate::llm::functions::handle_function_calls_live;
use crate::llm::live::reconnect::{live_reconnect_gave_up_error, wait_before_reconnect};
use crate::llm::live::session::LiveSessionImpl;
use crate::tool::utils::function_calls;

/// Error text when the model has no live connection support.
pub(crate) const ERR_MODEL_DOES_NOT_SUPPORT_LIVE: &str = "model does not support live connection";
/// Error text when the run config has no live settings.
pub(crate) const ERR_LIVE_RUN_CONFIG_NOT_FOUND: &str = "live run config not found";
/// The function whose response ends the live flow (injected by SequentialAgent::run_live).
const TASK_COMPLETED: &str = "task_completed";
/// Grace period before ending on task_completed.
const TASK_COMPLETED_GRACE: Duration = Duration::from_millis(100);
/// Upper bound on closing a connection during cleanup (a wedged write cannot stall teardown).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// What a connection's reader and sender report to the consumer loop (events and errors).
/// `Event` is boxed (clippy::large_enum_variant), as runner/mod.rs `Processed::Event(Box<Event>)` does.
enum ConnMsg {
    Event(Box<Event>),
    /// The failure and when it was produced.
    Error(LiveConnError, Instant),
}

/// How the consumer loop for one connection ended.
enum Outcome {
    /// The session is over (error reported, closed, cancelled, or task_completed).
    Stop,
    /// A resumable error: dial again.
    Reconnect,
}

/// "failed to connect live session: <cause>" for the first dial.
#[derive(Debug)]
struct LiveConnectFailed(ModelError);

impl fmt::Display for LiveConnectFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to connect live session: {}", self.0)
    }
}

impl Error for LiveConnectFailed {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

fn conn_err(err: LiveConnError) -> AgentError {
    AgentError::Other(Box::new(err))
}

/// Closes the session, and once the session task ends no streaming tool outlives
/// the flow. A drop guard rather than trailing code, so it also runs if the task is dropped
/// unfinished (a runtime shutting down). A tool task still running on another worker can
/// register a streaming tool after the drain; `done` is cancelled before the drain so that
/// `start_live_streaming`, which checks it after registering, cancels such a late tool.
/// Equivalent to `LiveSession::close`, which is async only for the trait.
struct Teardown(Arc<LiveSessionImpl>);

impl Drop for Teardown {
    fn drop(&mut self) {
        self.0.done().cancel();
        self.0.cancel_every_streaming_tool();
        self.0.probe().teardowns.fetch_add(1, Ordering::SeqCst);
    }
}

/// Starts the live flow. Returns at once; the work runs on a spawned session task, so this
/// must be called inside a tokio runtime. Dropping the returned stream cancels the session.
pub(crate) fn run_live(
    flow: Arc<Flow>,
    ctx: InvocationContext,
) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
    // The model must be a live-capable model before anything else.
    let Some(model) = flow.model.clone() else {
        return Err(AgentError::other(ERR_MODEL_DOES_NOT_SUPPORT_LIVE));
    };
    let Some(live_cfg) = ctx.run_config().live.clone() else {
        return Err(AgentError::other(ERR_LIVE_RUN_CONFIG_NOT_FOUND));
    };

    let sess = LiveSessionImpl::new();
    let guard = sess.probe().task_guard();
    let task_sess = sess.clone();
    // The session work runs on a new task: a fresh fiber
    // started by this spawn site, child of the caller's.
    let task = tokio::spawn(fiber::spawn_child(LIVE_SESSION_CREATOR).scope(async move {
        let _guard = guard;
        // Declared after `_guard`, so it runs first; see `Teardown`.
        let _teardown = Teardown(task_sess.clone());
        session_task(flow, model, ctx, live_cfg, task_sess).await;
    }));
    let stream = sess.event_stream(Some(task));
    let sess: Arc<dyn LiveSession> = sess;
    Ok((sess, stream))
}

/// The session task: preprocesses the request, connects, and runs the connect/reconnect loop.
async fn session_task(
    flow: Arc<Flow>,
    model: Arc<dyn Llm>,
    ctx: InvocationContext,
    live_cfg: LiveRunConfig,
    sess: Arc<LiveSessionImpl>,
) {
    let mut req = LlmRequest {
        model: model.name().to_owned(),
        ..Default::default()
    };
    {
        let mut pre = flow.preprocess(&ctx, &mut req);
        while let Some(item) = pre.next().await {
            match item {
                Err(e) => {
                    sess.push_error(e).await;
                    return;
                }
                Ok(ev) => {
                    if !sess.push_event(ev).await {
                        return;
                    }
                }
            }
        }
    }

    // Exactly these fields are copied from the run config.
    let gen_cfg = req.config.clone().unwrap_or_default();
    let mut connect_cfg = LiveConnectConfig {
        response_modalities: live_cfg.response_modalities.clone(),
        speech_config: live_cfg.speech_config.clone(),
        system_instruction: gen_cfg.system_instruction,
        tools: gen_cfg.tools,
        session_resumption: live_cfg.session_resumption.clone(),
        input_audio_transcription: live_cfg.input_audio_transcription.clone(),
        output_audio_transcription: live_cfg.output_audio_transcription.clone(),
        realtime_input_config: live_cfg.realtime_input_config.clone(),
        ..Default::default()
    };
    // Affective dialog and proactivity stay unset for the models
    // `is_gemini_3x_live` matches (affective dialog is not supported on them; proactive audio is
    // always on or not supported). Live Translate is excluded by it and still gets both.
    if !is_gemini_3x_live(model.name()) {
        if live_cfg.enable_affective_dialog {
            connect_cfg.enable_affective_dialog = Some(true);
        }
        connect_cfg.proactivity = live_cfg.proactivity.clone();
    }

    let policy = flow.live_reconnect.clone().unwrap_or_default();
    let ctx_token = ctx.cancellation_token().clone();
    // reconnect_attempts counts consecutive reconnects since a connection last worked (content,
    // or outliving healthy_uptime) and drives the backoff; short_lived counts only connections
    // that died inside healthy_uptime and never resets.
    let mut reconnect_attempts = 0usize;
    let mut short_lived = 0usize;
    let mut current_backoff = policy.initial_backoff;
    let mut last_conn_was_brief = false;
    let mut last_err: Option<Box<dyn Error + Send + Sync>> = None;
    // False only for the very first dial: a first-ever connect failure is fatal, a failed redial
    // is charged to the budget.
    let mut is_reconnect = false;

    loop {
        if is_reconnect {
            reconnect_attempts += 1;
            if last_conn_was_brief {
                short_lived += 1;
            }
            if reconnect_attempts > policy.max_attempts {
                // Resumable errors are swallowed while retrying, so without this the caller's
                // stream just goes quiet.
                sess.push_error(live_reconnect_gave_up_error(
                    format!(
                        "{} consecutive attempts delivered no content",
                        policy.max_attempts
                    ),
                    last_err.take(),
                ))
                .await;
                return;
            }
            if short_lived > policy.max_total {
                sess.push_error(live_reconnect_gave_up_error(
                    format!(
                        "{} short-lived connections in one invocation",
                        policy.max_total
                    ),
                    last_err.take(),
                ))
                .await;
                return;
            }
            let sleep = policy.jittered(current_backoff);
            current_backoff = policy.next_backoff(current_backoff);
            tracing::warn!(
                attempt = reconnect_attempts,
                max_attempts = policy.max_attempts,
                short_lived,
                delay = ?sleep,
                "live session: reconnect attempt"
            );
            if !wait_before_reconnect(&ctx_token, &sess, sleep).await {
                // Cancellation reports itself; close() is a clean teardown.
                if ctx_token.is_cancelled() {
                    sess.push_error(AgentError::Cancelled).await;
                }
                return;
            }
        }

        let handle = ctx.live_session_resumption_handle();
        if !handle.is_empty() {
            let sr = connect_cfg
                .session_resumption
                .get_or_insert_with(SessionResumptionConfig::default);
            sr.handle = handle;
            if model.google_variant() == Some(Backend::VertexAi) {
                sr.transparent = true;
            }
        }
        if let Some(sr) = &connect_cfg.session_resumption {
            tracing::debug!(handle = %sr.handle, "connecting with live session handle");
        }

        // The connection's cancellation token.
        let conn_token = ctx_token.child_token();
        let connected = tokio::select! {
            r = model.connect_live(connect_cfg.clone()) => r,
            _ = sess.done().cancelled() => return,
            _ = ctx_token.cancelled() => {
                sess.push_error(AgentError::Cancelled).await;
                return;
            }
        };
        let conn: Arc<dyn LiveConnection> = match connected {
            Ok(c) => Arc::from(c),
            Err(err) => {
                conn_token.cancel();
                tracing::warn!(error = %err, "failed to connect live session");
                if is_reconnect {
                    last_err = Some(Box::new(err));
                    last_conn_was_brief = true;
                    continue;
                }
                sess.push_error(AgentError::Other(Box::new(LiveConnectFailed(err))))
                    .await;
                return;
            }
        };
        let connected_at = Instant::now();

        // Send the preprocessed contents as history, if any.
        if !req.contents.is_empty()
            && let Err(e) = conn.send_history(&req.contents).await
        {
            tracing::warn!(error = %e, "failed to send history");
            // Close the connection before reporting.
            conn_token.cancel();
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, conn.close()).await;
            sess.push_error(conn_err(e)).await;
            return;
        }

        // A function response awaiting delivery to the model, queued on the
        // session's input channel so the sender task is the connection's only writer (a write
        // from this loop could overtake user input queued there). While one is pending, the
        // events that arrive are held in order; errors are handled at once.
        let mut pending_send: Option<LiveRequest> = None;
        let mut held: VecDeque<Box<Event>> = VecDeque::new();

        let (tx, mut rx) = mpsc::channel::<ConnMsg>(1);
        let mut tasks = JoinSet::new();
        tasks.spawn(reader(
            conn.clone(),
            conn_token.clone(),
            tx.clone(),
            ctx.clone(),
            sess.clone(),
            live_cfg.save_live_blob,
        ));
        tasks.spawn(sender(
            conn.clone(),
            conn_token.clone(),
            tx,
            ctx.clone(),
            sess.clone(),
        ));

        let outcome = 'consume: loop {
            let held_ev = if pending_send.is_none() {
                held.pop_front()
            } else {
                None
            };
            let msg = match held_ev {
                Some(ev) => Some(ConnMsg::Event(ev)),
                None => tokio::select! {
                    queued = queue_pending(&sess, pending_send.clone()) => {
                        if !queued {
                            break 'consume Outcome::Stop;
                        }
                        pending_send = None;
                        continue 'consume;
                    }
                    m = rx.recv() => m,
                    _ = sess.done().cancelled() => break 'consume Outcome::Stop,
                    _ = ctx_token.cancelled() => {
                        sess.push_error(AgentError::Cancelled).await;
                        break 'consume Outcome::Stop;
                    }
                },
            };
            match msg {
                Some(ConnMsg::Event(ev)) if pending_send.is_some() => held.push_back(ev),
                Some(ConnMsg::Event(ev)) => {
                    let mut ev = *ev;
                    let has_calls = !function_calls(ev.response.content.as_ref()).is_empty();
                    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
                    if has_calls {
                        for t in flow.tools.read().iter() {
                            tools.insert(t.name().to_owned(), t.clone());
                        }
                        ev.long_running_tool_ids = find_long_running_function_call_ids(
                            ev.response.content.as_ref(),
                            &tools,
                        );
                    }
                    let has_content = ev.response.content.is_some();
                    let interrupted = ev.response.interrupted;
                    let turn_complete = ev.response.turn_complete;
                    let call_ev = has_calls.then(|| ev.clone());
                    if !sess.push_event(ev).await {
                        break 'consume Outcome::Stop;
                    }
                    // Content proves the connection is serving: the consecutive budget starts
                    // over (short_lived does not).
                    if has_content {
                        reconnect_attempts = 0;
                        current_backoff = policy.initial_backoff;
                    }
                    // flush_caches is snapshot-then-drain, so it must have one caller at a time:
                    // this consumer loop is the only one.
                    if live_cfg.save_live_blob {
                        let flush_model = interrupted || turn_complete;
                        let flush_user = turn_complete;
                        if flush_user || flush_model {
                            match sess
                                .audio()
                                .flush_caches(&ctx, flush_user, flush_model)
                                .await
                            {
                                Err(e) => {
                                    sess.push_error(e).await;
                                    break 'consume Outcome::Stop;
                                }
                                Ok(flushed) => {
                                    for fev in flushed {
                                        if !sess.push_event(fev).await {
                                            break 'consume Outcome::Stop;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(call_ev) = call_ev {
                        // Once the session is closed, start no tool: a streaming tool registered
                        // now could miss close()'s cancellation. Every later push would be
                        // refused anyway.
                        if sess.done().is_cancelled() {
                            break 'consume Outcome::Stop;
                        }
                        let live: Arc<dyn LiveSession> = sess.clone();
                        match handle_function_calls_live(&ctx, &flow, &tools, &call_ev, live).await
                        {
                            Err(e) => {
                                sess.push_error(e).await;
                                break 'consume Outcome::Stop;
                            }
                            Ok(None) => {}
                            Ok(Some(resp_ev)) => {
                                let content = resp_ev.response.content.clone();
                                if !sess.push_event(resp_ev).await {
                                    break 'consume Outcome::Stop;
                                }
                                let task_completed = content.as_ref().is_some_and(|c| {
                                    c.parts.iter().any(|p| {
                                        p.function_response
                                            .as_ref()
                                            .is_some_and(|fr| fr.name == TASK_COMPLETED)
                                    })
                                });
                                if task_completed {
                                    tokio::time::sleep(TASK_COMPLETED_GRACE).await;
                                    break 'consume Outcome::Stop;
                                }
                                // Queue the function response for the sender task, the
                                // connection's only writer.
                                if let Some(c) = content {
                                    pending_send = Some(LiveRequest {
                                        content: Some(c),
                                        realtime_input: None,
                                    });
                                }
                            }
                        }
                    }
                }
                Some(ConnMsg::Error(err, at)) => {
                    if err.is_resumable() {
                        tracing::warn!(error = %err, "live connection error, attempting to resume");
                        // Score the connection's life from where the error was produced, not
                        // from here (this loop also runs tools and blocks on the caller).
                        last_conn_was_brief =
                            at.saturating_duration_since(connected_at) < policy.healthy_uptime;
                        if !last_conn_was_brief {
                            // Served as long as a healthy connection: routine cycling spends
                            // neither budget and clears the consecutive one.
                            reconnect_attempts = 0;
                            current_backoff = policy.initial_backoff;
                        }
                        last_err = Some(Box::new(err));
                        break 'consume Outcome::Reconnect;
                    }
                    sess.push_error(conn_err(err)).await;
                    break 'consume Outcome::Stop;
                }
                // Both tasks gone without a report: only after the connection was torn down.
                None => break 'consume Outcome::Stop,
            }
        };

        // Cleanup before ending or reconnecting.
        conn_token.cancel();
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, conn.close()).await;
        tasks.shutdown().await;

        match outcome {
            Outcome::Stop => return,
            Outcome::Reconnect => is_reconnect = true,
        }
    }
}

/// Queues `req` on the session's input channel for the sender task;
/// never completes without a request, so a select branch over it stays idle. `false` once the
/// session is done.
async fn queue_pending(sess: &LiveSessionImpl, req: Option<LiveRequest>) -> bool {
    match req {
        Some(req) => sess.queue_input(req).await,
        None => std::future::pending().await,
    }
}

/// Whether `r` carries nothing worth an event,
/// as the response built from the Live API's setupComplete does. adk-python's live flow skips
/// the same responses.
pub(crate) fn is_empty_live_response(r: &LlmResponse) -> bool {
    r.content.is_none()
        && r.error_code.is_empty()
        && !r.interrupted
        && !r.turn_complete
        && r.input_transcription.is_none()
        && r.output_transcription.is_none()
        && r.usage_metadata.is_none()
        && r.session_resumption_handle.is_empty()
        && r.grounding_metadata.is_none()
}

/// Reports `err` to the consumer, stamped now; gives up when the connection is torn down.
async fn report(token: &CancellationToken, tx: &mpsc::Sender<ConnMsg>, err: LiveConnError) {
    let at = Instant::now();
    tokio::select! {
        _ = token.cancelled() => {}
        _ = tx.send(ConnMsg::Error(err, at)) => {}
    }
}

/// The reader task: reads responses from the model connection.
async fn reader(
    conn: Arc<dyn LiveConnection>,
    token: CancellationToken,
    tx: mpsc::Sender<ConnMsg>,
    ctx: InvocationContext,
    sess: Arc<LiveSessionImpl>,
    save_live_blob: bool,
) {
    let _guard = sess.probe().task_guard();
    loop {
        let res = tokio::select! {
            _ = token.cancelled() => return,
            r = conn.recv() => r,
        };
        let resp = match res {
            Ok(Some(r)) => r,
            // The stream ended without a close frame: end of stream.
            Ok(None) => {
                report(&token, &tx, LiveConnError::eof()).await;
                return;
            }
            Err(e) => {
                report(&token, &tx, e).await;
                return;
            }
        };
        if !resp.session_resumption_handle.is_empty() {
            tracing::debug!(handle = %resp.session_resumption_handle, "received session resumption handle");
            ctx.set_live_session_resumption_handle(&resp.session_resumption_handle);
        }
        if save_live_blob && let Some(content) = &resp.content {
            for part in &content.parts {
                if let Some(blob) = &part.inline_data {
                    sess.audio()
                        .cache_output(ctx.platform(), &blob.data, &blob.mime_type);
                }
            }
        }
        if is_empty_live_response(&resp) {
            continue;
        }
        // Author and response only (no branch).
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.author = ctx.agent().name().to_owned();
        ev.response = resp;
        tokio::select! {
            _ = token.cancelled() => return,
            r = tx.send(ConnMsg::Event(Box::new(ev))) => if r.is_err() { return; },
        }
    }
}

/// The sender task: sends requests to the model connection.
///
/// `next_input` acknowledges a request when it takes it, so a caller that abandons its
/// `LiveSession::send` future after the request was queued (for example a streaming tool whose
/// token was cancelled mid-send) still has it delivered by whichever connection's sender takes it.
/// A taken request is lost only if its write fails.
async fn sender(
    conn: Arc<dyn LiveConnection>,
    token: CancellationToken,
    tx: mpsc::Sender<ConnMsg>,
    ctx: InvocationContext,
    sess: Arc<LiveSessionImpl>,
) {
    let _guard = sess.probe().task_guard();
    while let Some(req) = sess.next_input(&token).await {
        if let Some(content) = req.content {
            let sent = tokio::select! {
                _ = token.cancelled() => return,
                r = conn.send_content(content) => r,
            };
            if let Err(e) = sent {
                report(&token, &tx, e).await;
                return;
            }
        }
        if let Some(input) = req.realtime_input {
            // Input audio is always cached.
            if let RealtimeInput::Blob(blob) = &input {
                sess.audio()
                    .cache_input(ctx.platform(), &blob.data, &blob.mime_type);
            }
            let sent = tokio::select! {
                _ = token.cancelled() => return,
                r = conn.send_realtime(input) => r,
            };
            if let Err(e) = sent {
                report(&token, &tx, e).await;
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::sync::{Mutex, OnceLock};

    use futures::stream::BoxStream;
    use gadk_agent::{Context, StreamingTool, ToolError};
    use serde_json::Value;
    use tokio::sync::oneshot;

    use super::*;
    use crate::llm::flow::RequestProcessor;
    use crate::llm::live::test_server::{
        FakeConn, FakeLiveServer, assert_no_run_live_leak, live_config_processor, live_ctx,
        probe_of, serve,
    };

    const NEVER_CALL: &str =
        r#"{"toolCall":{"functionCalls":[{"id":"never_1","name":"never_yields","args":{}}]}}"#;

    /// A streaming tool that never yields. Its first `as_streaming` (made by the tool-call task,
    /// right before `start_live_streaming` registers it, with no await in between) reports that
    /// the tool is starting, then blocks its worker until the event stream has been dropped
    /// (`done` cancelled): the tool registers after the session was closed. The session task is
    /// not aborted (Go's goroutine finishes the call), so it is its teardown that must cancel it.
    struct NeverYields {
        dropped: OnceLock<CancellationToken>,
        entered: Mutex<Option<oneshot::Sender<()>>>,
    }

    impl Tool for NeverYields {
        fn name(&self) -> &str {
            "never_yields"
        }
        fn description(&self) -> &str {
            "never yields"
        }
        fn as_streaming(&self) -> Option<&dyn StreamingTool> {
            if let Some(tx) = self.entered.lock().unwrap().take() {
                let _ = tx.send(());
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if self.dropped.get().is_some_and(|d| d.is_cancelled()) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Some(self)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl StreamingTool for NeverYields {
        fn run_stream(
            &self,
            _ctx: Context,
            _args: Value,
        ) -> BoxStream<'static, Result<String, ToolError>> {
            Box::pin(futures::stream::pending())
        }
    }

    /// R9 on the drop path: dropping the event stream while a streaming tool is starting must
    /// not leave the tool's task running, even for a direct `run_live` caller whose invocation is
    /// never cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dropping_the_stream_while_a_streaming_tool_starts_does_not_leak_it() {
        let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
            assert!(conn.write_text(NEVER_CALL).await, "writing toolCall failed");
            conn.block_until_client_closes().await;
        }))
        .await;
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let tool = Arc::new(NeverYields {
            dropped: OnceLock::new(),
            entered: Mutex::new(Some(entered_tx)),
        });
        let flow = Arc::new(Flow {
            model: Some(server.model().await),
            request_processors: vec![live_config_processor as RequestProcessor],
            tools: parking_lot::RwLock::new(vec![tool.clone() as Arc<dyn Tool>]),
            ..Default::default()
        });
        // The invocation is deliberately never cancelled (no `cancel_on_drop` safety net).
        let (ctx, _cancel) = live_ctx();
        let (sess, mut stream) = run_live(flow, ctx).unwrap();
        let probe = probe_of(&sess);
        let done = sess
            .as_any()
            .downcast_ref::<LiveSessionImpl>()
            .unwrap()
            .done()
            .clone();
        tool.dropped.set(done).unwrap();

        // Consume events (pushes are rendezvous) until the tool is starting.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    _ = &mut entered_rx => return,
                    item = stream.next() => assert!(item.is_some(), "stream ended before the tool call"),
                }
            }
        })
        .await
        .expect("the streaming tool never started");
        drop(stream);

        assert_no_run_live_leak(&probe).await;
        assert_eq!(probe.teardowns.load(Ordering::SeqCst), 1);
        assert_eq!(
            sess.as_any()
                .downcast_ref::<LiveSessionImpl>()
                .unwrap()
                .active_streaming_tool_count("never_yields"),
            0,
            "the late registration is cancelled, then unregistered by its task"
        );
    }
}
