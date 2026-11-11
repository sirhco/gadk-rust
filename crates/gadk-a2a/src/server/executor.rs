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

//! The A2A agent executor (port of ADK-Go `server/adka2a/v2/executor.go`). Bodies: plan 5b
//! Task 7.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_agent::{Agent, EventStream, Plugin, RunConfig};
use gadk_genai::Content;
use gadk_session::{Event, SessionService};

use super::context::ExecutorContext;
use crate::Error;
use crate::convert::{A2APartConverter, GenAIPartConverter};

/// Go `OutputMode`: how the executor turns ADK events into artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Go `OutputArtifactPerRun` (`"artifact-per-run"`): one artifact per runner run.
    #[default]
    ArtifactPerRun,
    /// Go `OutputArtifactPerEvent` (`"artifact-per-event"`): one artifact per non-partial event.
    ArtifactPerEvent,
}

/// Go `Runner`: the runner API the executor drives (`gadk_engine::runner::Runner` or a custom
/// implementation).
pub trait Runner: Send + Sync {
    /// Go `Run`: runs the agent for `msg`, yielding its events.
    fn run(&self, user_id: &str, session_id: &str, msg: Content, cfg: RunConfig) -> EventStream;
}

/// Go `RunnerConfig`: the part of the runner configuration the executor depends on.
#[derive(Clone)]
pub struct RunnerConfig {
    /// The app name, used in session keys and A2A event metadata.
    pub app_name: String,
    /// The root agent.
    pub agent: Arc<dyn Agent>,
    /// The session service.
    pub session_service: Arc<dyn SessionService>,
}

/// Go `RunnerProvider`: builds the runner for one request; it must install `plugin` in the
/// returned runner.
pub type RunnerProvider = Arc<
    dyn Fn(
            &a2a_server::ExecutorContext,
            Plugin,
        ) -> BoxFuture<'static, Result<(RunnerConfig, Arc<dyn Runner>), Error>>
        + Send
        + Sync,
>;

/// Go `BeforeExecuteCallback`: runs before an execution starts; values it stores in the
/// extensions are visible to [`AfterEventCallback`], [`AfterExecuteCallback`] and
/// [`ExecutorContext::extensions`] (Go's returned `context.Context`). An error stops the
/// execution.
pub type BeforeExecuteCallback = Arc<
    dyn Fn(
            &a2a_server::ExecutorContext,
            &mut http::Extensions,
        ) -> BoxFuture<'static, Result<(), Error>>
        + Send
        + Sync,
>;

/// Go `AfterEventCallback`: runs after an ADK event was converted to an A2A artifact update.
pub type AfterEventCallback = Arc<
    dyn Fn(
            &dyn ExecutorContext,
            &Event,
            &mut a2a::TaskArtifactUpdateEvent,
        ) -> BoxFuture<'static, Result<(), Error>>
        + Send
        + Sync,
>;

/// Go `AfterExecuteCallback`: runs after an execution resolved into its final status update.
pub type AfterExecuteCallback = Arc<
    dyn Fn(
            &dyn ExecutorContext,
            &mut a2a::TaskStatusUpdateEvent,
            Option<&Error>,
        ) -> BoxFuture<'static, Result<(), Error>>
        + Send
        + Sync,
>;

/// Go `A2AExecutionCleanupCallback`: runs once after an execution or cancellation completed or
/// failed, with the remote sub-agents' cards; `result` is the folded task (`None` if no event was
/// yielded).
pub type A2AExecutionCleanupCallback = Arc<
    dyn Fn(
            &a2a_server::ExecutorContext,
            Vec<a2a::AgentCard>,
            Option<&a2a::Task>,
            Option<&Error>,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Resolves the ADK user ID of a request (`None` or an empty string falls back to
/// `"A2A_USER_" + context_id`).
pub type UserResolver = Arc<dyn Fn(&a2a_server::ExecutorContext) -> Option<String> + Send + Sync>;

/// Go `ExecutorConfig`, plus [`ExecutorConfig::user_resolver`].
#[derive(Clone, Default)]
pub struct ExecutorConfig {
    /// Builds the default runner provider; ignored when `runner_provider` is set.
    pub runner_config: Option<gadk_engine::runner::RunnerConfig>,
    /// Builds the runner for each request; the default calls `gadk_engine::runner::Runner::new`
    /// with `runner_config` and the executor's plugin.
    pub runner_provider: Option<RunnerProvider>,
    /// Passed to the runner on every execution.
    pub run_config: RunConfig,
    /// Go `BeforeExecuteCallback`.
    pub before_execute_callback: Option<BeforeExecuteCallback>,
    /// Go `AfterEventCallback`.
    pub after_event_callback: Option<AfterEventCallback>,
    /// Go `AfterExecuteCallback`.
    pub after_execute_callback: Option<AfterExecuteCallback>,
    /// Go `A2APartConverter`.
    pub a2a_part_converter: Option<A2APartConverter>,
    /// Go `GenAIPartConverter`.
    pub genai_part_converter: Option<GenAIPartConverter>,
    /// Go `OutputMode`.
    pub output_mode: OutputMode,
    /// Go `A2AExecutionCleanupCallback`; without it, failures are logged.
    pub a2a_execution_cleanup_callback: Option<A2AExecutionCleanupCallback>,
    /// The ADK user of a request; `None` uses
    /// [`default_user_resolver`](crate::server::default_user_resolver).
    pub user_resolver: Option<UserResolver>,
}

/// Go `Executor`: an `a2a_server::AgentExecutor` that runs a gadk agent.
#[derive(Clone)]
pub struct Executor {
    pub(crate) config: Arc<ExecutorConfig>,
}

impl Executor {
    /// Go `NewExecutor`.
    pub fn new(config: ExecutorConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl a2a_server::AgentExecutor for Executor {
    /// Go `Execute`, wrapped in the cleanup guard.
    fn execute(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> {
        todo!("plan 5b Task 7")
    }

    /// Go `Cancel`, wrapped in the cleanup guard.
    fn cancel(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> {
        todo!("plan 5b Task 7")
    }
}
