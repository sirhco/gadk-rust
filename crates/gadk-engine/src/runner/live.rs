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

//! Live mode of the runner (port of ADK-Go `runner/runner.go` lines 831-1149 and
//! `hasInlineData`): [`Runner::run_live`], `runnerLiveSession` and `closedLiveSession`.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::fiber::FiberStream;
use gadk_agent::{
    AgentError, EventStream, InvocationContext, LiveRequest, LiveRunConfig, LiveSession, RunConfig,
    StreamingMode,
};
use gadk_model::LlmResponse;
use gadk_session::{Event, Session};
use tokio_util::sync::CancellationToken;

use super::compaction::InvocationGuard;
use super::{
    ERR_USER_FUNCTION_CALL, RunOptions, Runner, RunnerInner, from_plugin, has_function_call,
    stamp_timestamp, wrap,
};

impl Runner {
    /// Port of Go `(*Runner).RunLive`: starts a live (bidirectional streaming) run of the agent
    /// that should handle the session's next turn. Returns the session to send input on and the
    /// event stream. Dropping the stream at a yield runs the after-run callbacks first, on the
    /// still-live invocation context, and then cancels the invocation; dropped inside an await,
    /// it cancels at once. The live session task is detached rather than aborted, so a tool call
    /// already running finishes on its own (the after-run callbacks do not wait for it, as Go's
    /// live tool goroutines are not awaited either).
    ///
    /// Must be called inside a Tokio runtime: the live flow spawns tasks. `opts` is accepted for
    /// symmetry with [`Runner::run`] and, as in Go, not applied.
    ///
    /// The returned stream runs on the consumer's fiber ([`FiberStream::consumer`]), as Go's
    /// iterator runs on the goroutine that ranges over it; this function's own work (session
    /// lookup, before-run callbacks, starting the live flow) runs in the caller's future, as Go's
    /// `RunLive` runs on the caller's goroutine.
    pub async fn run_live(
        &self,
        user_id: &str,
        session_id: &str,
        cfg: LiveRunConfig,
        opts: RunOptions,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
        // Go parses its RunOptions and never reads them in RunLive.
        let _ = opts;
        let this = self.inner.clone();
        let stored = this.get_or_create_session(user_id, session_id).await?;
        let agent_to_run = this.find_agent_to_run(&*stored, None)?;
        let Some(live) = agent_to_run.as_live() else {
            return Err(AgentError::other(format!(
                "agent {} does not support Live Run",
                agent_to_run.name()
            )));
        };

        // Live is always bidirectional streaming (Go runconfig.ToContext with Bidi + Live).
        let run_cfg = RunConfig {
            streaming_mode: StreamingMode::Bidi,
            live: Some(cfg),
            ..Default::default()
        };
        let token = CancellationToken::new();
        // Go RunLive attaches no compaction runtime (runner.go:928).
        let ictx = this.new_invocation_context(
            &stored,
            agent_to_run.clone(),
            None,
            &run_cfg,
            &token,
            false,
            // Go `r.withPluginManager(ctx)`: `run_live` takes no caller context, so there is no
            // inheritance mark.
            this.with_plugin_manager(None),
            // Go RunLive: NewInvocationContext without an invocation ID or branch.
            String::new(),
            String::new(),
        );

        // Go #2105: the before-run reply goes through on_event (and is stamped and persisted) as
        // on Runner::run, and the run through the after-run callbacks.
        if let Some(event) = this.run_before_run_callback(&ictx).await? {
            let stream = FiberStream::consumer(early_exit_stream(
                this.clone(),
                ictx.clone(),
                stored.clone(),
                event,
                token,
            ))
            .boxed();
            return Ok((Arc::new(ClosedLiveSession), stream));
        }

        let (agent_sess, inner) = live.run_live(ictx.clone()).await?;
        let stream = FiberStream::consumer(wrap_live_stream(
            this.clone(),
            ictx.clone(),
            stored.clone(),
            inner,
            token,
        ))
        .boxed();
        let session: Arc<dyn LiveSession> = Arc::new(RunnerLiveSession {
            sess: agent_sess,
            runner: this,
            ctx: ictx,
            stored,
        });
        Ok((session, stream))
    }
}

