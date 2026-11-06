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

//! Port of Go runner/agent_node.go: any agent as a workflow node, forwarding the agent's events.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, ContextDelta, InvocationContext};
use gadk_genai::{Content, ROLE_USER};
use gadk_session::Session;
use serde_json::Value;

use super::run_node::is_llm_agent;
use crate::llm::node_wrapper::run_llm_agent_as_node;
use crate::llm::processors::go_json_marshal;
use crate::tool::utils::function_responses;
use crate::workflow::{Emit, Node, NodeConfig, dynamic_node};
use crate::{WorkflowError, WorkflowErrorKind};

/// Port of Go runner/agent_node.go `newAgentNode`: wraps any agent as a dynamic workflow node.
/// HITL rides on `long_running_tool_ids`: the node parks on them and re-runs on resume.
pub(crate) fn new_agent_node(a: Arc<dyn Agent>) -> Arc<dyn Node> {
    // EmitsOwnSpan: the agent's run already emits invoke_agent, so the scheduler must not add a
    // redundant invoke_node wrapper.
    let mut cfg = NodeConfig {
        emits_own_span: true,
        ..Default::default()
    };
    // RerunOnResume defaults to true only for LlmAgent (adk-python build_node); other kinds keep
    // the engine default.
    if is_llm_agent(&*a) {
        cfg.rerun_on_resume = Some(true);
    }
    let name = a.name().to_string();
    dynamic_node::<Value, Value, _, _>(
        &name,
        move |ctx: Context, input: Value, emit: Emit| {
            run_agent_node_body(a.clone(), ctx, input, emit)
        },
        cfg,
    )
}

/// Go `workflow.ErrNodeInterrupted`.
fn node_interrupted() -> AgentError {
    WorkflowError::new(
        WorkflowErrorKind::NodeInterrupted,
        WorkflowError::sentinel_text(WorkflowErrorKind::NodeInterrupted),
    )
    .into()
}

/// Port of Go runner/agent_node.go `runAgentNodeBody`: one activation of the wrapped agent.
async fn run_agent_node_body(
    a: Arc<dyn Agent>,
    ctx: Context,
    input: Value,
    emit: Emit,
) -> Result<Value, AgentError> {
    // On resume the input is the ORIGINAL user text; re-feeding it would loop. Resume is decided
    // from this turn's function responses, not from history, so a fresh text turn after a
    // completed resume still reaches the agent.
    let session = ctx.session();
    let resolved = answered_interrupts(Some(&*session), ctx.user_content()).unwrap_or_default();
    let is_resume = !resolved.is_empty();
    if is_llm_agent(&*a) {
        run_llm_agent_body(a, ctx, input, is_resume, emit).await
    } else {
        run_generic_agent_body(a, ctx, input, resolved, emit).await
    }
}

/// Port of Go runner/agent_node.go `runLlmAgentBody`: drives an LlmAgent through
/// [`run_llm_agent_as_node`] (mode-aware); a long-running call parks the node.
async fn run_llm_agent_body(
    a: Arc<dyn Agent>,
    ctx: Context,
    input: Value,
    is_resume: bool,
    emit: Emit,
) -> Result<Value, AgentError> {
    let node_input = if is_resume { Value::Null } else { input };
    let mut events = run_llm_agent_as_node(ctx, a, node_input);
    let mut paused = false;
    let mut last_output = Value::Null;
    while let Some(item) = events.next().await {
        // An ErrNodeInterrupted bubbled up from a task delegation parks the node.
        let event = item?;
        if !event.long_running_tool_ids.is_empty() {
            paused = true;
        }
        if let Some(out) = &event.output {
            last_output = out.clone();
        }
        emit(event).await?;
    }
    if paused {
        return Err(node_interrupted());
    }
    Ok(last_output)
}

