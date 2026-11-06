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

//! Port of ADK-Go agent/llmagent/llmagent.go (New, installTaskTools, run, maybeSaveOutputToState)
//! and the wrappedSession type of agent/llmagent/llm_agent_wrapper.go.
//!
//! `RunLive` is out of scope (1b-live). `FindAgent` is covered by the `Agent::find_agent`
//! provided method; `RunNode` is replaced by explicit `run_llm_agent_as_node` calls.

use std::any::Any;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentRunFn, AgentState, AgentType, EventStream,
    InvocationContext, Mode,
};
use gadk_session::{Event, Session, State};
use serde_json::{Map, Value};

use crate::llm::config::LlmAgentConfig;
use crate::llm::flow::Flow;
use crate::llm::mode::resolve_mode;
use crate::llm::processors::{default_request_processors, default_response_processors};
use crate::llm::state::{LlmAgentState, reveal};
use crate::tool::finish_task::FinishTaskTool;
use crate::tool::single_turn::SingleTurnTool;
use crate::tool::task_agent_tool::TaskAgentTool;
use crate::workflow::route::go_quote;

/// Port of Go agent/llmagent/llmagent.go New.
pub fn new(cfg: LlmAgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    // Go: State{Tools: slices.Clone(cfg.Tools), …}; Config = cfg.
    let state = Arc::new(LlmAgentState::new(cfg.clone()));

    let run_state = Arc::clone(&state);
    let run: AgentRunFn =
        Arc::new(move |ctx: InvocationContext| run_flow(Arc::clone(&run_state), ctx));

    let hook_state = Arc::clone(&state);
    let agent_state = AgentState {
        agent_type: AgentType::LlmAgent,
        config: Some(Arc::clone(&state) as Arc<dyn Any + Send + Sync>),
        // Go: state.OnBeforeAgentCallbackEvent = a.maybeSaveOutputToState. A before-agent
        // callback that returns content short-circuits the run, so its event never reaches
        // run_flow; this gives it the same output_key handling.
        on_before_agent_callback_event: Some(Arc::new(move |ev: &mut Event| {
            maybe_save_output_to_state(&hook_state, ev)
        })),
        live: None,
    };

    let agent = gadk_agent::agent::new_with_state(
        AgentConfig {
            name: cfg.name,
            description: cfg.description,
            sub_agents: cfg.sub_agents,
            before_agent_callbacks: cfg.before_agent_callbacks,
            after_agent_callbacks: cfg.after_agent_callbacks,
            run: Some(run),
        },
        agent_state,
    )
    .map_err(|e| AgentError::Config(format!("failed to create agent: {e}")))?;

    install_task_tools(&agent, &state)?;
    Ok(agent)
}

/// Port of Go agent/llmagent/llmagent.go installTaskTools.
///
/// Go's `NewSingleTurnTool` never fails and `NewTaskAgentTool` fails only for a nil agent, which
/// Rust cannot express, so those two Go error branches have no counterpart here.
fn install_task_tools(agent: &Arc<dyn Agent>, state: &LlmAgentState) -> Result<(), AgentError> {
    if state.cfg.mode == Mode::Task {
        let finish = FinishTaskTool::new(agent.as_ref()).map_err(|e| {
            AgentError::Config(format!(
                "installing `finish_task` tool for agent {}: {e}",
                go_quote(agent.name())
            ))
        })?;
        state.tools.write().push(finish);
    }
    for sub in agent.sub_agents() {
        let Some(sub_state) = reveal(sub.as_ref()) else {
            continue;
        };
        // A sub-agent that declares no mode is a chat peer, reached via transfer_to_agent
        // rather than through a tool on the parent. The declaration is read on purpose: this
        // decides the tree shape, not a per-run placement.
        match resolve_mode(sub_state.cfg.mode, Mode::Chat) {
            Mode::SingleTurn => state
                .tools
                .write()
                .push(SingleTurnTool::new(Arc::clone(sub))),
            Mode::Task => state
                .tools
                .write()
                .push(TaskAgentTool::new(Arc::clone(sub))),
            Mode::Unset | Mode::Chat => {}
        }
    }
    Ok(())
}

