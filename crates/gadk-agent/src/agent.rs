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
    last_error: Option<String>,
}

/// Port of Go `telemetry.StartNodeSpan(OperationAgent)` (`internal/telemetry/node_tracing.go`):
/// `invoke_workflow` for workflow agents only, `invoke_agent` otherwise.
///
/// Go also sets `gen_ai.workflow.nested = true` on a workflow span started inside another workflow
/// span (its private workflow-scope context value). Rust contexts carry no such value, so the
/// attribute is omitted.
fn agent_run_span(agent: &CustomAgent, ctx: &InvocationContext) -> tracing::Span {
    use tracing::field::Empty;
    if agent.state.agent_type == AgentType::WorkflowAgent {
        tracing::info_span!(
            "invoke_workflow",
            otel.name = %format!("invoke_workflow {}", agent.name),
            gen_ai.operation.name = "invoke_workflow",
            gen_ai.workflow.name = %agent.name,
            gen_ai.conversation.id = %ctx.session_id(),
            otel.status_code = Empty,
            error.message = Empty,
        )
    } else {
        tracing::info_span!(
            "invoke_agent",
            otel.name = %format!("invoke_agent {}", agent.name),
            gen_ai.operation.name = "invoke_agent",
            gen_ai.agent.name = %agent.name,
            gen_ai.agent.description = %agent.description,
            gen_ai.conversation.id = %ctx.session_id(),
            gcp.vertex.agent.invocation_id = %ctx.invocation_id(),
            otel.status_code = Empty,
            error.message = Empty,
        )
    }
}

impl Drop for RunState {
    /// Go `WrapYield`'s deferred `endSpan`: the span's status follows the last yielded item
    /// (`TraceAgentResult` → `RecordError`), at the end of the stream or when it is dropped early.
    fn drop(&mut self) {
        if let Some(msg) = self.last_error.take() {
            self.span.record("otel.status_code", "ERROR");
            self.span.record("error.message", msg.as_str());
        }
    }
}

/// Remembers the item for the span's final status (Go `WrapYield` keeps the last value and error).
fn track(
    mut st: RunState,
    item: Result<Event, AgentError>,
) -> Option<(Result<Event, AgentError>, RunState)> {
    st.last_error = item.as_ref().err().map(ToString::to_string);
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