/// Port of Go runner/agent_node.go `runGenericAgentBody`: forwards events verbatim, parks on
/// long-running calls, no output.
async fn run_generic_agent_body(
    a: Arc<dyn Agent>,
    ctx: Context,
    input: Value,
    resolved: BTreeSet<String>,
    emit: Emit,
) -> Result<Value, AgentError> {
    let user_content = if resolved.is_empty() {
        input_to_user_content(&input) // fresh turn
    } else {
        resume_user_content(ctx.user_content(), &resolved)
    };
    let agent_ctx = new_agent_context(&ctx, a.clone(), user_content);
    let mut events = a.run(agent_ctx);
    let mut paused = false;
    while let Some(item) = events.next().await {
        let event = item?;
        if !event.long_running_tool_ids.is_empty() {
            paused = true;
        }
        emit(event).await?;
    }
    if paused {
        return Err(node_interrupted());
    }
    Ok(Value::Null)
}

/// Port of Go runner/agent_node.go `answeredInterrupts`: the long-running interrupt IDs this
/// turn's function responses answer. `None` is Go's nil map, `Some(empty)` Go's empty map.
pub(crate) fn answered_interrupts(
    sess: Option<&dyn Session>,
    user_content: Option<&Content>,
) -> Option<BTreeSet<String>> {
    let sess = sess?;
    let responses = function_responses(user_content);
    if responses.is_empty() {
        return None;
    }
    let long_running: HashSet<String> = sess
        .events()
        .into_iter()
        .flat_map(|e| e.long_running_tool_ids)
        .collect();
    Some(
        responses
            .into_iter()
            .filter(|fr| !fr.id.is_empty() && long_running.contains(&fr.id))
            .map(|fr| fr.id.clone())
            .collect(),
    )
}

/// Port of Go runner/agent_node.go `resumeUserContent`: only the parts whose function response
/// ID is in `resolved`; `None` when nothing survives.
pub(crate) fn resume_user_content(
    user_content: Option<&Content>,
    resolved: &BTreeSet<String>,
) -> Option<Content> {
    let uc = user_content?;
    let parts: Vec<_> = uc
        .parts
        .iter()
        .filter(|p| {
            p.function_response
                .as_ref()
                .is_some_and(|fr| resolved.contains(&fr.id))
        })
        .cloned()
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(Content {
        role: uc.role.clone(),
        parts,
    })
}

/// Port of Go runner/agent_node.go `newAgentContext`: overrides Agent and UserContent and resets
/// Path, RunID and OutputForAncestors for the fresh activation; the node's sub-scheduler is
/// carried over unchanged.
fn new_agent_context(
    ctx: &Context,
    a: Arc<dyn Agent>,
    user_content: Option<Content>,
) -> InvocationContext {
    ctx.with_delta(ContextDelta {
        agent: Some(a),
        user_content: Some(user_content),
        run_id: Some(String::new()),
        path: Some(String::new()),
        output_for_ancestors: Some(Vec::new()),
        ..Default::default()
    })
    .as_invocation()
    .clone()
}

/// Port of Go runner/agent_node.go `inputToUserContent`. Go's `*genai.Content` case is a
/// Content-shaped object (keys only "parts" (an array) and optionally "role") that deserializes
/// as Content, passed through unchanged; anything else that is not a string is its Go
/// `json.Marshal` text.
fn input_to_user_content(input: &Value) -> Option<Content> {
    match input {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(Content::from_text(s.clone(), ROLE_USER)),
        Value::Object(m)
            if m.get("parts").is_some_and(Value::is_array)
                && m.keys().all(|k| k == "parts" || k == "role") =>
        {
            match serde_json::from_value::<Content>(input.clone()) {
                Ok(c) => Some(c),
                Err(_) => Some(Content::from_text(go_json_marshal(input), ROLE_USER)),
            }
        }
        other => Some(Content::from_text(go_json_marshal(other), ROLE_USER)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn input_to_user_content_matches_go() {
        assert_eq!(input_to_user_content(&Value::Null), None);
        assert_eq!(input_to_user_content(&json!("")), None);
        assert_eq!(
            input_to_user_content(&json!("hi")),
            Some(Content::from_text("hi", ROLE_USER))
        );
        let c = Content::from_text("x", "custom");
        assert_eq!(
            input_to_user_content(&serde_json::to_value(&c).unwrap()),
            Some(c)
        );
        // Go json.Marshal: sorted keys, Go float format, HTML escaping.
        assert_eq!(
            input_to_user_content(&json!({"b": 1.0, "a": "<"})),
            Some(Content::from_text("{\"a\":\"\\u003c\",\"b\":1}", ROLE_USER))
        );
    }
}