/// Port of Go agent/llmagent/llmagent.go (*llmAgent).run.
///
/// Go first rebinds `ctx`'s agent to itself; `CustomAgent::run` (built by `new_with_state`)
/// already does that before calling this closure, so the rebinding is omitted. `Flow::tools`
/// stays at its default: Go never sets `Flow.Tools` here (the tools processor fills it).
fn run_flow(state: Arc<LlmAgentState>, ctx: InvocationContext) -> EventStream {
    let cfg = &state.cfg;
    let flow = Arc::new(Flow {
        model: cfg.model.clone(),
        request_processors: default_request_processors(),
        response_processors: default_response_processors(),
        before_model_callbacks: cfg.before_model_callbacks.clone(),
        after_model_callbacks: cfg.after_model_callbacks.clone(),
        on_model_error_callbacks: cfg.on_model_error_callbacks.clone(),
        before_tool_callbacks: cfg.before_tool_callbacks.clone(),
        after_tool_callbacks: cfg.after_tool_callbacks.clone(),
        on_tool_error_callbacks: cfg.on_tool_error_callbacks.clone(),
        ..Default::default()
    });
    Box::pin(flow.run(ctx).map(move |item| {
        item.map(|mut ev| {
            maybe_save_output_to_state(&state, &mut ev);
            ev
        })
    }))
}

/// Port of Go agent/llmagent/llmagent.go (*llmAgent).maybeSaveOutputToState: saves the model
/// output to `state_delta[output_key]` when this agent authored a final text response. Never
/// touches `event.output` (the node wrapper stamps output).
pub(crate) fn maybe_save_output_to_state(state: &LlmAgentState, event: &mut Event) {
    if event.author != state.cfg.name {
        // Go TODO: log "Skipping output save for agent %s: event authored by %s"
        return;
    }
    if state.cfg.output_key.is_empty() || !event.is_final_response() {
        return;
    }
    let Some(content) = event.response.content.as_ref() else {
        return;
    };
    if content.parts.is_empty() {
        return;
    }
    let mut result = String::new();
    let mut has_text_part = false;
    for part in &content.parts {
        if !part.text.is_empty() && !part.thought {
            has_text_part = true;
            result.push_str(&part.text);
        }
    }
    if !has_text_part {
        return;
    }
    // Go TODO: output schema validation and unmarshalling. An all-whitespace result is the
    // empty final chunk of a stream; do not save it.
    if state.cfg.output_schema.is_some() && result.trim().is_empty() {
        return;
    }
    event
        .actions
        .state_delta
        .get_or_insert_with(Map::new)
        .insert(state.cfg.output_key.clone(), Value::String(result));
}

/// Port of Go agent/llmagent/llm_agent_wrapper.go wrappedSession: a read view of `inner` with
/// one synthetic seed event inserted at the event count snapshotted at creation. The seed is
/// never written to `inner`.
pub(crate) struct WrappedSession {
    inner: Arc<dyn Session>,
    seed: Event,
    insert_at: usize,
}

impl WrappedSession {
    /// Port of newWrappedSession.
    pub(crate) fn new(inner: Arc<dyn Session>, seed: Event) -> Self {
        let insert_at = inner.events().len();
        Self {
            inner,
            seed,
            insert_at,
        }
    }

    /// Port of (*wrappedSession).Unwrap.
    pub(crate) fn inner(&self) -> &Arc<dyn Session> {
        &self.inner
    }
}

impl Session for WrappedSession {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn state(&self) -> &dyn State {
        self.inner.state()
    }

