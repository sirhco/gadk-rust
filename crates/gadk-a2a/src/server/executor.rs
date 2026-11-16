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

//! The A2A agent executor (port of ADK-Go `server/adka2a/v2/executor.go` and the
//! `toInvocationMeta` helper of `metadata.go`).

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(feature = "client")]
use std::time::Duration;

use futures::StreamExt as _;
use futures::future::{AbortHandle, Abortable, BoxFuture};
use futures::stream::BoxStream;
use gadk_agent::{Agent, EventStream, Plugin, RunConfig};
use gadk_genai::Content;
use gadk_session::{CreateRequest, Event, GetRequest, SessionService};
use serde_json::Value;

use super::cleanup::CleanupGuard;
use super::context::{ExecutorContext, ExecutorContextImpl, InvocationMeta, clone_request};
use super::identity::default_user_resolver;
use super::input_required::handle_input_required;
#[cfg(feature = "client")]
use super::input_required::{get_pending_long_running_call_ids, get_subagent_tasks_to_cancel};
use super::plugin::ExecutorPlugin;
use super::processor::{
    EventProcessor, TaskInfo, new_status_update, non_empty, to_task_failed_update_event,
};
use super::task_artifact::{ArtifactMaker, EventToArtifactTransform, LegacyArtifactMaker};
use crate::Error;
use crate::convert::{
    A2APartConverter, GenAIPartConverter, add_adk_extension_meta, to_a2a_meta_key, to_genai_content,
};
#[cfg(feature = "client")]
use crate::remoteagent::auth::{CallIdentity, CredentialCell, credential_scope, with_auth_scope};
#[cfg(feature = "client")]
use crate::remoteagent::config::{create_a2a_client, find_remote_agent_state};
#[cfg(feature = "client")]
use crate::remoteagent::{A2AClient, A2AServerConfig, ClientContext};

/// Bounds each child-task `CancelTask` of the server cleanup (the `CancelTask` call with its
/// credential resolution; creating the client, `create_a2a_client`, is not bounded). Go's server
/// path has no bound (a declared divergence, ledger 5b); the 5 s value is that of Go's
/// remote-agent `cleanupTimeout` (agent/remoteagent/v2/a2a_agent.go), which bounds the remote
/// agent's own cleanup cancel.
#[cfg(feature = "client")]
pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

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
///
/// It translates the runner's ADK events to A2A events by Go's rules:
/// - a request that references no stored task first yields a `submitted` [`a2a::Task`];
/// - right before the runner starts, a `working` status update;
/// - per ADK event, an artifact update with the converted parts (shaped by
///   [`OutputMode`]);
/// - after the last event, the update resetting the partial artifact (if one was made);
/// - finally `failed` (an LLM error response or a run failure), else `input-required` (a
///   long-running function call), else `completed`.
///
/// Every task and status update carries the ADK extension key
/// ([`ADK_EXTENSION_URI`](crate::convert::ADK_EXTENSION_URI)). Go's `Cleanup` runs exactly once
/// per execute or cancel stream: on its terminal event or error, at its end, or when it is
/// dropped. A cancel stops the task's running execution at once (Go cancels its context): the
/// execute stream ends, dropping the runner, and its cleanup sees the `canceled` task.
///
/// Serving an agent over A2A JSON-RPC (Go `ExampleNewExecutor_jsonRPCHandler`): the executor
/// behind a2a-rs's request handler, the JSON-RPC router at `/invoke` and the static card at
/// `/.well-known/agent-card.json`.
///
/// ```
/// use std::sync::Arc;
///
/// use gadk_a2a::a2a_server::{
///     DefaultRequestHandler, InMemoryTaskStore, StaticAgentCard, agent_card::agent_card_router,
///     jsonrpc::jsonrpc_router,
/// };
/// use gadk_a2a::server::{Executor, ExecutorConfig};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Build or load your ADK agent here.
/// let root_agent = gadk_agent::new_agent(gadk_agent::AgentConfig {
///     name: "weather-agent".into(),
///     ..Default::default()
/// })?;
///
/// let agent_card: gadk_a2a::a2a::AgentCard = serde_json::from_value(serde_json::json!({
///     "name": "weather-agent",
///     "supportedInterfaces": [{
///         "url": "http://localhost:8080/invoke",
///         "protocolBinding": "JSONRPC",
///         "protocolVersion": "1.0",
///     }],
///     "version": "1.0.0",
///     "capabilities": {},
///     "defaultInputModes": ["text/plain"],
///     "defaultOutputModes": ["text/plain"],
///     "skills": [],
/// }))?;
///
/// let executor = Executor::new(ExecutorConfig {
///     runner_config: Some(gadk_engine::runner::RunnerConfig {
///         app_name: "weather-agent".into(),
///         agent: root_agent,
///         session_service: Arc::new(gadk_session::InMemorySessionService::new()),
///         artifact_service: None,
///         memory_service: None,
///         plugin_config: Default::default(),
///         auto_create_session: false,
///         platform: Default::default(),
///         compaction: None,
///     }),
///     ..Default::default()
/// });
/// let request_handler = Arc::new(DefaultRequestHandler::new(executor, InMemoryTaskStore::new()));
///
/// let router: axum::Router = agent_card_router(Arc::new(StaticAgentCard::new(agent_card)))
///     .nest("/invoke", jsonrpc_router(request_handler));
/// # let _ = router;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Executor {
    pub(crate) config: Arc<ExecutorConfig>,
    /// The running executions, by task ID, that a cancel stops.
    executions: Executions,
}

/// The running executions, by task ID.
type Executions = Arc<parking_lot::Mutex<HashMap<String, Arc<RunningExecution>>>>;

/// A running execute stream that a cancel of its task stops.
struct RunningExecution {
    /// Ends the execute stream (Go cancels the execution context).
    abort: AbortHandle,
    /// The `canceled` status update the cancel yielded: in Go the cancel event goes through
    /// the execution's own event queue, so the execution's result is the canceled task.
    canceled: parking_lot::Mutex<Option<a2a::StreamResponse>>,
}

/// Keeps an execution registered for cancellation until dropped (by the cleanup guard, when
/// it starts the cleanup).
pub(crate) struct ExecutionRegistration {
    executions: Executions,
    task_id: String,
    execution: Arc<RunningExecution>,
}

impl ExecutionRegistration {
    /// The `canceled` status update of a cancel that stopped this execution.
    pub(crate) fn canceled_event(&self) -> Option<a2a::StreamResponse> {
        self.execution.canceled.lock().clone()
    }
}

impl Drop for ExecutionRegistration {
    fn drop(&mut self) {
        let mut executions = self.executions.lock();
        if executions
            .get(&self.task_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.execution))
        {
            executions.remove(&self.task_id);
        }
    }
}

impl Executor {
    /// Go `NewExecutor`: without a runner provider, the default one builds a
    /// `gadk_engine::runner::Runner` from `runner_config` with the `a2a-executor` plugin.
    pub fn new(mut config: ExecutorConfig) -> Self {
        if config.runner_provider.is_none() {
            config.runner_provider = Some(default_runner_provider(config.runner_config.clone()));
        }
        Self {
            config: Arc::new(config),
            executions: Executions::default(),
        }
    }