/// Go `RunLive`'s `earlyExitIter` (#2105): yields the before-run reply, then runs the after-run
/// callbacks (deferred: also when the consumer drops the stream at the yield).
fn early_exit_stream(
    this: Arc<RunnerInner>,
    ctx: InvocationContext,
    stored: Arc<dyn Session>,
    event: Event,
    token: CancellationToken,
) -> EventStream {
    let cancel_on_drop = token.drop_guard();
    Box::pin(async_stream::stream! {
        let mut guard = InvocationGuard::new(this, stored, ctx);
        guard.cancel_on_drop(cancel_on_drop);
        guard.no_compaction();
        guard.defer_after_run();
        guard.at_yield(true);
        yield Ok(event);
        guard.at_yield(false);
        guard.finish().await;
    })
}

/// Go `RunLive`'s `wrappedIter`: plugin `on_event`, chronological buffering of tool calls and
/// responses while a transcription is in progress, persistence of non-partial events without
/// inline data, and the EOF flush of buffered events.
///
/// An [`InvocationGuard`] without compaction (Go `RunLive` does not compact) owns the deferred
/// after-run callbacks and the invocation token's cancel-on-drop: a consumer that drops the
/// stream at a yield (Go `break`) gets the after-run callbacks on its fiber before the token is
/// cancelled; a drop inside an await (Go's caller cancelling) cancels at once.
fn wrap_live_stream(
    this: Arc<RunnerInner>,
    ctx: InvocationContext,
    stored: Arc<dyn Session>,
    mut inner: EventStream,
    token: CancellationToken,
) -> EventStream {
    // Dropping the stream cancels the invocation, which stops the live flow. The drop guard is
    // made here, not on first poll, so a stream dropped before it is ever polled cancels too.
    let cancel_on_drop = token.drop_guard();
    Box::pin(async_stream::stream! {
        // Made at the first poll, inside the consumer's fiber, which its `Drop` re-enters.
        let mut guard = InvocationGuard::new(this.clone(), stored.clone(), ctx.clone());
        guard.cancel_on_drop(cancel_on_drop);
        guard.no_compaction();
        // Go `defer pluginManager.RunAfterRunCallback(iCtx)` inside the iterator: runs once when
        // the stream ends, or (spawned) when the consumer drops it early.
        guard.defer_after_run();
        let mut buffered: Vec<Event> = Vec::new();
        let mut is_transcribing = false;

        while let Some(item) = inner.next().await {
            let mut event = match item {
                Ok(ev) => ev,
                Err(e) => {
                    guard.at_yield(true);
                    yield Err(e);
                    guard.at_yield(false);
                    continue;
                }
            };
            stamp_timestamp(&ctx, &mut event);

            if !event.response.partial
                && event.node_info.as_ref().is_some_and(|n| n.message_as_output)
                && event.response.content.is_some()
            {
                event.output = None;
            }

            let record = event.actions.compaction.clone();
            match this.plugin_manager.run_on_event(ctx.clone(), event.clone()).await {
                Err(e) => {
                    guard.at_yield(true);
                    yield Err(e);
                    guard.at_yield(false);
                    continue;
                }
                Ok(modified) => event = from_plugin(event, modified, record),
            }
            // A plugin may have handed back an event without a timestamp.
            stamp_timestamp(&ctx, &mut event);

            let has_transcription = event.response.input_transcription.is_some()
                || event.response.output_transcription.is_some();
            if event.response.partial && has_transcription {
                is_transcribing = true;
            }

            // Tool calls and responses that arrive mid-transcription wait for it, so the
            // session records them after the transcription they interrupted.
            if is_transcribing && is_tool_call_or_response(&event) {
                buffered.push(event);
                continue;
            }

            if !event.response.partial && has_transcription {
                is_transcribing = false;
                if let Err(e) = this.session_service.append_event(&*stored, &mut event).await {
                    guard.at_yield(true);
                    yield Err(wrap("failed to add event to session", e));
                    guard.at_yield(false);
                    continue;
                }
                guard.at_yield(true);
                yield Ok(event);
                guard.at_yield(false);
                for mut buffered_event in std::mem::take(&mut buffered) {
                    if let Err(e) = this
                        .session_service
                        .append_event(&*stored, &mut buffered_event)
                        .await
                    {
                        guard.at_yield(true);
                        yield Err(wrap("failed to add event to session", e));
                        guard.at_yield(false);
                        continue;
                    }
                    guard.at_yield(true);
                    yield Ok(buffered_event);
                    guard.at_yield(false);
                }
                continue;
            }

            if !event.response.partial
                && !has_inline_data(&event)
                && let Err(e) = this.session_service.append_event(&*stored, &mut event).await
            {
                guard.at_yield(true);
                yield Err(wrap("failed to add event to session", e));
                guard.at_yield(false);
                continue;
            }
            guard.at_yield(true);
            yield Ok(event);
            guard.at_yield(false);
        }

        // EOF: flush what is still buffered. Go uses context.WithoutCancel here; a Rust append
        // takes no context, so a cancelled invocation cannot cut the flush short. Reaching this
        // point means the consumer is still reading: a dropped stream never resumes.
        for mut buffered_event in std::mem::take(&mut buffered) {
            if let Err(e) = this
                .session_service
                .append_event(&*stored, &mut buffered_event)
                .await
            {
                guard.at_yield(true);
                yield Err(wrap("failed to add event to session", e));
                guard.at_yield(false);
                continue;
            }
            guard.at_yield(true);
            yield Ok(buffered_event);
            guard.at_yield(false);
        }

        guard.finish().await;
    })
}

