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

//! The runner (port of ADK-Go `runner/runner.go`; context compaction is in `compaction`, live mode is in `live`):
//! [`Runner`], [`RunnerConfig`], [`PluginConfig`], [`RunOptions`].

pub(crate) mod agent_node;
pub(crate) mod compaction;
pub(crate) mod find_agent;
pub(crate) mod live;
pub(crate) mod run_node;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::fiber::FiberStream;
use gadk_agent::{
    Agent, AgentError, Context, ContextDelta, EventStream, InvocationContext, Mode, ParentMap,
    Plugin, PluginManager, RunConfig,
};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, EventCompaction, GetRequest, Session, SessionService};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::llm::agent::underlying_session;
use crate::llm::mode::resolve_mode;
use crate::llm::state::reveal;
use find_agent::find_active_task_isolation_scope;
use run_node::is_llm_agent;

/// Go `plugininternal.BeforeRunReplyKey`: marks a before-run reply in `custom_metadata`.
pub const BEFORE_RUN_REPLY_KEY: &str = "adk:before_run_reply";

/// Port of Go `runner.Config`.
#[derive(Clone)]
pub struct RunnerConfig {
    /// The application name sessions are scoped to.
    pub app_name: String,
    /// The root agent, which starts the execution.
    pub agent: Arc<dyn Agent>,
    /// The session service.
    pub session_service: Arc<dyn SessionService>,
    /// Optional artifact service.
    pub artifact_service: Option<Arc<dyn gadk_artifact::ArtifactService>>,
    /// Optional memory service.
    pub memory_service: Option<Arc<dyn gadk_memory::MemoryService>>,
    /// Plugins and their close timeout.
    pub plugin_config: PluginConfig,
    /// Create the session on `run` when it does not exist.
    pub auto_create_session: bool,
    /// Clock and ID source.
    pub platform: Platform,
    /// Context compaction for the sessions this runner drives (Go `Config.Compaction`); `None`
    /// disables it. A config without a summarizer gets an [`LlmSummarizer`] over the root
    /// agent's model, which then has to be an LLM agent.
    ///
    /// [`LlmSummarizer`]: gadk_session::compaction::LlmSummarizer
    pub compaction: Option<gadk_session::compaction::Config>,
}

/// Port of Go `runner.PluginConfig`: the plugins a [`Runner`] applies and how long it waits for
/// them to close.
#[derive(Clone, Default)]
pub struct PluginConfig {
    /// The plugins, in registration order.
    pub plugins: Vec<Plugin>,
    /// How long [`Runner::close`] waits for the plugins to close.
    pub close_timeout: Duration,
}

/// Port of Go `runner.RunOption`s: `WithStateDelta` → `state_delta`, `WithYieldUserMessage` →
/// `yield_user_message` (honored by the node path only, as in Go).
#[derive(Clone, Default)]
pub struct RunOptions {
    /// State delta attached to the user message event.
    pub state_delta: Option<Map<String, Value>>,
    /// Yield the appended user message event before any agent event (node path only).
    pub yield_user_message: bool,
}

/// Port of Go `runner.Runner`: runs the agent tree within a session, persisting events and
/// driving plugins.
pub struct Runner {
    inner: Arc<RunnerInner>,
}

pub(crate) struct RunnerInner {
    app_name: String,
    root_agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    artifact_service: Option<Arc<dyn gadk_artifact::ArtifactService>>,
    memory_service: Option<Arc<dyn gadk_memory::MemoryService>>,
    parents: Arc<ParentMap>,
    plugin_manager: Arc<PluginManager>,
    auto_create_session: bool,
    platform: Platform,
    /// A validated copy of [`RunnerConfig::compaction`] with the summarizer resolved; `None`
    /// when compaction is disabled.
    compaction: Option<Arc<gadk_session::compaction::Config>>,
}

