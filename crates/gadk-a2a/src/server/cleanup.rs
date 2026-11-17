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

//! The cleanup guard around the execute and cancel streams: runs [`Executor`]'s cleanup
//! exactly once.
//!
//! The cleanup is due after an execution or cancellation finished; a2a-rs's
//! `AgentExecutor` has no such hook, and its `DefaultRequestHandler` stops polling the stream
//! after the first terminal event and drops it. The guard therefore starts the cleanup on the
//! first of: a terminal event, an error, the end of the stream, or the stream being dropped.
//! The cleanup runs as a task spawned on the current Tokio runtime, so it may start before
//! a2a-rs stores the terminal event; it reads only the snapshot folded here
//! and the ADK session. An execute stream that a cancel stopped ends without a terminal event
//! of its own; its snapshot gets the cancel's `canceled` update, as an execution result would.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::BoxStream;
use futures::{Stream, StreamExt as _};

use super::executor::{ExecutionRegistration, Executor};
use crate::Error;

/// Wraps an execute or cancel stream, folds the yielded events into a task snapshot and runs
/// the executor's cleanup on the first terminal event, error, end of stream or drop.
pub(crate) struct CleanupGuard {
    inner: BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>,
    /// `None` once the cleanup was started.
    state: Option<CleanupState>,
}

/// What the cleanup needs: the request and the task folded from the yielded events.
struct CleanupState {
    executor: Executor,
    ctx: a2a_server::ExecutorContext,
    /// The task snapshot, starting from the stored task.
    task: Option<a2a::Task>,
    /// Whether any event was yielded (the result is `None` otherwise).
    yielded: bool,
    /// An execute stream's registration for cancellation, released with the state.
    execution: Option<ExecutionRegistration>,
}

impl CleanupGuard {
    /// Guards `inner`, an execute or cancel stream for `ctx`.
    pub(crate) fn new(
        executor: Executor,
        ctx: a2a_server::ExecutorContext,
        inner: BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>,
    ) -> Self {
        let task = ctx.stored_task.clone();
        Self {
            inner,
            state: Some(CleanupState {
                executor,
                ctx,
                task,
                yielded: false,
                execution: None,
            }),
        }
    }

    /// Ties the execute stream's cancellation `registration` to this guard: it stays
    /// registered until the cleanup starts, and a cancel's `canceled` update is folded into
    /// the result.
    pub(crate) fn with_execution(mut self, registration: ExecutionRegistration) -> Self {
        if let Some(state) = self.state.as_mut() {
            state.execution = Some(registration);
        }
        self
    }

    /// Starts the cleanup unless it already started.
    fn finish(&mut self, cause: Option<Error>) {
        let Some(mut state) = self.state.take() else {
            return;
        };
        if let Some(canceled) = state
            .execution
            .take()
            .and_then(|registration| registration.canceled_event())
        {
            state.fold(&canceled);
        }
        let result = if state.yielded { state.task } else { None };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let CleanupState { executor, ctx, .. } = state;
                handle.spawn(async move { executor.cleanup(ctx, result, cause).await });
            }
            Err(_) => tracing::warn!("a2a executor cleanup skipped: no Tokio runtime"),
        }
    }
}

impl CleanupState {
    /// Applies `event` to the snapshot the way a2a-rs applies it to the stored task.
    fn fold(&mut self, event: &a2a::StreamResponse) {
        self.yielded = true;
        match event {
            a2a::StreamResponse::Task(task) => self.task = Some(task.clone()),
            a2a::StreamResponse::StatusUpdate(update) => {
                let task = self
                    .task
                    .get_or_insert_with(|| empty_task(&update.task_id, &update.context_id));
                task.status = update.status.clone();
            }
            a2a::StreamResponse::ArtifactUpdate(update) => {
                let task = self
                    .task
                    .get_or_insert_with(|| empty_task(&update.task_id, &update.context_id));
                let artifacts = task.artifacts.get_or_insert_with(Vec::new);
                let existing = artifacts
                    .iter_mut()
                    .find(|a| a.artifact_id == update.artifact.artifact_id);
                match existing {
                    Some(artifact) if update.append.unwrap_or(false) => {
                        artifact.parts.extend(update.artifact.parts.iter().cloned());
                        if let Some(meta) = &update.artifact.metadata {
                            artifact
                                .metadata
                                .get_or_insert_with(HashMap::new)
                                .extend(meta.clone());
                        }
                    }
                    Some(artifact) => *artifact = update.artifact.clone(),
                    None => artifacts.push(update.artifact.clone()),
                }
            }
            a2a::StreamResponse::Message(_) => {}
        }
    }
}

