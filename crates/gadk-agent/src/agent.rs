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

//! The agent interface and the one agent implementation every agent type builds on (port of Go
//! `agent/agent.go`).

use crate::go_fmt::go_quote;
use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use futures::StreamExt;
use gadk_genai::Content;
use gadk_session::{Event, EventActions};
use tracing::Instrument;

use crate::callback::{AfterAgentCallback, BeforeAgentCallback};
use crate::context::delta::new_actions;
use crate::context::{Context, ContextDelta, InvocationContext};
use crate::error::{AgentError, EventStream, wrap};
use crate::live::LiveSession;
use crate::state::{AgentState, AgentType};

/// Go's optional `RunLive` method (the runner's `liveAgent` and the sequential agent's
/// `liveRunner` type assertions). Agents expose it through [`Agent::as_live`]; `None` there means
/// the agent is not live-capable, and callers check that before doing anything else.
#[async_trait]
pub trait LiveAgent: Send + Sync {
    /// Starts a live run in `ctx`, whose current agent is the agent being run. Returns the
    /// session to send on and the event stream; dropping the stream ends the run.
    async fn run_live(
        &self,
        ctx: InvocationContext,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError>;
}

/// Go `agent.Agent`: the interface every agent implements.
///
/// Agents are created with ADK constructors ([`new`] and the engine's agent constructors) to ensure
/// correct initialisation and configuration.
pub trait Agent: Send + Sync + 'static {
    /// The agent's name, unique within the agent tree.
    fn name(&self) -> &str;
    /// The agent's description; an LLM uses it to decide whether to delegate to the agent.
    fn description(&self) -> &str;
    /// The child agents this agent can delegate to.
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    /// Runs the agent; dropping the stream cancels the run.
    fn run(&self, ctx: InvocationContext) -> EventStream;
    /// Go `FindAgent`: this agent or a descendant named `name`. The agent itself is found only for
    /// agents built by [`new`]/[`new_with_state`].
    fn find_agent(&self, name: &str) -> Option<Arc<dyn Agent>> {
        if self.name() == name {
            return self_arc_any(self.as_any());
        }
        self.find_sub_agent(name)
    }
    /// Go `FindSubAgent`: a descendant named `name`.
    fn find_sub_agent(&self, name: &str) -> Option<Arc<dyn Agent>> {
        self.sub_agents().iter().find_map(|s| s.find_agent(name))
    }
    /// Engine-owned state (Go `agent.internal().State`).
    #[doc(hidden)]
    fn agent_state(&self) -> &AgentState;

    /// The live entry point (Go `RunLive`), or `None` when the agent is not live-capable.
    ///
    /// The context passed to the returned [`LiveAgent::run_live`] must have this agent as
    /// `ctx.agent()`: live agents are stateless and resolve the agent to run from the context,
    /// not from the agent `as_live` was called on.
    fn as_live(&self) -> Option<&dyn LiveAgent> {
        None
    }
    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;
}

/// The function that defines a custom agent's behavior.
pub type AgentRunFn = Arc<dyn Fn(InvocationContext) -> EventStream + Send + Sync>;

/// Go `agent.Config`: the configuration of a custom agent.
#[derive(Default, Clone)]
pub struct AgentConfig {
    /// Must be non-empty and unique within the agent tree; `"user"` is reserved for end-user input.
    pub name: String,
    /// The agent's capability; an LLM uses it to decide whether to delegate to the agent.
    pub description: String,
    /// The child agents this agent can delegate tasks to.
    pub sub_agents: Vec<Arc<dyn Agent>>,
    /// Called sequentially before the agent starts its run; see [`BeforeAgentCallback`].
    pub before_agent_callbacks: Vec<BeforeAgentCallback>,
    /// Called sequentially after the agent completes its run; see [`AfterAgentCallback`].
    pub after_agent_callbacks: Vec<AfterAgentCallback>,
    /// The agent's behavior. An agent without one can be built, for example as a node in an agent
    /// tree, but running it yields an error.
    pub run: Option<AgentRunFn>,
}

/// Go `agent` struct: every agent type is this one struct, with engine state in
/// `AgentState::config`.
pub(crate) struct CustomAgent {
    self_weak: Weak<CustomAgent>,
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    before_agent_callbacks: Vec<BeforeAgentCallback>,
    after_agent_callbacks: Vec<AfterAgentCallback>,
    run: Option<AgentRunFn>,
    state: AgentState,
}