/// Go `fmt.Errorf("<context>: %w", err)`.
#[derive(Debug)]
struct Wrapped {
    context: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// Go `fmt.Errorf("<context>: %w", err)`: `to_string()` is Go's `err.Error()` and the source chain
/// stays walkable (Go `errors.Is`).
pub(crate) fn wrap(
    context: impl Into<String>,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> AgentError {
    AgentError::Other(Box::new(Wrapped {
        context: context.into(),
        source: source.into(),
    }))
}

/// Go `errors.Is(err, agent.ErrLLMCallsLimitExceeded)`.
fn is_llm_calls_limit(err: &AgentError) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if matches!(
            e.downcast_ref::<AgentError>(),
            Some(AgentError::LlmCallsLimitExceeded { .. })
        ) {
            return true;
        }
        cur = e.source();
    }
    false
}

/// Port of Go `fromPlugin`: whatever the plugin returns, the framework's compaction record wins.
fn from_plugin(original: Event, modified: Option<Event>, record: Option<EventCompaction>) -> Event {
    let mut ev = modified.unwrap_or(original);
    ev.actions.compaction = record;
    ev
}

/// Outcome of the per-event pipeline shared by both run paths.
pub(crate) enum Processed {
    /// The (possibly plugin-replaced) event, persisted unless partial: yield it.
    Event(Box<Event>),
    /// An `on_event` plugin failed: yield the error and continue.
    PluginError(AgentError),
    /// Persisting failed: yield the error and end the run.
    AppendError(AgentError),
}

impl Runner {
    /// Port of Go `runner.New`. Go's nil-agent and nil-service checks have no Rust counterpart.
    pub fn new(cfg: RunnerConfig) -> Result<Runner, AgentError> {
        Self::new_with_summarizer_timeout(cfg, compaction::DEFAULT_SUMMARIZER_TIMEOUT)
    }

    /// [`Runner::new`] with the default summarizer's timeout overridden (a test seam).
    pub(crate) fn new_with_summarizer_timeout(
        cfg: RunnerConfig,
        summarizer_timeout: Duration,
    ) -> Result<Runner, AgentError> {
        let parents =
            ParentMap::new(&cfg.agent).map_err(|e| wrap("failed to create agent tree", e))?;
        let plugin_manager =
            PluginManager::new(cfg.plugin_config.plugins, cfg.plugin_config.close_timeout)
                .map_err(|e| wrap("failed to create plugin manager", e))?;
        let compaction = compaction::resolve_compaction_config(
            cfg.compaction.as_ref(),
            &cfg.agent,
            summarizer_timeout,
        )?;
        Ok(Runner {
            inner: Arc::new(RunnerInner {
                app_name: cfg.app_name,
                root_agent: cfg.agent,
                session_service: cfg.session_service,
                artifact_service: cfg.artifact_service,
                memory_service: cfg.memory_service,
                parents: Arc::new(parents),
                plugin_manager: Arc::new(plugin_manager),
                auto_create_session: cfg.auto_create_session,
                platform: cfg.platform,
                compaction,
            }),
        })
    }

    /// Port of Go `runner.NewInMemory`: in-memory session, artifact and memory services with
    /// session auto-creation, for local development and tests.
    pub fn in_memory(app_name: &str, agent: Arc<dyn Agent>) -> Result<Runner, AgentError> {
        Runner::new(RunnerConfig {
            app_name: app_name.to_string(),
            agent,
            session_service: Arc::new(gadk_session::InMemorySessionService::new()),
            artifact_service: Some(Arc::new(gadk_artifact::InMemoryArtifactService::new())),
            memory_service: Some(Arc::new(gadk_memory::InMemoryMemoryService::new())),
            plugin_config: PluginConfig::default(),
            auto_create_session: true,
            platform: Platform::default(),
            compaction: None,
        })
    }