/// A task known only by its IDs.
fn empty_task(task_id: &str, context_id: &str) -> a2a::Task {
    a2a::Task {
        id: task_id.to_owned(),
        context_id: context_id.to_owned(),
        status: a2a::TaskStatus {
            state: a2a::TaskState::Unspecified,
            message: None,
            timestamp: None,
        },
        artifacts: None,
        history: None,
        metadata: None,
    }
}

/// a2a-rs's terminal events: a task or status update in a terminal state.
fn is_terminal(event: &a2a::StreamResponse) -> bool {
    match event {
        a2a::StreamResponse::Task(task) => task.status.state.is_terminal(),
        a2a::StreamResponse::StatusUpdate(update) => update.status.state.is_terminal(),
        a2a::StreamResponse::Message(_) | a2a::StreamResponse::ArtifactUpdate(_) => false,
    }
}

impl Stream for CleanupGuard {
    type Item = Result<a2a::StreamResponse, a2a::A2AError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let item = match this.inner.poll_next_unpin(cx) {
            Poll::Ready(item) => item,
            Poll::Pending => return Poll::Pending,
        };
        match &item {
            Some(Ok(event)) => {
                if let Some(state) = this.state.as_mut() {
                    state.fold(event);
                }
                if is_terminal(event) {
                    this.finish(None);
                }
            }
            Some(Err(err)) => this.finish(Some(Error::A2A(err.clone()))),
            None => this.finish(None),
        }
        Poll::Ready(item)
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.finish(None);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use a2a_server::AgentExecutor as _;
    use gadk_agent::{AgentConfig, new_agent};
    use gadk_genai::{Content, FunctionCall, Part};
    use gadk_model::LlmResponse;
    use gadk_session::{Event, InMemorySessionService, SessionService};
    use tokio::sync::mpsc;

    use super::*;
    use crate::server::processor::{TaskInfo, new_status_update};
    use crate::server::{ExecutorConfig, RunnerConfig};
    use crate::testutil::ScriptedRunner;

    /// What one cleanup saw: the folded task's state and the cause.
    type Seen = (Option<a2a::TaskState>, Option<String>);

    /// An executor whose runner yields `events` and whose cleanup callback reports to the
    /// returned channel.
    fn executor(events: Vec<Event>) -> (Executor, mpsc::UnboundedReceiver<Seen>) {
        let agent = new_agent(AgentConfig {
            name: "agent".into(),
            ..Default::default()
        })
        .unwrap();
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let executor = Executor::new(ExecutorConfig {
            runner_provider: Some(Arc::new(move |_ctx, _plugin| {
                let cfg = RunnerConfig {
                    app_name: "app".into(),
                    agent: Arc::clone(&agent),
                    session_service: Arc::clone(&sessions),
                };
                let runner: Arc<dyn crate::server::Runner> = Arc::new(ScriptedRunner::new(
                    events.iter().cloned().map(Ok).collect(),
                ));
                Box::pin(async move { Ok((cfg, runner)) })
            })),
            a2a_execution_cleanup_callback: Some(Arc::new(move |_ctx, _cards, result, cause| {
                let _ = tx.send((
                    result.map(|t| t.status.state.clone()),
                    cause.map(ToString::to_string),
                ));
                Box::pin(async {})
            })),
            ..Default::default()
        });
        (executor, rx)
    }