impl CustomAgent {
    /// Infallible constructor (no validation), via `Arc::new_cyclic` for the weak self-reference.
    pub(crate) fn build(cfg: AgentConfig, state: AgentState) -> Arc<CustomAgent> {
        Arc::new_cyclic(|w| CustomAgent {
            self_weak: w.clone(),
            name: cfg.name,
            description: cfg.description,
            sub_agents: cfg.sub_agents,
            before_agent_callbacks: cfg.before_agent_callbacks,
            after_agent_callbacks: cfg.after_agent_callbacks,
            run: cfg.run,
            state,
        })
    }
}

/// Go `agent.New`: an agent with custom logic defined by `cfg.run`.
pub fn new(cfg: AgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    new_with_state(
        cfg,
        AgentState {
            agent_type: AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: None,
        },
    )
}

/// [`new`] with engine-owned state (the engine's agent constructors).
#[doc(hidden)]
pub fn new_with_state(cfg: AgentConfig, state: AgentState) -> Result<Arc<dyn Agent>, AgentError> {
    let mut seen = HashSet::new();
    for sub in &cfg.sub_agents {
        if !seen.insert(Arc::as_ptr(sub).cast::<()>() as usize) {
            return Err(AgentError::Config(format!(
                "error creating agent: subagent {} appears multiple times in subAgents",
                go_quote(sub.name())
            )));
        }
    }
    Ok(CustomAgent::build(cfg, state))
}

/// The `Arc` of an agent built by [`new`]/[`new_with_state`] (`None` for foreign implementations).
pub fn self_arc(agent: &dyn Agent) -> Option<Arc<dyn Agent>> {
    self_arc_any(agent.as_any())
}

fn self_arc_any(any: &dyn Any) -> Option<Arc<dyn Agent>> {
    let a: Arc<dyn Agent> = any.downcast_ref::<CustomAgent>()?.self_weak.upgrade()?;
    Some(a)
}