    /// Port of Go `(*Runner).Run`: runs the agent for `msg` (`None` is Go's nil message) in the
    /// session, yielding its events. Dropping the returned stream cancels the invocation.
    ///
    /// With compaction configured, a sliding-window compaction runs after the invocation's
    /// events are persisted: inline when the stream ends (its error, if any, is the last item),
    /// or on a spawned task after the after-run callbacks when the stream is dropped early, in
    /// which case its error is logged. Dropping the stream does not cancel that task.
    ///
    /// The stream runs on the consumer's fiber ([`FiberStream::consumer`]: the one current at
    /// its first poll, else a new root it keeps), as Go's iterator runs on the goroutine that
    /// ranges over it; the after-run callbacks of a stream dropped early re-enter that fiber.
    pub fn run(
        &self,
        user_id: &str,
        session_id: &str,
        msg: Option<Content>,
        cfg: RunConfig,
        opts: RunOptions,
    ) -> EventStream {
        let inner = self.inner.clone();
        let root = inner.root_agent.clone();
        let stream = crate::telemetry::instrumentation::instrument_invocation(
            root.as_ref(),
            session_id,
            || inner.run_stream(user_id.to_string(), session_id.to_string(), msg, cfg, opts),
        );
        FiberStream::consumer(stream).boxed()
    }

    /// Closes every plugin (Go `PluginManager.Close` via the runner).
    pub async fn close(&self) -> Result<(), AgentError> {
        self.inner.plugin_manager.close().await
    }
}