    fn request(message: bool) -> a2a_server::ExecutorContext {
        a2a_server::ExecutorContext {
            message: message
                .then(|| a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("hi")])),
            task_id: "task-1".into(),
            stored_task: None,
            context_id: "ctx-1".into(),
            metadata: None,
            user: None,
            service_params: HashMap::new(),
            tenant: None,
        }
    }

    fn update(state: a2a::TaskState) -> Result<a2a::StreamResponse, a2a::A2AError> {
        let task = TaskInfo {
            task_id: "task-1".into(),
            context_id: "ctx-1".into(),
        };
        Ok(a2a::StreamResponse::StatusUpdate(new_status_update(
            &task, state, None,
        )))
    }

    /// Waits for one cleanup, then checks no second one follows.
    async fn expect_once(rx: &mut mpsc::UnboundedReceiver<Seen>) -> Seen {
        let seen = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the cleanup did not run")
            .unwrap();
        let again = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
        // A closed channel (the executor is gone) is no second cleanup either.
        assert!(
            !matches!(again, Ok(Some(_))),
            "the cleanup ran again: {again:?}"
        );
        seen
    }

    #[tokio::test]
    async fn cleanup_runs_once_on_the_terminal_event() {
        let (executor, mut rx) = executor(vec![]);
        let inner = futures::stream::iter([
            update(a2a::TaskState::Working),
            update(a2a::TaskState::Completed),
            update(a2a::TaskState::Working),
        ]);
        let mut guard = CleanupGuard::new(executor, request(true), Box::pin(inner));

        assert!(guard.next().await.unwrap().is_ok());
        assert!(guard.next().await.unwrap().is_ok());
        // Started by the terminal event, before the stream is read further.
        let seen = expect_once(&mut rx).await;
        assert_eq!(seen, (Some(a2a::TaskState::Completed), None));

        assert!(guard.next().await.unwrap().is_ok());
        assert!(guard.next().await.is_none());
        drop(guard);
        let again = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
        // A closed channel (the executor is gone) is no second cleanup either.
        assert!(
            !matches!(again, Ok(Some(_))),
            "the cleanup ran again: {again:?}"
        );
    }

    #[tokio::test]
    async fn cleanup_runs_once_on_an_error() {
        let (executor, mut rx) = executor(vec![]);
        let inner = futures::stream::iter([
            update(a2a::TaskState::Working),
            Err(a2a::A2AError::internal("boom")),
            update(a2a::TaskState::Completed),
        ]);
        let got: Vec<_> = CleanupGuard::new(executor, request(true), Box::pin(inner))
            .collect()
            .await;
        assert_eq!(got.len(), 3);
        let (state, cause) = expect_once(&mut rx).await;
        assert_eq!(state, Some(a2a::TaskState::Working));
        assert!(cause.unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn cleanup_runs_once_at_the_end_of_the_stream() {
        // A long-running call ends the execution input-required: not terminal, so the end of the
        // stream starts the cleanup.
        let call = Event {
            response: LlmResponse {
                content: Some(Content {
                    role: "model".into(),
                    parts: vec![Part {
                        function_call: Some(FunctionCall {
                            id: "call-1".into(),
                            name: "approve".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            },
            long_running_tool_ids: vec!["call-1".into()],
            ..Default::default()
        };
        let (executor, mut rx) = executor(vec![call]);
        let got: Vec<_> = executor.execute(request(true)).collect().await;
        let Some(Ok(a2a::StreamResponse::StatusUpdate(last))) = got.last() else {
            panic!("want a final status update, got {got:?}");
        };
        assert_eq!(last.status.state, a2a::TaskState::InputRequired);
        let seen = expect_once(&mut rx).await;
        assert_eq!(seen, (Some(a2a::TaskState::InputRequired), None));
    }

    #[tokio::test]
    async fn cleanup_runs_once_when_dropped_mid_stream() {
        let (executor, mut rx) = executor(vec![]);
        let mut stream = executor.execute(request(true));
        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(first, a2a::StreamResponse::Task(_)));
        let second = stream.next().await.unwrap().unwrap();
        assert!(matches!(second, a2a::StreamResponse::StatusUpdate(_)));
        drop(stream);
        let seen = expect_once(&mut rx).await;
        assert_eq!(seen, (Some(a2a::TaskState::Working), None));
    }

    #[tokio::test]
    async fn cleanup_without_a_yielded_event_has_no_result() {
        let (executor, mut rx) = executor(vec![]);
        let got: Vec<_> = executor.execute(request(false)).collect().await;
        assert_eq!(got.len(), 1);
        let seen = expect_once(&mut rx).await;
        assert_eq!(seen, (None, Some("message not provided".into())));
    }

    /// A runner whose run never yields (Go: an agent blocked in a model or tool call).
    struct BlockedRunner;

    impl crate::server::Runner for BlockedRunner {
        fn run(
            &self,
            _user_id: &str,
            _session_id: &str,
            _msg: Content,
            _cfg: gadk_agent::RunConfig,
        ) -> gadk_agent::EventStream {
            Box::pin(futures::stream::pending())
        }
    }

    #[tokio::test]
    async fn a_cancel_stops_the_running_execution_and_both_cleanups_see_it_canceled() {
        let (executor, mut rx) = executor(vec![]);
        let after_execute = Arc::new(AtomicUsize::new(0));
        let executor = Executor::new(ExecutorConfig {
            runner_provider: Some({
                let provider = executor.config.runner_provider.clone().unwrap();
                Arc::new(move |ctx, plugin| {
                    let provided = provider(ctx, plugin);
                    Box::pin(async move {
                        let (cfg, _) = provided.await?;
                        let runner: Arc<dyn crate::server::Runner> = Arc::new(BlockedRunner);
                        Ok((cfg, runner))
                    })
                })
            }),
            a2a_execution_cleanup_callback: executor.config.a2a_execution_cleanup_callback.clone(),
            // The run is dropped, not cancelled: Go calls it with the failed "agent run failed:
            // context canceled" status, gadk does not call it (a declared divergence).
            after_execute_callback: Some({
                let after_execute = Arc::clone(&after_execute);
                Arc::new(move |_ctx, _status, _err| {
                    after_execute.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(()) })
                })
            }),
            ..Default::default()
        });
        let mut execution = executor.execute(request(true));
        assert!(matches!(
            execution.next().await.unwrap().unwrap(),
            a2a::StreamResponse::Task(_)
        ));
        assert!(matches!(
            execution.next().await.unwrap().unwrap(),
            a2a::StreamResponse::StatusUpdate(_)
        ));

        let canceled: Vec<_> = executor.cancel(request(false)).collect().await;
        assert_eq!(canceled.len(), 1);
        // The blocked run is dropped at once: the execute stream ends.
        let end = tokio::time::timeout(Duration::from_secs(1), execution.next())
            .await
            .expect("the execution was not stopped");
        assert!(end.is_none(), "want the end of the stream, got {end:?}");

        let mut seen = Vec::new();
        for _ in 0..2 {
            seen.push(
                tokio::time::timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .expect("a cleanup did not run")
                    .unwrap(),
            );
        }
        assert_eq!(seen, vec![(Some(a2a::TaskState::Canceled), None); 2]);
        assert!(
            executor.executions_is_empty(),
            "the execution stayed registered"
        );
        assert_eq!(
            after_execute.load(Ordering::SeqCst),
            0,
            "after_execute_callback ran for a stopped execution"
        );
    }

    #[test]
    fn cleanup_is_skipped_without_a_runtime() {
        let (executor, _rx) = executor(vec![]);
        let inner = futures::stream::iter([update(a2a::TaskState::Working)]);
        // Dropped outside any Tokio runtime: logged and skipped, not a panic.
        drop(CleanupGuard::new(executor, request(true), Box::pin(inner)));
    }
}
