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

//! A workflow graph as an agent.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, AgentState, AgentType, BeforeAgentCallback,
    EventStream, InvocationContext,
};
use gadk_genai::FunctionResponse;
use serde_json::{Map, Value};

use crate::tool::utils::function_responses;
use crate::workflow::{
    Edge, RunState, WORKFLOW_INPUT_FUNCTION_CALL_NAME, Workflow, WorkflowOptions,
};

/// The configuration for creating a new workflow agent.
#[derive(Clone, Default)]
pub struct WorkflowAgentConfig {
    /// The agent's (and workflow's) name.
    pub name: String,
    /// The agent's description.
    pub description: String,
    /// The child agents this agent can delegate to.
    pub sub_agents: Vec<Arc<dyn Agent>>,
    /// The workflow graph's edges.
    pub edges: Vec<Edge>,
    /// Called sequentially before the agent starts its run.
    pub before_agent_callbacks: Vec<BeforeAgentCallback>,
    /// Called sequentially after the agent completes its run.
    pub after_agent_callbacks: Vec<AfterAgentCallback>,
}

/// A workflow agent. One returned agent can serve many concurrent sessions: the per-invocation
/// run state is rebuilt from session event history on a resume turn rather than held on the
/// agent. A paused workflow resumes on a follow-up turn when the user submits a FunctionResponse
/// targeting the interrupt id emitted by the paused node.
pub fn new(cfg: WorkflowAgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    // A graph that fails validation is reported as-is.
    let wf = Workflow::new(&cfg.name, cfg.edges.clone(), WorkflowOptions::default())?;
    gadk_agent::new_with_state(
        AgentConfig {
            name: cfg.name.clone(),
            description: cfg.description.clone(),
            sub_agents: cfg.sub_agents.clone(),
            before_agent_callbacks: cfg.before_agent_callbacks.clone(),
            after_agent_callbacks: cfg.after_agent_callbacks.clone(),
            run: Some(Arc::new(move |ctx: InvocationContext| run(wf.clone(), ctx))),
        },
        // The agent type makes the agent run open an "invoke_workflow <name>" span.
        AgentState {
            agent_type: AgentType::WorkflowAgent,
            config: Some(Arc::new(cfg)),
            on_before_agent_callback_event: None,
            live: None,
        },
    )
}

/// Runs `Workflow::resume` when the inbound user content answers a previously emitted input
/// request, `Workflow::run` on every other turn.
fn run(workflow: Arc<Workflow>, ctx: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        match detect_resume(&workflow, &ctx) {
            Err(e) => yield Err(e),
            Ok(Some((responses, state))) => {
                // Resume takes the invocation context as a plain `Context`.
                let mut s = workflow.resume(ctx.clone().into_context(), state, responses);
                while let Some(item) = s.next().await {
                    yield item;
                }
            }
            Ok(None) => {
                let mut s = workflow.run(ctx.clone());
                while let Some(item) = s.next().await {
                    yield item;
                }
            }
        }
    })
}

/// A resume turn: the responses keyed by interrupt id, and the reconstructed run state.
type Resume = (Map<String, Value>, RunState);

/// The responses keyed by interrupt id and the run state reconstructed from session history when
/// this turn is a resume; `None` for a fresh turn.
fn detect_resume(
    workflow: &Workflow,
    ctx: &InvocationContext,
) -> Result<Option<Resume>, AgentError> {
    let frs = function_responses(ctx.user_content());
    if frs.is_empty() {
        return Ok(None);
    }
    let mut responses = Map::new();
    for fr in frs {
        if fr.name != WORKFLOW_INPUT_FUNCTION_CALL_NAME {
            continue;
        }
        responses.insert(fr.id.clone(), decode_workflow_input_response(fr));
    }
    if responses.is_empty() {
        return Ok(None);
    }
    // Scope rehydration to this run's invocation (the runner reuses the paused run's id on
    // resume) so a prior completed run in the same session does not leak in. A bad resume (e.g.
    // failed schema validation) must fail, not silently fall through to a fresh run.
    let session = ctx.session();
    let Some(state) = workflow.reconstruct_run_state(&*session, ctx.invocation_id())? else {
        return Ok(None);
    };
    // The runner has already persisted this turn's answers. Rehydration unwraps and validates
    // them; the ingress decoder must not replace those values with a differently decoded copy.
    for ns in state.nodes.values() {
        for (id, response) in ns.resumed_inputs.iter().flatten() {
            if responses.contains_key(id) {
                responses.insert(id.clone(), response.clone());
            }
        }
    }
    Ok(Some((responses, state)))
}

/// The user-supplied payload of a FunctionResponse targeting a workflow input request. Three
/// accepted shapes, in priority order:
///
/// 1. `{"response": <value>}`: a string value is parsed as JSON and the result returned (the
///    string verbatim when it is not valid JSON); any other value is returned as-is.
/// 2. `{"payload": <any>}`: the value, verbatim.
/// 3. anything else: the whole response map.
pub(crate) fn decode_workflow_input_response(fr: &FunctionResponse) -> Value {
    if let Some(raw) = fr.response.get("response") {
        if let Value::String(s) = raw {
            return serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()));
        }
        return raw.clone();
    }
    if let Some(payload) = fr.response.get("payload") {
        return payload.clone();
    }
    Value::Object(fr.response.clone())
}