impl RunnerInner {
    /// Port of Go `(*Runner).getOrCreateSession`: any `get` error creates the session when
    /// auto-create is on.
    async fn get_or_create_session(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Arc<dyn Session>, AgentError> {
        let got = self
            .session_service
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: user_id.into(),
                session_id: session_id.into(),
                ..Default::default()
            })
            .await;
        match got {
            Ok(s) => Ok(s),
            Err(e) if !self.auto_create_session => Err(e.into()),
            Err(_) => Ok(self
                .session_service
                .create(CreateRequest {
                    app_name: self.app_name.clone(),
                    user_id: user_id.into(),
                    session_id: Some(session_id.into()),
                    state: None,
                })
                .await?),
        }
    }

    /// Port of Go `(*Runner).run` (runner.go:547-795): an LlmAgent root takes the node path
    /// ([`run_node::run_node`]); any other root runs directly.
    fn run_stream(
        self: Arc<Self>,
        user_id: String,
        session_id: String,
        msg: Option<Content>,
        cfg: RunConfig,
        opts: RunOptions,
    ) -> EventStream {
        let this = self;
        Box::pin(async_stream::stream! {
            let stored = match this.get_or_create_session(&user_id, &session_id).await {
                Ok(s) => s,
                Err(e) => { yield Err(e); return; }
            };

            // Node path: an LlmAgent root (runner.go:581-642).
            if is_llm_agent(&*this.root_agent) {
                let Some(state) = reveal(&*this.root_agent) else {
                    yield Err(AgentError::Config(format!(
                        "agent {} is not an LlmAgent",
                        this.root_agent.name()
                    )));
                    return;
                };
                // An LlmAgent root must be in chat mode.
                let root_mode = resolve_mode(state.cfg.mode, Mode::Chat);
                if root_mode != Mode::Chat {
                    yield Err(AgentError::Config(format!(
                        "root agent {} must be a chat LlmAgent, but has mode {}",
                        this.root_agent.name(),
                        root_mode.as_str()
                    )));
                    return;
                }
                // A chat coordinator with task-mode sub-agents delegates through its own node;
                // the legacy sub-agent picker must not bypass it on resume.
                let has_task_sub_agent = this
                    .root_agent
                    .sub_agents()
                    .iter()
                    .any(|s| reveal(&**s).is_some_and(|st| st.cfg.mode == Mode::Task));
                let agent_to_run = if has_task_sub_agent {
                    this.root_agent.clone()
                } else {
                    match this.find_agent_to_run(&*stored, msg.as_ref()) {
                        Ok(a) => a,
                        Err(e) => { yield Err(e); return; }
                    }
                };
                let mut events =
                    run_node::run_node(this.clone(), stored, agent_to_run, root_mode, msg, cfg, opts);
                while let Some(item) = events.next().await {
                    yield item;
                }
                return;
            }

            // Agent path (runner.go:644-794): the root agent runs directly.
            let token = CancellationToken::new();
            let cancel_on_drop = token.clone().drop_guard();
            let ictx = this.new_invocation_context(
                &stored,
                this.root_agent.clone(),
                msg.clone(),
                &cfg,
                &token,
                true,
            );
            let appended = this
                .append_message_to_session(
                    ictx.into_context(),
                    &stored,
                    msg,
                    cfg.save_input_blobs_as_artifacts,
                    opts.state_delta.clone(),
                )
                .await;
            let ctx = match appended {
                Ok((ctx, _)) => ctx,
                Err(e) => { yield Err(e); return; }
            };
            let ictx = ctx.as_invocation().clone();
            let mut guard = compaction::InvocationGuard::new(this.clone(), stored.clone(), ictx.clone());
            guard.cancel_on_drop(cancel_on_drop);
            guard.defer_after_run();
            let mut early = false;
            'run: {
                match this.run_before_run_callback(&ictx).await {
                    Err(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); early = true; break 'run; }
                    Ok(Some(ev)) => { guard.at_yield(true); yield Ok(ev); guard.at_yield(false); early = true; break 'run; }
                    Ok(None) => {}
                }
                let mut events = this.root_agent.run(ictx.clone());
                while let Some(item) = events.next().await {
                    let event = match item {
                        Ok(ev) => ev,
                        Err(e) => {
                            // The model-call budget is spent for the whole invocation, so
                            // nothing after this can reach a model (adk-python parity).
                            let stop = is_llm_calls_limit(&e);
                            guard.mark_failed();
                            guard.at_yield(true);
                            yield Err(e);
                            guard.at_yield(false);
                            if stop {
                                early = true;
                                break 'run;
                            }
                            continue;
                        }
                    };
                    match this.process_event(&ictx, &*stored, event).await {
                        Processed::Event(ev) => { guard.at_yield(true); yield Ok(*ev); guard.at_yield(false); }
                        Processed::PluginError(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); }
                        Processed::AppendError(e) => { guard.mark_failed(); guard.at_yield(true); yield Err(e); guard.at_yield(false); early = true; break 'run; }
                    }
                }
            }
            if early {
                guard.finish_early().await;
            } else {
                // Compact once every event of the invocation is persisted; the consumer
                // drained the stream, so a failure can still be reported.
                if let Some(e) = guard.compact_inline().await {
                    guard.at_yield(true);
                    yield Err(e);
                    guard.at_yield(false);
                }
                guard.finish().await;
            }
        })
    }

    /// The per-event pipeline of runner.go:747-777 and run_node.go:202-230: clear a
    /// message-as-output event's output, run `on_event`, restore the compaction record, persist
    /// unless partial.
    pub(crate) async fn process_event(
        &self,
        ctx: &InvocationContext,
        stored: &dyn Session,
        mut event: Event,
    ) -> Processed {
        if !event.response.partial
            && event
                .node_info
                .as_ref()
                .is_some_and(|n| n.message_as_output)
            && event.response.content.is_some()
        {
            event.output = None;
        }
        // Captured before the plugin runs.
        let record = event.actions.compaction.clone();
        match self
            .plugin_manager
            .run_on_event(ctx.clone(), event.clone())
            .await
        {
            Err(e) => return Processed::PluginError(e),
            Ok(modified) => event = from_plugin(event, modified, record),
        }
        // Only non-partial events are committed to the session.
        if !event.response.partial
            && let Err(e) = self.session_service.append_event(stored, &mut event).await
        {
            return Processed::AppendError(wrap("failed to add event to session", e));
        }
        Processed::Event(Box::new(event))
    }

    /// Port of Go `(*Runner).runBeforeRunCallback`: processes and persists a plugin's
    /// replacement reply. `Ok(None)` means: run the agent.
    pub(crate) async fn run_before_run_callback(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Event>, AgentError> {
        let Some(content) = self.plugin_manager.run_before_run(ctx.clone()).await? else {
            return Ok(None);
        };
        let mut event = Event::new(ctx.platform(), ctx.invocation_id());
        // Match RunLive's attribution so later turns can resolve the agent from history.
        event.author = ctx.agent().name().to_string();
        event.response.content = Some(content);
        let record = event.actions.compaction.clone();
        let modified = self
            .plugin_manager
            .run_on_event(ctx.clone(), event.clone())
            .await?;
        let mut event = from_plugin(event, modified, record);
        // Stamped after on_event so a replacement keeps its before-run origin; the plugin's own
        // copy is not mutated (the event was moved/cloned out of it).
        event
            .response
            .custom_metadata
            .insert(BEFORE_RUN_REPLY_KEY.to_string(), Value::Bool(true));
        if !event.response.partial {
            // Go appends to ctx.Session(); writers always use the unwrapped session.
            let session = underlying_session(&ctx.session());
            self.session_service
                .append_event(&*session, &mut event)
                .await
                .map_err(|e| wrap("failed to add event to session", e))?;
        }
        Ok(Some(event))
    }

    /// Port of Go `(*Runner).appendMessageToSession`: runs `on_user_message`, saves inline blobs
    /// as artifacts when asked, and appends the user event. `None` (Go's nil message) appends
    /// nothing.
    pub(crate) async fn append_message_to_session(
        &self,
        mut ctx: Context,
        stored: &Arc<dyn Session>,
        msg: Option<Content>,
        save_input_blobs_as_artifacts: bool,
        state_delta: Option<Map<String, Value>>,
    ) -> Result<(Context, Option<Event>), AgentError> {
        let Some(mut msg) = msg else {
            return Ok((ctx, None));
        };
        let modified = self
            .plugin_manager
            .run_on_user_message(ctx.as_invocation().clone(), msg.clone())
            .await
            .map_err(|e| wrap("error running on run user message callback ", e))?;
        if let Some(m) = modified {
            msg = m;
            ctx = ctx.with_delta(ContextDelta {
                user_content: Some(Some(msg.clone())),
                ..Default::default()
            });
        }

        if let (Some(artifacts), true) = (ctx.artifacts(), save_input_blobs_as_artifacts) {
            let mut replaced = false;
            for i in 0..msg.parts.len() {
                if msg.parts[i].inline_data.is_none() {
                    continue;
                }
                let file_name = format!("artifact_{}_{}", ctx.invocation_id(), i);
                artifacts
                    .save(&file_name, msg.parts[i].clone())
                    .await
                    .map_err(|e| wrap(format!("failed to save artifact {file_name}"), e))?;
                // Replace the part with a text placeholder.
                msg.parts[i] = Part::from_text(format!(
                    "Uploaded file: {file_name}. It has been saved to the artifacts"
                ));
                replaced = true;
            }
            // Go mutates the message in place, which the context's UserContent aliases.
            if replaced {
                ctx = ctx.with_delta(ContextDelta {
                    user_content: Some(Some(msg.clone())),
                    ..Default::default()
                });
            }
        }

        let mut event = Event::new(ctx.platform(), ctx.invocation_id());
        event.author = "user".to_string();
        event.response.content = Some(msg);
        if let Some(delta) = state_delta {
            event.actions.state_delta = Some(delta);
        }
        if event.isolation_scope.is_empty() {
            let iso = find_active_task_isolation_scope(Some(&**stored));
            if !iso.is_empty() {
                event.isolation_scope = iso;
            }
        }
        self.session_service
            .append_event(&**stored, &mut event)
            .await
            .map_err(|e| wrap("failed to append event to sessionService", e))?;
        Ok((ctx, Some(event)))
    }
}

#[cfg(test)]
impl Runner {
    pub(crate) fn session_service(&self) -> Arc<dyn SessionService> {
        self.inner.session_service.clone()
    }
    pub(crate) fn has_artifact_service(&self) -> bool {
        self.inner.artifact_service.is_some()
    }
    pub(crate) fn has_memory_service(&self) -> bool {
        self.inner.memory_service.is_some()
    }
    pub(crate) fn auto_create_session(&self) -> bool {
        self.inner.auto_create_session
    }
}