    /// Registers the execution of `task_id`, which `abort` stops.
    fn register_execution(&self, task_id: String, abort: AbortHandle) -> ExecutionRegistration {
        let execution = Arc::new(RunningExecution {
            abort,
            canceled: parking_lot::Mutex::new(None),
        });
        self.executions
            .lock()
            .insert(task_id.clone(), Arc::clone(&execution));
        ExecutionRegistration {
            executions: Arc::clone(&self.executions),
            task_id,
            execution,
        }
    }

    /// Whether no execution is registered for cancellation.
    #[cfg(test)]
    pub(crate) fn executions_is_empty(&self) -> bool {
        self.executions.lock().is_empty()
    }

    /// Stops the running execution of `task_id`, if any, recording the `canceled` update as
    /// its result (Go: the canceler writes it to the execution's queue, whose consumer then
    /// cancels the execution context).
    fn stop_execution(&self, task_id: &str, canceled: &a2a::StreamResponse) {
        let execution = self.executions.lock().get(task_id).cloned();
        if let Some(execution) = execution {
            *execution.canceled.lock() = Some(canceled.clone());
            execution.abort.abort();
        }
    }

    fn runner_provider(&self) -> RunnerProvider {
        self.config
            .runner_provider
            .clone()
            .unwrap_or_else(|| default_runner_provider(self.config.runner_config.clone()))
    }

    /// Go `toInvocationMeta`: the user is the non-empty user the resolver returns, else
    /// `"A2A_USER_" + context_id`; the session is the context.
    pub(crate) fn invocation_meta(
        &self,
        cfg: &RunnerConfig,
        ctx: &a2a_server::ExecutorContext,
    ) -> InvocationMeta {
        let resolver = self
            .config
            .user_resolver
            .clone()
            .unwrap_or_else(default_user_resolver);
        let user_id = resolver(ctx)
            .filter(|user| !user.is_empty())
            .unwrap_or_else(|| format!("A2A_USER_{}", ctx.context_id));
        let session_id = ctx.context_id.clone();
        let event_meta = HashMap::from([
            (
                to_a2a_meta_key("app_name"),
                Value::String(cfg.app_name.clone()),
            ),
            (to_a2a_meta_key("user_id"), Value::String(user_id.clone())),
            (
                to_a2a_meta_key("session_id"),
                Value::String(session_id.clone()),
            ),
        ]);
        InvocationMeta {
            user_id,
            session_id,
            agent_name: cfg.agent.name().to_owned(),
            event_meta,
        }
    }

    /// Go `Execute` (without the cleanup guard).
    fn execute_stream(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> {
        let this = self.clone();
        Box::pin(async_stream::stream! {
            let Some(msg) = ctx.message.clone() else {
                yield Err(Error::msg("message not provided").into());
                return;
            };
            let content = match to_genai_content(&msg, this.config.a2a_part_converter.as_ref()).await {
                Ok(content) => content,
                Err(err) => {
                    yield Err(Error::wrap("a2a message conversion failed", err).into());
                    return;
                }
            };

            let executor_plugin = ExecutorPlugin::new();

            let (cfg, runner) = match (this.runner_provider())(&ctx, executor_plugin.plugin.clone()).await {
                Ok(provided) => provided,
                Err(err) => {
                    yield Err(Error::wrap("failed to create a runner", err).into());
                    return;
                }
            };
            // Go's `BeforeExecuteCallback` returns a derived context; here it stores values in
            // the extensions (a nil context, Go's other failure, cannot occur).
            let mut extensions = http::Extensions::new();
            if let Some(callback) = &this.config.before_execute_callback
                && let Err(err) = callback(&ctx, &mut extensions).await
            {
                yield Err(Error::wrap("before execute", err).into());
                return;
            }

            match handle_input_required(&ctx, &content).await {
                Err(err) => {
                    yield Err(err.into());
                    return;
                }
                Ok(Some(event)) => {
                    yield Ok(stamped(a2a::StreamResponse::StatusUpdate(event)));
                    return;
                }
                Ok(None) => {}
            }

            // parity: divergence: a2a-rs's non-streaming send passes the submitted snapshot as
            // `stored_task`, so on that path no `Task` is yielded (the stored task is the same).
            if ctx.stored_task.is_none() {
                yield Ok(stamped(a2a::StreamResponse::Task(new_submitted_task(&ctx, msg))));
            }

            let meta = this.invocation_meta(&cfg, &ctx);
            let task = TaskInfo::of(&ctx);
            let exec_ctx = ExecutorContextImpl::new(meta.clone(), executor_plugin, content, ctx, extensions);

            if let Err(err) = prepare_session(&cfg, &meta).await {
                let status = to_task_failed_update_event(&task, &err.to_string(), non_empty(meta.event_meta.clone()));
                for item in this.write_final_task_status(&exec_ctx, None, status, Some(err)).await {
                    yield item;
                }
                return;
            }

            let mut working = new_status_update(&task, a2a::TaskState::Working, None);
            working.metadata = non_empty(meta.event_meta.clone());
            yield Ok(stamped(a2a::StreamResponse::StatusUpdate(working)));

            let transform: Box<dyn EventToArtifactTransform> = match this.config.output_mode {
                OutputMode::ArtifactPerEvent => Box::new(ArtifactMaker::new(task.clone())),
                OutputMode::ArtifactPerRun => Box::new(LegacyArtifactMaker::new(task.clone())),
            };
            let mut processor = EventProcessor::new(
                task,
                meta,
                this.config.genai_part_converter.clone(),
                transform,
            );

            // Go `process`: processing failures are delivered as failed status updates.
            let mut events = runner.run(
                &processor.meta.user_id,
                &processor.meta.session_id,
                exec_ctx.user_content().clone(),
                this.config.run_config.clone(),
            );
            while let Some(item) = events.next().await {
                let adk_event = match item {
                    Ok(event) => event,
                    // A compaction failure is bookkeeping, not the task. The agent has already
                    // answered and its events are persisted, so failing the task would report
                    // work as lost that the caller has in hand.
                    Err(err) if err.is_compaction() => {
                        tracing::warn!(error = %err, "context compaction failed");
                        continue;
                    }
                    Err(err) => {
                        let cause = Error::Agent(err);
                        let event = processor.make_task_failed_event(&format!("agent run failed: {cause}"), None);
                        let reset = processor.make_final_artifact_update();
                        for item in this.write_final_task_status(&exec_ctx, reset, event, Some(cause)).await {
                            yield item;
                        }
                        return;
                    }
                };

                let mut processed = processor.process(&adk_event).await;
                if let (Ok(Some(update)), Some(callback)) = (&mut processed, &this.config.after_event_callback)
                    && let Err(err) = callback(&exec_ctx, &adk_event, update).await
                {
                    processed = Err(err);
                }

                match processed {
                    Err(err) => {
                        let event = processor.make_task_failed_event(&format!("processor failed: {err}"), Some(&adk_event));
                        let reset = processor.make_final_artifact_update();
                        for item in this.write_final_task_status(&exec_ctx, reset, event, Some(err)).await {
                            yield item;
                        }
                        return;
                    }
                    Ok(Some(update)) => yield Ok(stamped(a2a::StreamResponse::ArtifactUpdate(update))),
                    Ok(None) => {}
                }
            }

            let final_status = processor.make_final_status_update();
            let reset = processor.make_final_artifact_update();
            for item in this.write_final_task_status(&exec_ctx, reset, final_status, None).await {
                yield item;
            }
        })
    }

    /// Go `writeFinalTaskStatus`: runs the after-execute callback, then yields the partial
    /// artifact reset (if any) and the final status update.
    async fn write_final_task_status(
        &self,
        ctx: &dyn ExecutorContext,
        partial_reset: Option<a2a::TaskArtifactUpdateEvent>,
        mut status: a2a::TaskStatusUpdateEvent,
        err: Option<Error>,
    ) -> Vec<Result<a2a::StreamResponse, a2a::A2AError>> {
        if let Some(callback) = &self.config.after_execute_callback
            && let Err(err) = callback(ctx, &mut status, err.as_ref()).await
        {
            return vec![Err(Error::wrap("after execute", err).into())];
        }
        let mut items = Vec::with_capacity(2);
        if let Some(reset) = partial_reset {
            items.push(Ok(stamped(a2a::StreamResponse::ArtifactUpdate(reset))));
        }
        items.push(Ok(stamped(a2a::StreamResponse::StatusUpdate(status))));
        items
    }

    /// Go `Cleanup`: releases what a completed request leaves behind. After a successful cancel
    /// of a task that was input-required, the remote sub-agents' tasks waiting for input are
    /// cancelled; then the cleanup callback runs with the remote sub-agents' cards, or a failure
    /// is logged.
    pub(crate) async fn cleanup(
        &self,
        ctx: a2a_server::ExecutorContext,
        result: Option<a2a::Task>,
        cause: Option<Error>,
    ) {
        let cfg = match self.create_runner_config(&ctx).await {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::error!(error = %err, "failed to create runner config");
                if let Some(callback) = &self.config.a2a_execution_cleanup_callback {
                    callback(&ctx, Vec::new(), result.as_ref(), cause.as_ref()).await;
                }
                return;
            }
        };

        #[cfg(feature = "client")]
        let cards = {
            let remote_subagents = find_remote_subagents(&cfg.agent);

            // If the task was input-required and got successfully canceled - run the cleanup
            // logic.
            if let Some(status) = child_cancel_status(&ctx, result.as_ref())
                && let Err(err) = self
                    .cancel_child_input_required_tasks(&ctx, &status, &cfg, &remote_subagents)
                    .await
            {
                tracing::warn!(cause = %err, "failed to cancel subagent tasks waiting for input");
            }

            // parity: divergence: Go passes a nil card for a sub-agent configured with a card
            // provider; a `Vec<AgentCard>` cannot hold one, so such sub-agents are left out.
            remote_subagents
                .iter()
                .filter_map(|subagent| subagent.config.agent_card.clone())
                .collect::<Vec<_>>()
        };
        #[cfg(not(feature = "client"))]
        let cards: Vec<a2a::AgentCard> = {
            let _ = &cfg;
            Vec::new()
        };

        if let Some(callback) = &self.config.a2a_execution_cleanup_callback {
            callback(&ctx, cards, result.as_ref(), cause.as_ref()).await;
        } else if let Some(cause) = &cause {
            if ctx.message.is_some() {
                tracing::warn!(error = %cause, "execution failed");
            } else {
                tracing::warn!(error = %cause, "cancellation failed");
            }
        }
    }

