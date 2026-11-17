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

//! `AgentNode`: a graph node that runs an agent.

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, ContextDelta, EventStream, Mode};
use gadk_genai::{Content, Schema};
use gadk_session::{Event, EventActions, NodeInfo};
use serde_json::{Map, Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::llm::mode::{resolve_mode, with_bound_mode};
use crate::llm::node_wrapper::run_llm_agent_as_node;
use crate::llm::output_schema_binding::with_bound_output_schema;
use crate::llm::processors::go_json_marshal;
use crate::llm::state::reveal;
use crate::workflow::base_node::compile_schema;
use crate::workflow::model_output_schema::model_output_schema;
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::message_text;
use crate::workflow::{BaseNode, Node, NodeConfig};

/// A graph node that runs an agent.
pub struct AgentNode {
    pub(crate) base: BaseNode,
    pub(crate) agent: Arc<dyn Agent>,
    /// The output schema in the form the model is asked to answer with, or `None`. `run` binds it
    /// per invocation ([`crate::llm::output_schema_binding::with_bound_output_schema`]) rather than
    /// setting it on the agent.
    pub(crate) model_output_schema: Option<Schema>,
}

impl AgentNode {
    /// A node running `agent` with no declared schemas: both are the empty schema `{}`.
    pub fn new(agent: Arc<dyn Agent>, cfg: NodeConfig) -> Result<Arc<AgentNode>, WorkflowError> {
        Self::with_schemas(agent, None, None, cfg)
    }

    /// A node running `agent` with the given input and output schemas. A missing schema is the
    /// empty schema `{}`.
    pub fn with_schemas(
        agent: Arc<dyn Agent>,
        input: Option<Value>,
        output: Option<Value>,
        mut cfg: NodeConfig,
    ) -> Result<Arc<AgentNode>, WorkflowError> {
        let input = compile_schema(Some(input.unwrap_or_else(|| json!({})))).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving input schema for agent {}: {e}",
                    go_quote(agent.name())
                ),
            )
        })?;
        let output = output.unwrap_or_else(|| json!({}));
        let model_output_schema = model_output_schema(Some(&output));
        let output = compile_schema(Some(output)).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving output schema for agent {}: {e}",
                    go_quote(agent.name())
                ),
            )
        })?;
        // The wrapped agent's run already emits an invoke_agent span.
        cfg.emits_own_span = true;
        let base = BaseNode::from_compiled(agent.name(), agent.description(), cfg, input, output);
        Ok(Arc::new(AgentNode {
            base,
            agent,
            model_output_schema,
        }))
    }

    /// The wrapped agent.
    pub fn agent(&self) -> &Arc<dyn Agent> {
        &self.agent
    }
}

impl Node for AgentNode {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    /// Runs the agent on a fresh invocation context over the node's
    /// (input as user content, agent replaced, branch and isolation scope inherited); an
    /// undeclared LlmAgent mode binds to single_turn. Own final model text becomes the output
    /// (except for task-mode agents), and events are stamped with the ctx isolation scope.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let agent = Arc::clone(self.agent());
        let model_output_schema = self.model_output_schema.clone();
        Box::pin(async_stream::stream! {
            let user_content = match node_input_to_content(&input) {
                Ok(c) => c,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            let declared_mode: Option<Mode> = reveal(agent.as_ref()).map(|s| s.cfg.mode);
            // A fresh agent context over the node's invocation: the Context-level fields reset,
            // agent and user content replaced, the end flag copied from the node's context.
            let mut delta = ContextDelta {
                agent: Some(Arc::clone(&agent)),
                user_content: Some(user_content),
                path: Some(String::new()),
                run_id: Some(String::new()),
                sub_scheduler: Some(None),
                output_for_ancestors: Some(Vec::new()),
                resumed_inputs: Some(Arc::new(Map::new())),
                // function_call_id is cleared by fresh_end_flag; Some("") would mint a UUID.
                tool_confirmation: Some(None),
                actions: Some(Arc::new(parking_lot::Mutex::new(EventActions::default()))),
                fresh_end_flag: true,
                ..ContextDelta::default()
            };
            if let Some(declared) = declared_mode {
                // A graph node is a one-shot placement: an undeclared mode runs single_turn,
                // bound under this agent's own key.
                let mode = resolve_mode(declared, Mode::SingleTurn);
                let bound = with_bound_mode(ctx.as_invocation(), agent.name(), agent.agent_state(), mode);
                // Offer the node's output shape to the model. Each request decides whether to
                // use it, against the mode bound for it and the agent's own setup. A `None`
                // schema is still bound, so this placement clears one an outer placement bound.
                let bound = with_bound_output_schema(
                    &bound,
                    agent.name(),
                    agent.agent_state(),
                    model_output_schema.clone(),
                );
                delta.bound_modes = Some(bound.bound_modes().clone());
            }
            let ex_ctx = ctx.with_delta(delta);
            // An LLM agent runs through its node entry point.
            let mut events = if declared_mode.is_some() {
                run_llm_agent_as_node(ex_ctx, Arc::clone(&agent), input)
            } else {
                agent.run(ex_ctx.as_invocation().clone())
            };
            // Task-mode LlmAgents set their output through the task run, not model text.
            let synthesize = declared_mode != Some(Mode::Task);
            let scope = ctx.isolation_scope();
            while let Some(item) = events.next().await {
                let mut ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                // A composite agent yields one final response per sub-agent (each a distinct
                // author); synthesizing all of them would trip the one-output-per-node rule.
                if synthesize && is_own_agent_event(&ev, agent.name()) {
                    synthesize_agent_output(&mut ev);
                }
                // Tag the event for scope filtering (the direct agent-wrapper path).
                if !scope.is_empty() && ev.isolation_scope.is_empty() {
                    ev.isolation_scope = scope.clone();
                }
                yield Ok(ev);
            }
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A final model response without output gets the concatenated
/// model text as output (empty text is an empty `""` output) and `node_info.message_as_output`.
/// Long-running-tool events are a pause, not a completion, and are left untouched.
pub(crate) fn synthesize_agent_output(ev: &mut Event) {
    if ev.output.is_some() || !ev.is_final_response() || !ev.long_running_tool_ids.is_empty() {
        return;
    }
    if let Some(text) = message_text(ev) {
        ev.output = Some(Value::String(text));
        ev.node_info
            .get_or_insert_with(NodeInfo::default)
            .message_as_output = true;
    }
}

/// Whether the event is the node agent's own: un-authored events count as its own.
pub(crate) fn is_own_agent_event(ev: &Event, node_agent_name: &str) -> bool {
    ev.author.is_empty() || ev.author == node_agent_name
}

/// The user content for a node's input: null → none; a string → user text; anything else → its
/// compact JSON text (keys sorted, `<`, `>` and `&` written as Unicode escapes, as
/// `go_json_marshal` writes it) as user text. Marshaling a `Value` cannot fail, so
/// the error branch is unreachable; the `Result` is kept for the signature.
pub(crate) fn node_input_to_content(input: &Value) -> Result<Option<Content>, AgentError> {
    match input {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(Content::from_text(s.clone(), "user"))),
        other => Ok(Some(Content::from_text(go_json_marshal(other), "user"))),
    }
}