    /// Port of (*wrappedEvents).All/Len/At (collapsed, as Rust `Session::events` returns a
    /// `Vec`): the seed sits at `insert_at`, or at the end when the underlying session has
    /// fewer events than at the snapshot.
    fn events(&self) -> Vec<Event> {
        let mut evs = self.inner.events();
        let at = self.insert_at.min(evs.len());
        evs.insert(at, self.seed.clone());
        evs
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.inner.last_update_time()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The session underneath any [`WrappedSession`] (Go `Unwrap`). Anything that must WRITE to the
/// session (e.g. `SessionService::append_event`, whose in-memory implementation downcasts to its
/// own type) must pass `underlying_session(&ctx.session())`, never the wrapped view.
pub(crate) fn underlying_session(session: &Arc<dyn Session>) -> Arc<dyn Session> {
    match session.as_any().downcast_ref::<WrappedSession>() {
        Some(w) => underlying_session(w.inner()),
        None => Arc::clone(session),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use gadk_genai::Content;
    use gadk_model::{Llm, LlmResponse};
    use serde_json::json;

    use crate::testutil::{MockModel, collect_events};

    #[test]
    fn task_mode_installs_finish_task_and_sub_agent_tools_in_order() {
        let st = new(LlmAgentConfig {
            name: "st".into(),
            mode: Mode::SingleTurn,
            ..Default::default()
        })
        .unwrap();
        let tk = new(LlmAgentConfig {
            name: "tk".into(),
            mode: Mode::Task,
            ..Default::default()
        })
        .unwrap();
        let chat = new(LlmAgentConfig {
            name: "chat".into(),
            ..Default::default()
        })
        .unwrap();
        let a = new(LlmAgentConfig {
            name: "root".into(),
            mode: Mode::Task,
            sub_agents: vec![st, tk, chat],
            ..Default::default()
        })
        .unwrap();
        let names: Vec<String> = reveal(a.as_ref())
            .unwrap()
            .tools
            .read()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(names, vec!["finish_task", "st", "tk"]);
        let tools = reveal(a.as_ref()).unwrap().tools.read().clone();
        assert!(
            tools[1].as_any().is::<SingleTurnTool>() && tools[2].as_any().is::<TaskAgentTool>()
        );
    }

    #[test]
    fn duplicate_sub_agent_is_rejected_with_go_prefix() {
        let s = new(LlmAgentConfig {
            name: "s".into(),
            ..Default::default()
        })
        .unwrap();
        let err = new(LlmAgentConfig {
            name: "p".into(),
            sub_agents: vec![s.clone(), s],
            ..Default::default()
        })
        .err()
        .unwrap();
        assert!(
            err.to_string().starts_with("failed to create agent: "),
            "{err}"
        );
    }

    #[test]
    fn agent_state_is_llm_agent_with_save_hook() {
        let a = new(LlmAgentConfig {
            name: "a".into(),
            output_key: "k".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(a.agent_state().agent_type, AgentType::LlmAgent);
        let hook = a
            .agent_state()
            .on_before_agent_callback_event
            .clone()
            .unwrap();
        let mut ev = Event {
            author: "a".into(),
            ..Default::default()
        };
        ev.response.content = Some(Content::from_text("cb text", "model"));
        hook(&mut ev);
        assert_eq!(
            ev.actions.state_delta.unwrap().get("k"),
            Some(&json!("cb text"))
        );
    }

    #[tokio::test]
    async fn run_drives_flow_and_saves_output_key() {
        let model = MockModel::new(vec![Ok(LlmResponse {
            content: Some(Content::from_text("hello", "model")),
            ..Default::default()
        })]);
        let a = new(LlmAgentConfig {
            name: "a".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            output_key: "out".into(),
            ..Default::default()
        })
        .unwrap();
        let ic = InvocationContext::builder()
            .agent(a.clone())
            .build_invocation();
        let events = collect_events(a.run(ic)).await.unwrap();
        let last = events.last().unwrap();
        assert_eq!(last.author, "a");
        assert_eq!(
            last.actions.state_delta.as_ref().unwrap().get("out"),
            Some(&json!("hello"))
        );
        assert_eq!(model.requests().len(), 1);
    }
}