impl Agent for CustomAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    /// Port of Go `(*agent).Run`: before-agent callbacks, the run function, after-agent callbacks,
    /// inside the agent span.
    fn run(&self, ctx: InvocationContext) -> EventStream {
        let Some(me) = self.self_weak.upgrade() else {
            let name = self.name.clone();
            return futures::stream::once(async move {
                Err(AgentError::Config(format!(
                    "agent {} is being dropped",
                    go_quote(&name)
                )))
            })
            .boxed();
        };
        let span = agent_run_span(&me, &ctx);
        let me_dyn: Arc<dyn Agent> = me.clone();
        // Go: icDelta{Agent: &aa, Context: &spanCtx}; PromoteWithDelta forks the ended flag.
        let node_ctx = ctx.into_context().with_delta(ContextDelta {
            agent: Some(me_dyn),
            ..Default::default()
        });
        let st = RunState {
            agent: me,
            ctx: node_ctx,
            phase: Phase::Before,
            pending: VecDeque::new(),
            span: span.clone(),
            last_error: None,
        };
        futures::stream::unfold(st, move |st| step(st).instrument(span.clone())).boxed()
    }

    fn agent_state(&self) -> &AgentState {
        &self.state
    }

    fn as_live(&self) -> Option<&dyn LiveAgent> {
        self.state.live.as_deref()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

enum Phase {
    Before,
    StartMain,
    Main(EventStream),
    PostMain,
    After,
    Done,
}

struct RunState {
    agent: Arc<CustomAgent>,
    ctx: Context,
    phase: Phase,
    pending: VecDeque<Result<Event, AgentError>>,
    span: tracing::Span,
    /// The last item's error: Go `err.Error()` and `errorType(err)`.
    last_error: Option<(String, String)>,
}

/// Port of Go `telemetry.StartNodeSpan(OperationAgent)` (`internal/telemetry/node_tracing.go`):
/// `invoke_workflow` for workflow agents only, `invoke_agent` otherwise. A workflow span started
/// inside another `invoke_workflow` span gets `gen_ai.workflow.nested = true` (Go
/// `startInvokeWorkflowSpan`'s workflow-scope context value; default schema only).
fn agent_run_span(agent: &CustomAgent, ctx: &InvocationContext) -> tracing::Span {
    use tracing::field::Empty;
    if agent.state.agent_type == AgentType::WorkflowAgent {
        let nested = telemetry::in_workflow_scope() && !telemetry::use_legacy_schema();
        let span = tracing::info_span!(
            "invoke_workflow",
            otel.name = %format!("invoke_workflow {}", agent.name),
            gen_ai.operation.name = "invoke_workflow",
            gen_ai.workflow.name = %agent.name,
            gen_ai.conversation.id = %ctx.session_id(),
            gen_ai.workflow.nested = Empty,
            error.type = Empty,
            otel.status_code = Empty,
            error.message = Empty,
        );
        if nested {
            span.record("gen_ai.workflow.nested", true);
        }
        span
    } else {
        tracing::info_span!(
            "invoke_agent",
            otel.name = %format!("invoke_agent {}", agent.name),
            gen_ai.operation.name = "invoke_agent",
            gen_ai.agent.name = %agent.name,
            gen_ai.agent.description = %agent.description,
            gen_ai.conversation.id = %ctx.session_id(),
            gcp.vertex.agent.invocation_id = %ctx.invocation_id(),
            error.type = Empty,
            otel.status_code = Empty,
            error.message = Empty,
        )
    }
}

impl Drop for RunState {
    /// Go `WrapYield`'s deferred `endSpan`: the span's status follows the last yielded item
    /// (`TraceAgentResult` → `RecordError`: status, description and, in the default schema,
    /// `error.type`), at the end of the stream or when it is dropped early.
    fn drop(&mut self) {
        if let Some((msg, error_type)) = self.last_error.take() {
            self.span.record("otel.status_code", "ERROR");
            self.span.record("error.message", msg.as_str());
            if !telemetry::use_legacy_schema() {
                self.span.record("error.type", error_type.as_str());
            }
        }
    }
}

/// Remembers the item for the span's final status (Go `WrapYield` keeps the last value and error).
fn track(
    mut st: RunState,
    item: Result<Event, AgentError>,
) -> Option<(Result<Event, AgentError>, RunState)> {
    st.last_error = item
        .as_ref()
        .err()
        .map(|e| (e.to_string(), telemetry::error_type(e)));
    Some((item, st))
}

async fn step(mut st: RunState) -> Option<(Result<Event, AgentError>, RunState)> {
    loop {
        if let Some(item) = st.pending.pop_front() {
            return track(st, item);
        }
        match std::mem::replace(&mut st.phase, Phase::Done) {
            Phase::Before => {
                match run_before_agent_callbacks(&st.agent, &st.ctx).await {
                    Ok(Some(mut ev)) => {
                        if let Some(hook) = &st.agent.state.on_before_agent_callback_event {
                            hook(&mut ev);
                        }
                        st.pending.push_back(Ok(ev));
                    }
                    Ok(None) => {}
                    // Go yields (nil, err) and keeps going if the caller does.
                    Err(e) => st.pending.push_back(Err(e)),
                }
                // The ended check runs after the consumer took the pending item, as in Go.
                st.phase = Phase::StartMain;
            }
            Phase::StartMain => {
                if st.ctx.ended() {
                    return None;
                }
                match st.agent.run.clone() {
                    // Matches adk-python: an agent without a run function is built fine and fails
                    // when run.
                    None => st.pending.push_back(Err(AgentError::Config(format!(
                        "agent {} has no Run function",
                        go_quote(&st.agent.name)
                    )))),
                    Some(run) => st.phase = Phase::Main(run(st.ctx.as_invocation().clone())),
                }
            }
            Phase::Main(mut inner) => match inner.next().await {
                Some(Ok(mut ev)) => {
                    if ev.author.is_empty() {
                        ev.author = author_for_event(&st.agent, &ev);
                    }
                    st.phase = Phase::Main(inner);
                    return track(st, Ok(ev));
                }
                Some(Err(e)) => {
                    st.phase = Phase::Main(inner);
                    return track(st, Err(e));
                }
                None => st.phase = Phase::PostMain,
            },
            Phase::PostMain => {
                if st.ctx.ended() {
                    return None;
                }
                st.phase = Phase::After;
            }
            Phase::After => match run_after_agent_callbacks(&st.agent, &st.ctx).await {
                Ok(Some(ev)) => st.pending.push_back(Ok(ev)),
                Ok(None) => {}
                Err(e) => st.pending.push_back(Err(e)),
            },
            Phase::Done => return None,
        }
    }
}

/// Go `getAuthorForEvent`.
fn author_for_event(agent: &CustomAgent, ev: &Event) -> String {
    match &ev.response.content {
        Some(c) if c.role == gadk_genai::ROLE_USER => gadk_genai::ROLE_USER.to_string(),
        _ => agent.name.clone(),
    }
}

/// Go `eventActionsFrom`: the actions a callback accumulated, less the fields that are the
/// framework's to write rather than a callback's.
fn event_actions_from(a: &EventActions) -> EventActions {
    EventActions {
        compaction: None,
        ..a.clone()
    }
}

/// The callback event (Go `session.NewEvent` + content, author, branch and the callback's actions).
fn callback_event(
    agent: &CustomAgent,
    ctx: &Context,
    actions: &parking_lot::Mutex<EventActions>,
    content: Option<Content>,
) -> Event {
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.response.content = content;
    ev.author = agent.name.clone();
    ev.branch = ctx.branch();
    ev.actions = event_actions_from(&actions.lock());
    ev
}

/// Whether the callback recorded a state or artifact delta.
fn has_delta(actions: &parking_lot::Mutex<EventActions>) -> bool {
    let a = actions.lock();
    a.state_delta.as_ref().is_some_and(|d| !d.is_empty())
        || a.artifact_delta.as_ref().is_some_and(|d| !d.is_empty())
}

/// Port of Go `runBeforeAgentCallbacks`: `Some` content from a plugin or a callback skips the agent
/// run and is returned as an event; otherwise a recorded delta is returned as an event.
async fn run_before_agent_callbacks(
    agent: &CustomAgent,
    ctx: &Context,
) -> Result<Option<Event>, AgentError> {
    let actions = new_actions();
    let cb_ctx = ctx.with_delta(ContextDelta {
        actions: Some(actions.clone()),
        ..Default::default()
    });

    if let Some(pm) = ctx.plugin_manager() {
        let content = pm
            .run_before_agent(cb_ctx.clone())
            .await
            .map_err(|e| wrap("failed to run plugin before agent callback", e))?;
        if let Some(c) = content {
            let ev = callback_event(agent, ctx, &actions, Some(c));
            ctx.end_invocation();
            return Ok(Some(ev));
        }
    }

    for callback in &agent.before_agent_callbacks {
        let content = callback(cb_ctx.clone())
            .await
            .map_err(|e| wrap("failed to run before agent callback", e))?;
        let Some(c) = content else { continue };
        let ev = callback_event(agent, ctx, &actions, Some(c));
        ctx.end_invocation();
        return Ok(Some(ev));
    }

    // check if has delta create event with it
    if has_delta(&actions) {
        return Ok(Some(callback_event(agent, ctx, &actions, None)));
    }
    Ok(None)
}

/// Port of Go `runAfterAgentCallbacks`: `Some` content from a plugin or a callback, or a recorded
/// delta, is returned as an event.
async fn run_after_agent_callbacks(
    agent: &CustomAgent,
    ctx: &Context,
) -> Result<Option<Event>, AgentError> {
    let actions = new_actions();
    let cb_ctx = ctx.with_delta(ContextDelta {
        actions: Some(actions.clone()),
        ..Default::default()
    });

    if let Some(pm) = ctx.plugin_manager() {
        let content = pm
            .run_after_agent(cb_ctx.clone())
            .await
            .map_err(|e| wrap("failed to run plugin after agent callback", e))?;
        if let Some(c) = content {
            return Ok(Some(callback_event(agent, ctx, &actions, Some(c))));
        }
    }

    for callback in &agent.after_agent_callbacks {
        let content = callback(cb_ctx.clone())
            .await
            .map_err(|e| wrap("failed to run after agent callback", e))?;
        let Some(c) = content else { continue };
        // Deliberately not ending the invocation here, which matches the Python ADK. Ended reports
        // whether the invocation should stop early, and nothing reads it after this point: running
        // the after-agent callbacks is the last step of the agent's Run loop. The flag is also local
        // to this agent's context and never propagates to a parent, so setting it here would imply
        // an effect it does not have.
        return Ok(Some(callback_event(agent, ctx, &actions, Some(c))));
    }

    // check if has delta create event with it
    if has_delta(&actions) {
        return Ok(Some(callback_event(agent, ctx, &actions, None)));
    }
    Ok(None)
}

/// The parts of Go `internal/telemetry` that gadk-agent's agent spans and gadk-engine's
/// `telemetry` module share (gadk-engine depends on gadk-agent): the environment provider and
/// the legacy-schema switch, Go `errorType`, and the workflow scope of Go
/// `startInvokeWorkflowSpan`. Applications install the provider with
/// `gadk_engine::telemetry::set_env_provider`; the rest is the engine's.
#[doc(hidden)]
pub mod telemetry {
    use std::sync::{Arc, OnceLock};

    use gadk_model::ModelError;
    use gadk_platform::env::{EnvProvider, ProcessEnv};
    use gadk_platform::go_to_lower;
    use parking_lot::RwLock;
    use tracing_subscriber::registry::LookupSpan;

    use crate::error::AgentError;

    /// Go `adkTelemetrySchemaVersionOptIn`: `otel_semconv_1_36` or `1` selects the legacy (1.36)
    /// schema; anything else the default (1.44).
    pub const SCHEMA_VERSION_OPT_IN_ENV_VAR: &str = "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN";
    /// Go `otelSemconv136`.
    pub const OTEL_SEMCONV_1_36: &str = "otel_semconv_1_36";
    /// Go `errorType`'s value for an error that is not a `genai.APIError`: the type of a Go
    /// `errors.New` error, which is what an ADK-Go error reaching a span usually is.
    pub const GO_ERROR_STRING_TYPE: &str = "*errors.errorString";

    static PROVIDER: RwLock<Option<Arc<dyn EnvProvider>>> = RwLock::new(None);

    /// Stores the process-wide telemetry environment. Call
    /// `gadk_engine::telemetry::set_env_provider`, which also drops the engine's cached
    /// content-capture mode.
    pub fn set_env_provider(env: Arc<dyn EnvProvider>) {
        *PROVIDER.write() = Some(env);
    }

    /// The installed provider ([`ProcessEnv`] when none was installed).
    pub fn env_provider() -> Arc<dyn EnvProvider> {
        PROVIDER
            .read()
            .clone()
            .unwrap_or_else(|| Arc::new(ProcessEnv))
    }

    /// Go `useLegacySchema`: whether `ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN` selects the 1.36
    /// schema; read on every call.
    pub fn use_legacy_schema() -> bool {
        let v = env_provider()
            .var(SCHEMA_VERSION_OPT_IN_ENV_VAR)
            .unwrap_or_default();
        matches!(go_to_lower(v.trim()).as_str(), OTEL_SEMCONV_1_36 | "1")
    }

    /// Names the Go dynamic type of an error crate gadk-agent cannot see (Go `%T`); `None` for
    /// an error it does not name.
    pub type GoErrorTypeResolver = fn(&(dyn std::error::Error + 'static)) -> Option<&'static str>;

    static GO_ERROR_TYPE_RESOLVER: OnceLock<GoErrorTypeResolver> = OnceLock::new();

    /// Installs the resolver [`error_type`] asks for the Go type of the error itself (the first
    /// one installed wins). gadk-engine installs one when it builds a `workflow.NodeRunError`.
    pub fn set_go_error_type_resolver(f: GoErrorTypeResolver) {
        let _ = GO_ERROR_TYPE_RESOLVER.set(f);
    }

    /// Go `errorType`: an API error's HTTP code (Go `errors.As(err, &genai.APIError{})`, through
    /// wrapping), else the Go type of the error itself (Go `semconv.ErrorType`'s `%T`) when the
    /// installed resolver names it (seen through [`AgentError::Workflow`], which is transparent),
    /// else [`GO_ERROR_STRING_TYPE`] (a Rust error has no Go type; an ADK-Go error reaching a span
    /// is usually an `errors.New` one).
    pub fn error_type(err: &(dyn std::error::Error + 'static)) -> String {
        if let Some(code) = api_error_code(err) {
            return code;
        }
        let top = match err.downcast_ref::<AgentError>() {
            Some(AgentError::Workflow(inner)) => &**inner as &(dyn std::error::Error + 'static),
            _ => err,
        };
        if let Some(t) = GO_ERROR_TYPE_RESOLVER.get().and_then(|f| f(top)) {
            return t.to_owned();
        }
        GO_ERROR_STRING_TYPE.to_owned()
    }

    /// Go `errors.As(err, &genai.APIError{})`: the HTTP code of the first API error in `err`'s
    /// chain.
    fn api_error_code(err: &(dyn std::error::Error + 'static)) -> Option<String> {
        let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
        while let Some(e) = cur {
            let model = e.downcast_ref::<ModelError>().or_else(|| {
                e.downcast_ref::<AgentError>().and_then(|a| match a {
                    AgentError::Model(m) => Some(m),
                    _ => None,
                })
            });
            if let Some(ModelError::Api { code, .. }) = model {
                return Some(code.to_string());
            }
            cur = e.source();
        }
        None
    }

    /// Whether the current span is inside an `invoke_workflow` span (Go's `workflowScopeKey`
    /// context value, which `startInvokeWorkflowSpan` sets). Read from the subscriber's span
    /// registry; `false` under a subscriber that is not a `tracing_subscriber::Registry` stack.
    pub fn in_workflow_scope() -> bool {
        let Some(id) = tracing::Span::current().id() else {
            return false;
        };
        tracing::dispatcher::get_default(|d| {
            let Some(registry) = d.downcast_ref::<tracing_subscriber::Registry>() else {
                return false;
            };
            registry
                .span(&id)
                .is_some_and(|s| s.scope().any(|s| s.name() == "invoke_workflow"))
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod live_tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{AgentConfig, LiveAgent, new, new_with_state};
    use crate::context::InvocationContext;
    use crate::error::{AgentError, EventStream};
    use crate::live::LiveSession;
    use crate::state::{AgentState, AgentType};

    struct NoopLive;

    #[async_trait]
    impl LiveAgent for NoopLive {
        async fn run_live(
            &self,
            _ctx: InvocationContext,
        ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
            Err(AgentError::other("noop live"))
        }
    }

    #[test]
    fn as_live_is_none_for_agent_new() {
        let a = new(AgentConfig {
            name: "plain".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(
            a.as_live().is_none(),
            "gadk_agent::new agents are not live-capable"
        );
    }

    #[tokio::test]
    async fn as_live_returns_the_agent_state_hook() {
        let a = new_with_state(
            AgentConfig {
                name: "live".into(),
                ..Default::default()
            },
            AgentState {
                agent_type: AgentType::CustomAgent,
                config: None,
                on_before_agent_callback_event: None,
                live: Some(Arc::new(NoopLive)),
            },
        )
        .unwrap();
        let live = a
            .as_live()
            .expect("AgentState.live makes the agent live-capable");
        match live
            .run_live(InvocationContext::builder().build_invocation())
            .await
        {
            Err(e) => assert_eq!(e.to_string(), "noop live"),
            Ok(_) => panic!("NoopLive must fail"),
        }
        // The trait is object-safe and shareable.
        let _shared: Arc<dyn LiveAgent> = Arc::new(NoopLive);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod span_tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use futures::StreamExt;
    use gadk_model::ModelError;
    use gadk_platform::env::MapEnv;
    use parking_lot::Mutex;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

    use super::telemetry::{OTEL_SEMCONV_1_36, SCHEMA_VERSION_OPT_IN_ENV_VAR, set_env_provider};
    use super::{AgentConfig, new, new_with_state};
    use crate::context::InvocationContext;
    use crate::error::AgentError;
    use crate::state::{AgentState, AgentType};

    /// Serialises the tests that install a telemetry environment (held across awaits).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    type Spans = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

    struct Capture(Spans);
    struct Fields<'a>(&'a mut BTreeMap<String, String>);
    impl Visit for Fields<'_> {
        fn record_str(&mut self, f: &Field, v: &str) {
            self.0.insert(f.name().to_owned(), v.to_owned());
        }
        fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
            self.0.insert(f.name().to_owned(), format!("{v:?}"));
        }
    }
    struct Idx(usize);
    impl<S> Layer<S> for Capture
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
            let mut fields = BTreeMap::new();
            attrs.record(&mut Fields(&mut fields));
            let mut spans = self.0.lock();
            spans.push((attrs.metadata().name().to_owned(), fields));
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(Idx(spans.len() - 1));
            }
        }
        fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
            let Some(i) = ctx
                .span(id)
                .and_then(|s| s.extensions().get::<Idx>().map(|i| i.0))
            else {
                return;
            };
            if let Some((_, fields)) = self.0.lock().get_mut(i) {
                values.record(&mut Fields(fields));
            }
        }
    }

    /// Installs (once per process) a no-op global `Registry`, so the global max-level hint and
    /// callsite interest never collapse to "off" while thread-default capture subscribers come
    /// and go on parallel test threads (as gadk-engine's `span_digest::pin_tracing_interest`).
    fn pin_tracing_interest() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
    }

    /// Runs `agent` to completion under `schema`, inside an `invoke_workflow` span when `in_workflow`,
    /// and returns the spans' names and recorded fields.
    async fn run_spans(
        agent: Arc<dyn super::Agent>,
        schema: &str,
        in_workflow: bool,
    ) -> Vec<(String, BTreeMap<String, String>)> {
        let _lock = ENV_LOCK.lock().await;
        set_env_provider(Arc::new(MapEnv(
            [(SCHEMA_VERSION_OPT_IN_ENV_VAR.to_owned(), schema.to_owned())].into(),
        )));
        pin_tracing_interest();
        let spans: Spans = Arc::default();
        let sub = tracing_subscriber::registry().with(Capture(spans.clone()));
        {
            let _g = tracing::subscriber::set_default(sub);
            let outer = in_workflow.then(|| tracing::info_span!("invoke_workflow"));
            let _entered = outer.as_ref().map(tracing::Span::enter);
            let ctx = InvocationContext::builder()
                .agent(agent.clone())
                .build_invocation();
            let mut s = agent.run(ctx);
            while s.next().await.is_some() {}
        }
        set_env_provider(Arc::new(MapEnv::default()));
        spans.lock().clone()
    }

    fn failing(err: AgentError, state: Option<AgentState>) -> Arc<dyn super::Agent> {
        let err = Arc::new(Mutex::new(Some(err)));
        let cfg = AgentConfig {
            name: "a".into(),
            run: Some(Arc::new(move |_| {
                let e = err.lock().take();
                Box::pin(futures::stream::iter(e.map(Err)))
            })),
            ..Default::default()
        };
        match state {
            Some(st) => new_with_state(cfg, st).unwrap(),
            None => new(cfg).unwrap(),
        }
    }

    fn workflow_state() -> AgentState {
        AgentState {
            agent_type: AgentType::WorkflowAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: None,
        }
    }

    /// Go `TraceAgentResult` → `RecordError`: a failed agent span gets `error.type` (Go
    /// `errorType`) in the default schema only.
    #[tokio::test]
    async fn agent_span_records_error_type_in_the_default_schema() {
        for (schema, err, want) in [
            (
                "otel_semconv_1_44",
                AgentError::Config("boom".into()),
                Some("*errors.errorString"),
            ),
            (
                "otel_semconv_1_44",
                AgentError::Model(ModelError::Api {
                    code: 429,
                    message: "quota".into(),
                    status: String::new(),
                    details: vec![],
                }),
                Some("429"),
            ),
            (OTEL_SEMCONV_1_36, AgentError::Config("boom".into()), None),
        ] {
            let spans = run_spans(failing(err, None), schema, false).await;
            let (name, fields) = &spans[0];
            assert_eq!(name, "invoke_agent");
            assert_eq!(
                fields.get("otel.status_code").map(String::as_str),
                Some("ERROR")
            );
            assert_eq!(
                fields.get("error.type").map(String::as_str),
                want,
                "{schema}"
            );
        }
        let spans = run_spans(
            failing(AgentError::Config("x".into()), Some(workflow_state())),
            "",
            false,
        )
        .await;
        assert_eq!(spans[0].0, "invoke_workflow");
        assert_eq!(
            spans[0].1.get("error.type").map(String::as_str),
            Some("*errors.errorString")
        );
    }

    /// Go `startInvokeWorkflowSpan`: a workflow agent's span inside another `invoke_workflow` span
    /// is marked nested, in the default schema only.
    #[tokio::test]
    async fn workflow_agent_span_is_nested_inside_a_workflow_span() {
        for (schema, in_workflow, want) in [
            ("", true, Some("true")),
            ("", false, None),
            (OTEL_SEMCONV_1_36, true, None),
        ] {
            let agent = failing(AgentError::Config("x".into()), Some(workflow_state()));
            let spans = run_spans(agent, schema, in_workflow).await;
            let (_, fields) = spans.last().unwrap();
            assert_eq!(
                fields.get("gen_ai.workflow.nested").map(String::as_str),
                want,
                "{schema:?} in_workflow={in_workflow}"
            );
        }
    }
}