    /// Go `cancelChildInputRequiredTasks`: cancels the remote tasks behind the pending
    /// long-running calls of `status`, each through its sub-agent's client and inside the
    /// credential scope of the invocation that created it (when the sub-agent owns its auth
    /// scope).
    #[cfg(feature = "client")]
    pub(crate) async fn cancel_child_input_required_tasks(
        &self,
        ctx: &a2a_server::ExecutorContext,
        status: &a2a::TaskStatus,
        cfg: &RunnerConfig,
        subagents: &[RemoteSubagent],
    ) -> Result<(), Error> {
        if subagents.is_empty() {
            return Ok(());
        }

        let meta = self.invocation_meta(cfg, ctx);
        let session = cfg
            .session_service
            .get(GetRequest {
                app_name: cfg.app_name.clone(),
                user_id: meta.user_id.clone(),
                session_id: meta.session_id.clone(),
                ..Default::default()
            })
            .await
            .map_err(|e| Error::wrap("failed to get a session", e))?;

        let tasks_to_cancel = get_subagent_tasks_to_cancel(status, &*session)
            .map_err(|e| Error::wrap("subtask search failed", e))?;
        if tasks_to_cancel.is_empty() {
            return Ok(());
        }

        let mut failures: Vec<Error> = Vec::new();
        let mut client_cache: HashMap<String, Arc<dyn A2AClient>> = HashMap::new();
        for task in tasks_to_cancel {
            let Some(subagent) = subagents.iter().find(|a| a.agent.name() == task.agent_name)
            else {
                continue;
            };
            // The subagent's client may carry the auth interceptor the remote agent installs for
            // A2AConfig.auth, which keys the credential on the scope the call carries. Without
            // it the provider would see no identity for this cancel, and a secured remote would
            // leave its task running. The client provider and a card fetch see it too, matching
            // the remote agent's own run loop.
            let id = call_identity(
                &subagent.config,
                credential_scope(
                    &cfg.app_name,
                    &meta.user_id,
                    &meta.session_id,
                    &task.agent_name,
                ),
            );
            let client = match client_cache.get(&task.agent_name) {
                Some(client) => Arc::clone(client),
                None => {
                    let created = with_auth_scope(
                        id.clone(),
                        create_a2a_client(ClientContext::Detached, &subagent.config),
                    )
                    .await;
                    match created {
                        Ok((_, client)) => {
                            client_cache.insert(task.agent_name.clone(), Arc::clone(&client));
                            client
                        }
                        Err(err) => {
                            failures.push(Error::wrap("failed to create A2A client", err));
                            continue;
                        }
                    }
                }
            };
            let req = a2a::CancelTaskRequest {
                id: task.task_id.clone(),
                metadata: None,
                tenant: None,
            };
            let cancelled = tokio::time::timeout(
                CLEANUP_TIMEOUT,
                with_auth_scope(id, client.cancel_task(&req)),
            )
            .await;
            match cancelled {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => failures.push(Error::wrap("failed to cancel task", err)),
                Err(_) => failures.push(Error::msg(
                    "failed to cancel task: context deadline exceeded",
                )),
            }
        }
        for client in client_cache.values() {
            if let Err(err) = client.destroy().await {
                failures.push(Error::wrap("client destroy failed", err));
            }
        }
        if failures.is_empty() {
            return Ok(());
        }
        // Go `errors.Join`.
        Err(Error::msg(
            failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }

    /// Go `createRunnerConfig`.
    async fn create_runner_config(
        &self,
        ctx: &a2a_server::ExecutorContext,
    ) -> Result<RunnerConfig, Error> {
        let executor_plugin = ExecutorPlugin::new();
        let (cfg, _) = (self.runner_provider())(ctx, executor_plugin.plugin)
            .await
            .map_err(|e| Error::wrap("runner provider failed", e))?;
        Ok(cfg)
    }
}

impl a2a_server::AgentExecutor for Executor {
    /// Go `Execute`, wrapped in the cleanup guard.
    fn execute(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> {
        let guarded = clone_request(&ctx);
        let task_id = ctx.task_id.clone();
        let (abort, abort_registration) = AbortHandle::new_pair();
        let inner = Abortable::new(self.execute_stream(ctx), abort_registration);
        let registration = self.register_execution(task_id, abort);
        Box::pin(
            CleanupGuard::new(self.clone(), guarded, Box::pin(inner)).with_execution(registration),
        )
    }

    /// Go `Cancel`, wrapped in the cleanup guard: one `canceled` status update. The task's
    /// running execution, if any, stops at once (Go cancels its context). A request without a
    /// task ID yields only an error (Go 58429928).
    fn cancel(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>> {
        if ctx.task_id.is_empty() {
            return Box::pin(futures::stream::iter([Err(a2a::A2AError::internal(
                "A2A cancellation must have a task ID",
            ))]));
        }
        let event = new_status_update(&TaskInfo::of(&ctx), a2a::TaskState::Canceled, None);
        let event = stamped(a2a::StreamResponse::StatusUpdate(event));
        self.stop_execution(&ctx.task_id, &event);
        let inner = futures::stream::iter([Ok(event)]);
        Box::pin(CleanupGuard::new(self.clone(), ctx, Box::pin(inner)))
    }
}

/// Go `withADKExtensionMeta`: every yielded event carries the ADK extension key (tasks and
/// status updates only).
fn stamped(mut event: a2a::StreamResponse) -> a2a::StreamResponse {
    add_adk_extension_meta(&mut event);
    event
}

/// Go `a2a.NewSubmittedTask(execCtx, msg)`.
fn new_submitted_task(ctx: &a2a_server::ExecutorContext, msg: a2a::Message) -> a2a::Task {
    let id = if ctx.task_id.is_empty() {
        a2a::new_task_id()
    } else {
        ctx.task_id.clone()
    };
    let context_id = if ctx.context_id.is_empty() {
        a2a::new_context_id()
    } else {
        ctx.context_id.clone()
    };
    a2a::Task {
        id,
        context_id,
        status: a2a::TaskStatus {
            state: a2a::TaskState::Submitted,
            message: None,
            timestamp: None,
        },
        artifacts: None,
        history: Some(vec![msg]),
        metadata: None,
    }
}

/// Go `prepareSession`: gets the session, creating it when the get fails.
async fn prepare_session(cfg: &RunnerConfig, meta: &InvocationMeta) -> Result<(), Error> {
    let service = &cfg.session_service;
    let found = service
        .get(GetRequest {
            app_name: cfg.app_name.clone(),
            user_id: meta.user_id.clone(),
            session_id: meta.session_id.clone(),
            ..Default::default()
        })
        .await;
    if found.is_ok() {
        return Ok(());
    }
    service
        .create(CreateRequest {
            app_name: cfg.app_name.clone(),
            user_id: meta.user_id.clone(),
            session_id: Some(meta.session_id.clone()),
            state: Some(serde_json::Map::new()),
        })
        .await
        .map_err(|e| Error::wrap("failed to create a session", e))?;
    Ok(())
}

/// The status Go's cleanup reads the pending long-running calls from, when the request was a
/// successful cancel of an input-required task.
///
/// Go's trigger is a stored task in `input-required` and a `canceled` result for a request
/// without a message. a2a-rs's `cancel_task` moves the stored task to `canceled` (keeping its
/// status message) before the executor runs, so a `canceled` stored task is read as the
/// `input-required` one it was (`pre_cancel_status`) when its status message still holds pending
/// long-running calls.
#[cfg(feature = "client")]
fn child_cancel_status(
    ctx: &a2a_server::ExecutorContext,
    result: Option<&a2a::Task>,
) -> Option<a2a::TaskStatus> {
    if ctx.message.is_some() {
        return None;
    }
    let stored = ctx.stored_task.as_ref()?;
    if result?.status.state != a2a::TaskState::Canceled {
        return None;
    }
    match stored.status.state {
        a2a::TaskState::InputRequired => Some(stored.status.clone()),
        // parity: divergence: a2a-rs `begin_cancel` sets `canceled` before the executor runs.
        a2a::TaskState::Canceled => {
            let mut pre_cancel_status = stored.status.clone();
            pre_cancel_status.state = a2a::TaskState::InputRequired;
            match get_pending_long_running_call_ids(&pre_cancel_status) {
                Ok(ids) if ids.is_empty() => None,
                // A malformed message still reaches Go's "subtask search failed" warning.
                _ => Some(pre_cancel_status),
            }
        }
        _ => None,
    }
}

/// Go `remoteAgent`: a remote sub-agent and its A2A configuration.
#[cfg(feature = "client")]
pub(crate) struct RemoteSubagent {
    agent: Arc<dyn Agent>,
    config: Arc<A2AServerConfig>,
}

/// Go `findRemoteSubagents`: the remote agents of the tree, not descending into them.
#[cfg(feature = "client")]
pub(crate) fn find_remote_subagents(root: &Arc<dyn Agent>) -> Vec<RemoteSubagent> {
    fn collect(agent: &Arc<dyn Agent>, result: &mut Vec<RemoteSubagent>) {
        if let Some(config) = find_remote_agent_state(agent.as_ref()) {
            result.push(RemoteSubagent {
                agent: Arc::clone(agent),
                config,
            });
            return;
        }
        for sub in agent.sub_agents() {
            collect(sub, result);
        }
    }
    let mut result = Vec::new();
    collect(root, &mut result);
    result
}

/// Go `AttachAuthScope`: the scope to cancel a child task in, only when the sub-agent owns its
/// auth scope. The provider sees no identity on this path (Go carries only the session ID), and
/// the credential is not cached across calls (Go's nil credential cell).
///
/// The `AUTH_SCOPE` task-local means "apply this provider's credential", so a configuration that
/// owns its scope without a credential provider (Go: a scope with no transport keyed on it)
/// attaches none.
#[cfg(feature = "client")]
fn call_identity(
    config: &A2AServerConfig,
    scope: crate::remoteagent::CredentialScope,
) -> Option<CallIdentity> {
    if !config.owns_auth_scope {
        return None;
    }
    Some(CallIdentity {
        scope,
        identity: None,
        auth: config.auth.clone()?,
        card_fetch_client: config.card_fetch_client.clone(),
        cell: Arc::new(CredentialCell::uncached()),
    })
}

/// Go `newDefaultRunnerProvider`: a `gadk_engine::runner::Runner` built from `base` with the
/// executor's plugin appended to its plugins.
fn default_runner_provider(base: Option<gadk_engine::runner::RunnerConfig>) -> RunnerProvider {
    Arc::new(move |_ctx, plugin| {
        let base = base.clone();
        Box::pin(async move {
            // Go also rejects a nil `SessionService`; gadk's `RunnerConfig` always has one.
            let Some(mut cfg) = base else {
                return Err(Error::msg("runner.Config.Agent is not provided"));
            };
            cfg.plugin_config.plugins.push(plugin);
            let runner_config = RunnerConfig {
                app_name: cfg.app_name.clone(),
                agent: Arc::clone(&cfg.agent),
                session_service: Arc::clone(&cfg.session_service),
            };
            let runner = gadk_engine::runner::Runner::new(cfg)?;
            Ok((
                runner_config,
                Arc::new(DefaultRunner(runner)) as Arc<dyn Runner>,
            ))
        })
    })
}

/// Go `defaultRunner`: adapts `gadk_engine::runner::Runner` to [`Runner`].
struct DefaultRunner(gadk_engine::runner::Runner);

impl Runner for DefaultRunner {
    fn run(&self, user_id: &str, session_id: &str, msg: Content, cfg: RunConfig) -> EventStream {
        self.0.run(
            user_id,
            session_id,
            Some(msg),
            cfg,
            gadk_engine::runner::RunOptions::default(),
        )
    }
}

#[cfg(all(test, feature = "client"))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use a2a_server::AgentExecutor as _;
    use gadk_agent::{
        AgentConfig, AgentError, AgentState, AgentType, InvocationContext, StreamingMode,
        new_agent, new_with_state,
    };
    use gadk_genai::{FunctionCall, Part};
    use gadk_model::LlmResponse;
    use gadk_session::InMemorySessionService;
    use parking_lot::Mutex;
    use serde_json::json;

    use super::*;
    use crate::convert::{ADK_EXTENSION_URI, StreamResponseRef};
    use crate::remoteagent::{
        A2AClientProvider, CredentialScope, RemoteAgentState, current_credential_scope,
        new_a2a_client_provider,
    };
    use crate::server::identity::{IDENTITY_HEADER, identity_header_resolver};
    use crate::server::plugin::EXECUTOR_PLUGIN_NAME;
    use crate::testutil::{
        FakeA2AClient, FakeCall, FakeClientProvider, ScriptedRunner, eventually, spawn_a2a_server,
    };

    fn engine_config(
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> gadk_engine::runner::RunnerConfig {
        gadk_engine::runner::RunnerConfig {
            app_name: agent.name().to_owned(),
            agent,
            session_service,
            artifact_service: None,
            memory_service: None,
            plugin_config: gadk_engine::runner::PluginConfig::default(),
            auto_create_session: false,
            platform: gadk_platform::Platform::default(),
            compaction: None,
        }
    }

    fn text_event(text: &str) -> Event {
        Event {
            response: LlmResponse {
                content: Some(Content {
                    role: "model".into(),
                    parts: vec![Part::from_text(text)],
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn request(message: Option<a2a::Message>) -> a2a_server::ExecutorContext {
        a2a_server::ExecutorContext {
            message,
            task_id: "task-1".into(),
            stored_task: None,
            context_id: "ctx-1".into(),
            metadata: None,
            user: None,
            service_params: HashMap::new(),
            tenant: None,
        }
    }

    fn hi() -> a2a::Message {
        a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("hi")])
    }

    fn card(name: &str) -> a2a::AgentCard {
        serde_json::from_value(json!({
            "name": name, "description": "", "version": "1.0.0",
            "supportedInterfaces": [], "capabilities": {"streaming": true},
            "defaultInputModes": [], "defaultOutputModes": [], "skills": [],
        }))
        .unwrap()
    }

    fn status_state(event: &a2a::StreamResponse) -> Option<a2a::TaskState> {
        match event {
            a2a::StreamResponse::StatusUpdate(u) => Some(u.status.state.clone()),
            a2a::StreamResponse::Task(t) => Some(t.status.state.clone()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn every_config_field_reaches_the_execution() {
        #[derive(Clone)]
        struct Marker(&'static str);

        let seen_input = Arc::new(Mutex::new(String::new()));
        let seen_mode = Arc::new(Mutex::new(None));
        let agent = {
            let (seen_input, seen_mode) = (Arc::clone(&seen_input), Arc::clone(&seen_mode));
            new_agent(AgentConfig {
                name: "agent".into(),
                run: Some(Arc::new(move |ctx: InvocationContext| {
                    if let Some(part) = ctx.user_content().and_then(|c| c.parts.first()) {
                        *seen_input.lock() = part.text.clone();
                    }
                    *seen_mode.lock() = Some(ctx.run_config().streaming_mode);
                    futures::stream::iter([Ok(text_event("out"))]).boxed()
                })),
                ..Default::default()
            })
            .unwrap()
        };
        let base = engine_config(agent, Arc::new(InMemorySessionService::new()));
        let provided = Arc::new(AtomicUsize::new(0));
        let after_events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let after_execute = Arc::new(Mutex::new(Vec::<(&'static str, bool)>::new()));
        let cleanups = Arc::new(AtomicUsize::new(0));

        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(base.clone()),
            runner_provider: Some({
                let provided = Arc::clone(&provided);
                let inner = default_runner_provider(Some(base));
                Arc::new(move |ctx, plugin| {
                    provided.fetch_add(1, Ordering::SeqCst);
                    inner(ctx, plugin)
                })
            }),
            run_config: RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
            before_execute_callback: Some(Arc::new(|_ctx, ext| {
                ext.insert(Marker("before"));
                Box::pin(async { Ok(()) })
            })),
            after_event_callback: Some({
                let after_events = Arc::clone(&after_events);
                Arc::new(move |ctx, _event, _update| {
                    after_events
                        .lock()
                        .push(ctx.extensions().get::<Marker>().map_or("", |m| m.0));
                    Box::pin(async { Ok(()) })
                })
            }),
            after_execute_callback: Some({
                let after_execute = Arc::clone(&after_execute);
                Arc::new(move |ctx, _status, err| {
                    after_execute.lock().push((
                        ctx.extensions().get::<Marker>().map_or("", |m| m.0),
                        err.is_some(),
                    ));
                    Box::pin(async { Ok(()) })
                })
            }),
            a2a_part_converter: Some(Arc::new(|_evt: StreamResponseRef<'_>, part: &a2a::Part| {
                let text = part.as_text().unwrap_or_default().to_uppercase();
                Box::pin(async move { Ok(Some(Part::from_text(text))) })
            })),
            genai_part_converter: Some(Arc::new(|_evt: Option<&Event>, part: &Part| {
                let text = format!("[{}]", part.text);
                Box::pin(async move { Ok(Some(a2a::Part::text(text))) })
            })),
            output_mode: OutputMode::ArtifactPerEvent,
            a2a_execution_cleanup_callback: Some({
                let cleanups = Arc::clone(&cleanups);
                Arc::new(move |_ctx, _cards, _result, _cause| {
                    cleanups.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async {})
                })
            }),
            user_resolver: Some(Arc::new(|_ctx| Some("alice".to_owned()))),
        });

        let got: Vec<_> = executor.execute(request(Some(hi()))).collect().await;
        let got: Vec<_> = got.into_iter().map(Result::unwrap).collect();

        assert_eq!(*seen_input.lock(), "HI", "a2a_part_converter");
        assert_eq!(*seen_mode.lock(), Some(StreamingMode::Sse), "run_config");
        let a2a::StreamResponse::StatusUpdate(working) = &got[1] else {
            panic!("events[1] = {:?}", got[1]);
        };
        assert_eq!(
            working.metadata.as_ref().unwrap()["adk_user_id"],
            json!("alice"),
            "user_resolver"
        );
        let a2a::StreamResponse::ArtifactUpdate(artifact) = &got[2] else {
            panic!("events[2] = {:?}", got[2]);
        };
        assert_eq!(
            artifact.artifact.parts[0].as_text(),
            Some("[out]"),
            "genai_part_converter"
        );
        assert_eq!(artifact.last_chunk, Some(true), "output_mode");
        assert_eq!(*after_events.lock(), vec!["before"], "after_event_callback");
        assert_eq!(
            *after_execute.lock(),
            vec![("before", false)],
            "after_execute_callback"
        );
        assert_eq!(
            status_state(got.last().unwrap()),
            Some(a2a::TaskState::Completed)
        );

        // The cleanup runs on a spawned task once the stream has ended.
        eventually(|| cleanups.load(Ordering::SeqCst) >= 1 && provided.load(Ordering::SeqCst) >= 2)
            .await;
        assert_eq!(cleanups.load(Ordering::SeqCst), 1, "cleanup callback");
        // Once for the execution, once for the cleanup (Go `createRunnerConfig`).
        assert_eq!(provided.load(Ordering::SeqCst), 2, "runner_provider");
    }

    #[tokio::test]
    async fn the_default_runner_installs_the_a2a_executor_plugin() {
        assert_eq!(ExecutorPlugin::new().plugin.name(), "a2a-executor");
        assert_eq!(EXECUTOR_PLUGIN_NAME, "a2a-executor");

        let agent = new_agent(AgentConfig {
            name: "agent".into(),
            run: Some(Arc::new(|_ctx| {
                futures::stream::iter([Ok(text_event("out"))]).boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        let seen = Arc::new(Mutex::new(None));
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(engine_config(
                agent,
                Arc::new(InMemorySessionService::new()),
            )),
            after_execute_callback: Some({
                let seen = Arc::clone(&seen);
                Arc::new(move |ctx, _status, _err| {
                    *seen.lock() = Some(ctx.events().len());
                    Box::pin(async { Ok(()) })
                })
            }),
            ..Default::default()
        });
        let _: Vec<_> = executor.execute(request(Some(hi()))).collect().await;
        // The plugin captured the invocation session: the user message and the agent's event.
        assert_eq!(*seen.lock(), Some(2));
    }

    #[tokio::test]
    async fn without_a_runner_config_the_default_provider_fails_like_go() {
        let executor = Executor::new(ExecutorConfig::default());
        let got: Vec<_> = executor.execute(request(Some(hi()))).collect().await;
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].as_ref().unwrap_err().message,
            "failed to create a runner: runner.Config.Agent is not provided"
        );
    }

    /// Runs one execution through a scripted runner and returns the user the runner saw.
    async fn resolved_user(
        resolver: Option<UserResolver>,
        user: Option<&str>,
        header: Option<&str>,
    ) -> String {
        let agent = new_agent(AgentConfig {
            name: "agent".into(),
            ..Default::default()
        })
        .unwrap();
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let runner = Arc::new(ScriptedRunner::new(vec![]));
        let executor = Executor::new(ExecutorConfig {
            runner_provider: Some({
                let runner = Arc::clone(&runner);
                Arc::new(move |_ctx, _plugin| {
                    let cfg = RunnerConfig {
                        app_name: "app".into(),
                        agent: Arc::clone(&agent),
                        session_service: Arc::clone(&sessions),
                    };
                    let runner = Arc::clone(&runner) as Arc<dyn Runner>;
                    Box::pin(async move { Ok((cfg, runner)) })
                })
            }),
            user_resolver: resolver,
            ..Default::default()
        });
        let mut ctx = request(Some(hi()));
        ctx.user = user.map(a2a_server::User::authenticated);
        if let Some(header) = header {
            ctx.service_params.insert(
                IDENTITY_HEADER.to_owned(),
                vec![String::new(), header.to_owned()],
            );
        }
        let got: Vec<_> = executor.execute(ctx).collect().await;
        let a2a::StreamResponse::StatusUpdate(working) = got[1].as_ref().unwrap() else {
            panic!("events[1] = {:?}", got[1]);
        };
        let user_id = runner.calls()[0].0.clone();
        assert_eq!(
            working.metadata.as_ref().unwrap()["adk_user_id"],
            json!(user_id),
            "the event metadata names the runner's user"
        );
        user_id
    }

    #[tokio::test]
    async fn the_user_is_resolved_in_order() {
        let fallback = "A2A_USER_ctx-1";
        // Default resolver: the call context's user, never the header.
        assert_eq!(resolved_user(None, Some("alice"), None).await, "alice");
        assert_eq!(resolved_user(None, None, Some("mallory")).await, fallback);
        assert_eq!(resolved_user(None, Some(""), None).await, fallback);
        assert_eq!(resolved_user(None, None, None).await, fallback);
        // The header resolver: the first non-empty header value, then the default.
        let header = || Some(identity_header_resolver());
        assert_eq!(
            resolved_user(header(), Some("alice"), Some("bob")).await,
            "bob"
        );
        assert_eq!(resolved_user(header(), Some("alice"), None).await, "alice");
        assert_eq!(resolved_user(header(), None, None).await, fallback);
        // A resolver's empty answer falls back too.
        let empty: UserResolver = Arc::new(|_ctx| Some(String::new()));
        assert_eq!(
            resolved_user(Some(empty), Some("alice"), None).await,
            fallback
        );
    }

    fn replay_executor(
        events: Vec<Event>,
        cleanup: Option<A2AExecutionCleanupCallback>,
    ) -> Executor {
        let agent = new_agent(AgentConfig {
            name: "agent".into(),
            run: Some(Arc::new(move |_ctx| {
                futures::stream::iter(events.clone().into_iter().map(Ok)).boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        Executor::new(ExecutorConfig {
            runner_config: Some(engine_config(
                agent,
                Arc::new(InMemorySessionService::new()),
            )),
            a2a_execution_cleanup_callback: cleanup,
            ..Default::default()
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    // parity: divergence: a2a-rs's non-streaming send passes a submitted snapshot as `stored_task`, so the executor yields no submitted `Task` there (Go yields one); the client still gets the task
    async fn a_non_streaming_send_yields_no_submitted_task() {
        // What a2a-rs hands the executor on a non-streaming send: the stored task is the
        // submitted snapshot of the new task.
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let cleanup: A2AExecutionCleanupCallback = {
            let snapshots = Arc::clone(&snapshots);
            Arc::new(move |ctx, _cards, _result, _cause| {
                snapshots
                    .lock()
                    .push(ctx.stored_task.as_ref().map(|t| t.status.state.clone()));
                Box::pin(async {})
            })
        };
        let server = spawn_a2a_server(
            replay_executor(vec![text_event("Hello")], Some(cleanup)),
            card("agent"),
        )
        .await;
        let client = new_a2a_client_provider(None)
            .create_client(ClientContext::Detached, &server.card)
            .await
            .unwrap();
        let req: a2a::SendMessageRequest = serde_json::from_value(json!({
            "message": {"messageId": a2a::new_message_id(), "role": "ROLE_USER", "parts": [{"text": "hi"}]},
        }))
        .unwrap();
        let a2a::SendMessageResponse::Task(task) = client.send_message(&req).await.unwrap() else {
            panic!("want a task");
        };
        assert_eq!(task.status.state, a2a::TaskState::Completed);
        eventually(|| !snapshots.lock().is_empty()).await;
        assert_eq!(*snapshots.lock(), vec![Some(a2a::TaskState::Submitted)]);

        // With that snapshot the executor starts at `working`.
        let mut ctx = request(Some(hi()));
        ctx.stored_task = Some(new_submitted_task(&ctx, hi()));
        let got: Vec<_> = replay_executor(vec![text_event("Hello")], None)
            .execute(ctx)
            .collect()
            .await;
        assert_eq!(
            status_state(got[0].as_ref().unwrap()),
            Some(a2a::TaskState::Working)
        );
        assert!(
            !got.iter()
                .any(|e| matches!(e, Ok(a2a::StreamResponse::Task(_)))),
            "no submitted task: {got:?}"
        );

        // A streaming send has no stored task: the submitted task comes first, stamped.
        let mut stream = client.send_streaming_message(&req).await.unwrap();
        let Some(Ok(a2a::StreamResponse::Task(first))) = stream.next().await else {
            panic!("want the submitted task first");
        };
        assert_eq!(first.status.state, a2a::TaskState::Submitted);
        assert_eq!(first.metadata.unwrap()[ADK_EXTENSION_URI], json!(true));
    }

    /// A remote sub-agent named `name` (what `new_a2a` builds) over `provider`.
    fn remote_agent(
        name: &str,
        provider: Arc<dyn A2AClientProvider>,
        auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
    ) -> Arc<dyn Agent> {
        new_with_state(
            AgentConfig {
                name: name.into(),
                description: "Remote approver.".into(),
                ..Default::default()
            },
            AgentState {
                agent_type: AgentType::RemoteAgent,
                config: Some(Arc::new(RemoteAgentState {
                    a2a: Arc::new(A2AServerConfig {
                        agent_card: Some(card(name)),
                        agent_card_provider: None,
                        client_provider: provider,
                        owns_auth_scope: auth.is_some(),
                        card_fetch_client: None,
                        auth,
                    }),
                })),
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .unwrap()
    }

    /// The child's long-running call, as the remote agent records it: authored by the child and
    /// carrying the child task's IDs.
    fn child_call_event() -> Event {
        let mut response = LlmResponse {
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
        };
        response.custom_metadata = crate::convert::to_custom_metadata("child-task-1", "child-ctx");
        Event {
            response,
            author: "child".into(),
            long_running_tool_ids: vec!["call-1".into()],
            ..Default::default()
        }
    }

    /// A root agent that delegates to `child` and yields the child's long-running call.
    fn root_agent(child: Arc<dyn Agent>) -> Arc<dyn Agent> {
        new_agent(AgentConfig {
            name: "root".into(),
            sub_agents: vec![child],
            run: Some(Arc::new(|_ctx| {
                futures::stream::iter([Ok::<_, AgentError>(child_call_event())]).boxed()
            })),
            ..Default::default()
        })
        .unwrap()
    }

    /// Go's a2a-go keeps the code of an A2A error wrapped by the executor (`errors.Is` against
    /// the code sentinels) with the wrapped message.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrapped_a2a_error_reaches_the_client_with_its_code() {
        let agent = new_agent(AgentConfig {
            name: "agent".into(),
            run: Some(Arc::new(|_ctx: InvocationContext| {
                futures::stream::iter([Ok(text_event("out"))]).boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(engine_config(
                agent,
                Arc::new(InMemorySessionService::new()),
            )),
            before_execute_callback: Some(Arc::new(|_ctx, _ext| {
                Box::pin(async { Err(Error::A2A(a2a::A2AError::invalid_params("bad input"))) })
            })),
            ..Default::default()
        });
        let server = spawn_a2a_server(executor, card("root")).await;
        let client = new_a2a_client_provider(None)
            .create_client(ClientContext::Detached, &server.card)
            .await
            .unwrap();

        let req: a2a::SendMessageRequest = serde_json::from_value(json!({
            "message": {"messageId": a2a::new_message_id(), "role": "ROLE_USER", "parts": [{"text": "Hello"}]},
        }))
        .unwrap();
        let err = client.send_message(&req).await.unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (a2a::error_code::INVALID_PARAMS, "before execute: bad input")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_an_input_required_task_cancels_the_child_task() {
        let fake = Arc::new(
            FakeA2AClient::new().with_cancel(Ok(serde_json::from_value(json!({
                "id": "child-task-1", "contextId": "child-ctx",
                "status": {"state": "TASK_STATE_CANCELED"},
            }))
            .unwrap())),
        );
        let child = remote_agent(
            "child",
            Arc::new(FakeClientProvider::new(Arc::clone(&fake))),
            None,
        );
        let cleanups = Arc::new(Mutex::new(Vec::new()));
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(engine_config(
                root_agent(child),
                Arc::new(InMemorySessionService::new()),
            )),
            a2a_execution_cleanup_callback: Some({
                let cleanups = Arc::clone(&cleanups);
                Arc::new(move |ctx, cards, result, _cause| {
                    cleanups.lock().push((
                        ctx.message.is_some(),
                        cards.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
                        result.map(|t| t.status.state.clone()),
                    ));
                    Box::pin(async {})
                })
            }),
            ..Default::default()
        });
        let server = spawn_a2a_server(executor, card("root")).await;
        let client = new_a2a_client_provider(None)
            .create_client(ClientContext::Detached, &server.card)
            .await
            .unwrap();

        let req: a2a::SendMessageRequest = serde_json::from_value(json!({
            "message": {"messageId": a2a::new_message_id(), "role": "ROLE_USER", "parts": [{"text": "Hello"}]},
        }))
        .unwrap();
        let a2a::SendMessageResponse::Task(task) = client.send_message(&req).await.unwrap() else {
            panic!("want a task");
        };
        assert_eq!(task.status.state, a2a::TaskState::InputRequired);
        assert!(fake.calls().is_empty(), "no child call before the cancel");

        let canceled = client
            .cancel_task(&a2a::CancelTaskRequest {
                id: task.id.clone(),
                metadata: None,
                tenant: None,
            })
            .await
            .unwrap();
        assert_eq!(canceled.status.state, a2a::TaskState::Canceled);

        // The cleanup cancels the child task asynchronously.
        eventually(|| fake.calls().len() >= 2).await;
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "calls = {calls:?}");
        let FakeCall::Cancel(cancel) = &calls[0] else {
            panic!("calls[0] = {:?}, want a cancel", calls[0]);
        };
        assert_eq!(cancel.id, "child-task-1");
        assert!(matches!(calls[1], FakeCall::Destroy));

        // Both cleanups (the execution's and the cancel's) run on spawned tasks.
        eventually(|| cleanups.lock().len() >= 2).await;
        let cleanups = cleanups.lock().clone();
        assert_eq!(
            cleanups,
            vec![
                (
                    true,
                    vec!["child".to_owned()],
                    Some(a2a::TaskState::InputRequired)
                ),
                (
                    false,
                    vec!["child".to_owned()],
                    Some(a2a::TaskState::Canceled)
                ),
            ]
        );
    }

    /// An A2A client that records the credential scope each cancel runs in.
    struct ScopeRecordingClient(Arc<Mutex<Vec<Option<CredentialScope>>>>);

    #[async_trait::async_trait]
    impl A2AClient for ScopeRecordingClient {
        async fn send_message(
            &self,
            _req: &a2a::SendMessageRequest,
        ) -> Result<a2a::SendMessageResponse, a2a::A2AError> {
            Err(a2a::A2AError::internal("unused"))
        }
        async fn send_streaming_message(
            &self,
            _req: &a2a::SendMessageRequest,
        ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError>
        {
            Err(a2a::A2AError::internal("unused"))
        }
        async fn cancel_task(
            &self,
            req: &a2a::CancelTaskRequest,
        ) -> Result<a2a::Task, a2a::A2AError> {
            self.0.lock().push(current_credential_scope());
            Err(a2a::A2AError::task_not_found(&req.id))
        }
        async fn destroy(&self) -> Result<(), a2a::A2AError> {
            Ok(())
        }
    }

    struct ScopeRecordingProvider(Arc<Mutex<Vec<Option<CredentialScope>>>>);

    #[async_trait::async_trait]
    impl A2AClientProvider for ScopeRecordingProvider {
        async fn create_client(
            &self,
            _cx: ClientContext<'_>,
            _card: &a2a::AgentCard,
        ) -> Result<Arc<dyn A2AClient>, Error> {
            Ok(Arc::new(ScopeRecordingClient(Arc::clone(&self.0))))
        }
    }

    /// Runs the cleanup of a successful cancel of an input-required task whose session holds the
    /// child's call; returns the scopes the child cancels ran in.
    async fn child_cancel_scopes(owns_auth_scope: bool) -> Vec<Option<CredentialScope>> {
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let auth = owns_auth_scope.then(|| gadk_auth::static_token("tok"));
        let child = remote_agent(
            "child",
            Arc::new(ScopeRecordingProvider(Arc::clone(&scopes))),
            auth,
        );
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let executor = Executor::new(ExecutorConfig {
            runner_config: Some(engine_config(root_agent(child), Arc::clone(&sessions))),
            ..Default::default()
        });

        // The execution that left the task input-required.
        let got: Vec<_> = executor.execute(request(Some(hi()))).collect().await;
        let Some(Ok(a2a::StreamResponse::StatusUpdate(input_required))) = got.last() else {
            panic!("want a final status update: {got:?}");
        };
        assert_eq!(input_required.status.state, a2a::TaskState::InputRequired);

        // a2a-rs `begin_cancel` keeps the status message and sets `canceled`.
        let mut stored = new_submitted_task(&request(None), hi());
        stored.status = input_required.status.clone();
        stored.status.state = a2a::TaskState::Canceled;
        let mut ctx = request(None);
        ctx.stored_task = Some(stored.clone());
        executor.cleanup(ctx, Some(stored), None).await;
        scopes.lock().clone()
    }

    #[tokio::test]
    async fn the_child_cancel_runs_in_the_invocation_scope_only_when_owned() {
        assert_eq!(
            child_cancel_scopes(true).await,
            vec![Some(credential_scope(
                "root",
                "A2A_USER_ctx-1",
                "ctx-1",
                "child"
            ))]
        );
        assert_eq!(child_cancel_scopes(false).await, vec![None]);
    }

    #[test]
    fn the_child_cancel_trigger_reads_the_pre_cancel_status() {
        let call_part = {
            let mut part = a2a::Part::data(json!({"id": "call-1", "name": "approve"}));
            part.metadata = Some(HashMap::from([(
                crate::convert::DATA_PART_META_TYPE_KEY.to_owned(),
                json!(crate::convert::DATA_PART_TYPE_FUNCTION_CALL),
            )]));
            part
        };
        let task = |state: a2a::TaskState, parts: Option<Vec<a2a::Part>>| a2a::Task {
            id: "task-1".into(),
            context_id: "ctx-1".into(),
            status: a2a::TaskStatus {
                state,
                message: parts.map(|p| a2a::Message::new(a2a::Role::Agent, p)),
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        };
        let ctx = |stored: a2a::Task, message: bool| {
            let mut ctx = request(message.then(hi));
            ctx.stored_task = Some(stored);
            ctx
        };
        let canceled = task(a2a::TaskState::Canceled, None);

        // a2a-rs: canceled with pending calls → restored to input-required.
        let pre = child_cancel_status(
            &ctx(
                task(a2a::TaskState::Canceled, Some(vec![call_part.clone()])),
                false,
            ),
            Some(&canceled),
        )
        .unwrap();
        assert_eq!(pre.state, a2a::TaskState::InputRequired);
        assert_eq!(pre.message.unwrap().parts, vec![call_part.clone()]);
        // Go: input-required stored task.
        assert!(
            child_cancel_status(
                &ctx(
                    task(a2a::TaskState::InputRequired, Some(vec![call_part.clone()])),
                    false
                ),
                Some(&canceled),
            )
            .is_some()
        );
        // Not after a cancel of a task without pending calls, nor of a send, nor without a
        // canceled result.
        assert!(
            child_cancel_status(
                &ctx(
                    task(a2a::TaskState::Canceled, Some(vec![a2a::Part::text("x")])),
                    false
                ),
                Some(&canceled),
            )
            .is_none()
        );
        assert!(
            child_cancel_status(
                &ctx(
                    task(a2a::TaskState::Canceled, Some(vec![call_part.clone()])),
                    true
                ),
                Some(&canceled),
            )
            .is_none()
        );
        assert!(
            child_cancel_status(
                &ctx(task(a2a::TaskState::Canceled, Some(vec![call_part])), false),
                Some(&task(a2a::TaskState::Working, None)),
            )
            .is_none()
        );
    }
}