/// Whether the event's content has a function call or a function response part.
fn is_tool_call_or_response(event: &Event) -> bool {
    event.response.content.as_ref().is_some_and(|c| {
        c.parts
            .iter()
            .any(|p| p.function_call.is_some() || p.function_response.is_some())
    })
}

/// Port of Go `hasInlineData`.
fn has_inline_data(event: &Event) -> bool {
    event
        .response
        .content
        .as_ref()
        .is_some_and(|c| c.parts.iter().any(|p| p.inline_data.is_some()))
}

/// Port of Go `runnerLiveSession`: forwards to the agent's live session and persists the user's
/// non-function-response content.
pub(crate) struct RunnerLiveSession {
    sess: Arc<dyn LiveSession>,
    runner: Arc<RunnerInner>,
    ctx: InvocationContext,
    stored: Arc<dyn Session>,
}

#[async_trait]
impl LiveSession for RunnerLiveSession {
    /// Go drops nil parts before forwarding; a Rust `Part` cannot be absent, so the request is
    /// forwarded unchanged.
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        // Go #2302: refused before anything is forwarded or stored.
        if has_function_call(req.content.as_ref()) {
            return Err(AgentError::other(ERR_USER_FUNCTION_CALL));
        }
        let content = req.content.clone();
        self.sess.send(req).await?;

        if let Some(content) = content
            && !content.parts.is_empty()
            && !content.parts.iter().any(|p| p.function_response.is_some())
        {
            let mut event = Event::new(self.ctx.platform(), self.ctx.invocation_id());
            event.author = "user".to_string();
            event.response = LlmResponse {
                content: Some(content),
                ..Default::default()
            };
            self.runner
                .session_service
                .append_event(&*self.stored, &mut event)
                .await
                .map_err(|e| wrap("failed to add user event to session", e))?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), AgentError> {
        self.sess.close().await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go `closedLiveSession`: the session returned by a before-run early exit.
pub(crate) struct ClosedLiveSession;

#[async_trait]
impl LiveSession for ClosedLiveSession {
    async fn send(&self, _req: LiveRequest) -> Result<(), AgentError> {
        Err(AgentError::other("session is closed"))
    }

    async fn close(&self) -> Result<(), AgentError> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use gadk_agent::{AgentConfig, Plugin};
    use gadk_genai::Content;
    use gadk_platform::Platform;
    use gadk_session::InMemorySessionService;

    use super::*;
    use crate::runner::{PluginConfig, RunnerConfig};

    /// R12: an agent without live support fails before any plugin runs.
    #[tokio::test]
    async fn run_live_rejects_agent_without_live_support_before_plugins() {
        let before_run = Arc::new(AtomicBool::new(false));
        let flag = before_run.clone();
        let plugin = Plugin::new(gadk_agent::PluginConfig {
            name: "p".into(),
            before_run_callback: Some(Arc::new(move |_ctx| {
                flag.store(true, Ordering::SeqCst);
                Box::pin(async { Ok::<Option<Content>, AgentError>(None) })
            })),
            ..Default::default()
        });
        let agent = gadk_agent::agent::new(AgentConfig {
            name: "plain".into(),
            ..Default::default()
        })
        .unwrap();
        let runner = Runner::new(RunnerConfig {
            app_name: "app".into(),
            agent,
            session_service: Arc::new(InMemorySessionService::new()),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig {
                plugins: vec![plugin],
                ..Default::default()
            },
            auto_create_session: true,
            platform: Platform::default(),
            compaction: None,
        })
        .unwrap();
        let err = runner
            .run_live("u", "s", LiveRunConfig::default(), RunOptions::default())
            .await
            .err()
            .expect("run_live must fail");
        assert_eq!(err.to_string(), "agent plain does not support Live Run");
        assert!(
            !before_run.load(Ordering::SeqCst),
            "no plugin may run first"
        );
    }

    #[tokio::test]
    async fn closed_live_session_rejects_send() {
        let err = ClosedLiveSession
            .send(LiveRequest::default())
            .await
            .expect_err("send must fail");
        assert_eq!(err.to_string(), "session is closed");
        ClosedLiveSession.close().await.unwrap();
    }

    /// Records the invocation's cancellation token and yields nothing.
    struct TokenCapture(Arc<parking_lot::Mutex<Option<CancellationToken>>>);

    #[async_trait]
    impl gadk_agent::LiveAgent for TokenCapture {
        async fn run_live(
            &self,
            ctx: InvocationContext,
        ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
            *self.0.lock() = Some(ctx.cancellation_token().clone());
            Ok((
                Arc::new(ClosedLiveSession),
                futures::stream::pending().boxed(),
            ))
        }
    }

    /// Dropping the returned stream cancels the invocation, even if it was never polled.
    #[tokio::test]
    async fn dropping_unpolled_stream_cancels_invocation() {
        let captured = Arc::new(parking_lot::Mutex::new(None));
        let agent = gadk_agent::new_with_state(
            AgentConfig {
                name: "live".into(),
                ..Default::default()
            },
            gadk_agent::AgentState {
                agent_type: gadk_agent::AgentType::CustomAgent,
                config: None,
                on_before_agent_callback_event: None,
                live: Some(Arc::new(TokenCapture(captured.clone()))),
            },
        )
        .unwrap();
        let runner = Runner::new(RunnerConfig {
            app_name: "app".into(),
            agent,
            session_service: Arc::new(InMemorySessionService::new()),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: true,
            platform: Platform::default(),
            compaction: None,
        })
        .unwrap();
        let (_sess, stream) = runner
            .run_live("u", "s", LiveRunConfig::default(), RunOptions::default())
            .await
            .unwrap();
        let token = captured.lock().clone().expect("run_live was called");
        assert!(!token.is_cancelled());
        drop(stream);
        assert!(token.is_cancelled(), "dropping the stream must cancel");
    }
}
