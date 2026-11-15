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

//! `AgentTool`, `AgentToolConfig`: run an agent as a tool (port of Go tool/agenttool/agent_tool.go).

use std::any::Any;
use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, Context, RunConfig, RunnableTool, StreamingMode, Tool, ToolError,
    ToolOriginKind,
};
use gadk_genai::{Content, FunctionDeclaration, ROLE_USER};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::{Map, Value};

use crate::artifacts::new_forwarding_service;
use crate::llm::processors::{as_go_floats, go_json_marshal, go_sprint};
use crate::llm::state::reveal;
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::single_turn::{input_schema_of, make_function_declaration};
use crate::tool::utils::{go_type_name, validate_map_on_schema, validate_output_schema};
use crate::workflow::route::go_quote;

/// Port of Go `agenttool.Config`.
#[derive(Clone, Default)]
pub struct AgentToolConfig {
    /// If true, the calling agent skips summarization after the sub-agent finishes.
    pub skip_summarization: bool,
}

/// Port of Go `agenttool`'s `agentTool`: a tool that runs an agent in a child runner over a fresh
/// in-memory session and returns its final answer.
pub struct AgentTool {
    agent: Arc<dyn Agent>,
    skip_summarization: bool,
}

impl AgentTool {
    /// Port of Go `agenttool.New`; `None` means `skip_summarization = false`.
    #[allow(clippy::new_ret_no_self)] // mirrors Go agenttool.New, which returns tool.Tool (C4)
    pub fn new(agent: Arc<dyn Agent>, cfg: Option<AgentToolConfig>) -> Arc<dyn Tool> {
        Arc::new(AgentTool {
            agent,
            skip_summarization: cfg.is_some_and(|c| c.skip_summarization),
        })
    }
}

/// The error of [`AgentTool`]'s run: Go's `fmt.Errorf("<message>: %w", source)`, so a caller can
/// find the wrapped error (e.g. `AgentError::LlmCallsLimitExceeded`) in the source chain.
#[derive(Debug)]
struct AgentToolError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl std::fmt::Display for AgentToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Some(s) => write!(f, "{}: {}", self.message, s),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for AgentToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

fn exec(message: String) -> ToolError {
    ToolError::Execution(Box::new(AgentToolError {
        message,
        source: None,
    }))
}

fn exec_src(
    message: String,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ToolError {
    ToolError::Execution(Box::new(AgentToolError {
        message,
        source: Some(source.into()),
    }))
}

impl Tool for AgentTool {
    fn name(&self) -> &str {
        self.agent.name()
    }

    fn description(&self) -> &str {
        self.agent.description()
    }

    /// Go: `workflowinternal.MakeFunctionDeclaration(t.agent)`.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(make_function_declaration(&*self.agent))
    }

    // `process_request` is not overridden: Go's ProcessRequest is toolutils.PackTool, which the
    // flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    /// Go `SkipSummarizationResultDisplayer`: with skip summarization the sub-agent's result is the
    /// final answer, so it is still shown to the user.
    fn display_result_on_skip_summarization(&self) -> bool {
        true
    }

    /// Go classifies a `tool/agenttool` tool as `SUB_AGENT` (`A2A` when its agent is remote).
    fn origin_kind(&self) -> ToolOriginKind {
        ToolOriginKind::SubAgent
    }

    /// The wrapped agent (Go `subAgentOrA2A` reads the tool's `agent` field).
    fn origin_agent(&self) -> Option<Arc<dyn Agent>> {
        Some(self.agent.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go's per-part text of the sub-agent's last content: non-empty text, else a non-empty code
/// execution output without trailing newlines, else executable code (Go `switch` order).
fn part_text(p: &gadk_genai::Part) -> String {
    if !p.text.is_empty() {
        p.text.clone()
    } else if let Some(r) = p
        .code_execution_result
        .as_ref()
        .filter(|r| !r.output.is_empty())
    {
        r.output.trim_end_matches('\n').to_string()
    } else if let Some(c) = &p.executable_code {
        c.code.clone()
    } else {
        String::new()
    }
}

#[async_trait]
impl RunnableTool for AgentTool {
    /// Port of Go `(*agentTool).Run`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let name = self.agent.name().to_string();
        let margs = match args {
            Value::Object(m) => m,
            other => {
                return Err(ToolError::InvalidArgument(format!(
                    "agentTool expects map[string]any arguments, got {}",
                    go_type_name(&other)
                )));
            }
        };

        // The same lookup as the declaration (Go 31111f89 `workflowinternal.InputSchema`), so a
        // workflow agent takes the arguments of its first sub-agent's input schema. The output
        // schema is an LLM agent's own only. Cloned so no borrow of the agent state is held
        // across an await.
        let input_schema = input_schema_of(&*self.agent);
        let output_schema = reveal(&*self.agent).and_then(|s| s.cfg.output_schema.clone());

        let content = match &input_schema {
            Some(schema) => {
                validate_map_on_schema(&margs, schema, true).map_err(|e| {
                    exec(format!("argument validation failed for agent {name}: {e}"))
                })?;
                // Go json.Marshal (sorted keys, HTML escaping); it cannot fail on a JSON map, so
                // Go's "error serializing tool arguments" branch is unreachable.
                let json = go_json_marshal(&Value::Object(margs));
                Content::from_text(json, ROLE_USER)
            }
            None => {
                let input = margs.get("request").ok_or_else(|| {
                    exec(format!(
                        "missing required argument 'request' for agent {name}"
                    ))
                })?;
                // Go: the string as is, else fmt.Sprint(input) of the JSON-decoded value.
                Content::from_text(go_sprint(&as_go_floats(input)), ROLE_USER)
            }
        };

        // Set only once the input is valid, so an argument error goes back to the parent model
        // instead of ending its turn (Go 4cc7ce5f).
        if self.skip_summarization {
            ctx.update_actions(|a| a.skip_summarization = true);
        }

        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let runner = Runner::new(RunnerConfig {
            app_name: name.clone(),
            agent: self.agent.clone(),
            session_service: session_service.clone(),
            artifact_service: Some(new_forwarding_service(ctx.artifacts())),
            memory_service: Some(Arc::new(gadk_memory::InMemoryMemoryService::new())),
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: ctx.platform().clone(),
            compaction: None,
        })
        .map_err(|e| exec_src(format!("failed to create runner for agent {name}"), e))?;

        // Filter out ADK-internal state.
        let state: Map<String, Value> = ctx
            .state()
            .all()
            .into_iter()
            .filter(|(k, _)| !k.starts_with("_adk"))
            .collect();
        let sub_session = session_service
            .create(CreateRequest {
                app_name: name.clone(),
                user_id: ctx.user_id().to_string(),
                session_id: None,
                state: Some(state),
            })
            .await
            .map_err(|e| exec_src(format!("failed to create session for sub-agent {name}"), e))?;

        // Forward the caller's already-resolved model-call budget (Go runconfig.FromContext);
        // 0 when the caller was not run by a runner, which gives the nested run the default.
        let max_llm_calls = ctx.llm_call_limiter().map_or(0, |l| l.max_llm_calls());
        let run_cfg = RunConfig {
            streaming_mode: StreamingMode::Sse,
            max_llm_calls,
            ..Default::default()
        };
        // Dropping the stream (on return) cancels the child invocation. Go 24d33b35 (#1777):
        // the sub-runner starts with a context marked to inherit this invocation's plugins
        // (`plugininternal.WithInheritedPlugins(toolCtx)`), which a plugin-less runner keeps.
        let mut events = runner.run_with_context(
            &ctx.as_invocation().with_inherited_plugins(),
            sub_session.user_id(),
            sub_session.id(),
            Some(content),
            run_cfg,
            RunOptions::default(),
        );
        let mut last_content: Option<Content> = None;
        // Long-running calls that are never answered before the run ends (e.g. a tool
        // confirmation) leave the sub-agent paused. Its session is discarded, so the pause can
        // never be resumed (Go f25d1424).
        let mut pending_ids: BTreeSet<String> = BTreeSet::new();
        loop {
            // Go passes toolCtx as the child's context: cancelling the caller cancels the child.
            let next = tokio::select! {
                biased;
                _ = ctx.cancellation_token().cancelled() => {
                    return Err(exec_src(
                        format!("error during execution of sub-agent {name}"),
                        AgentError::Cancelled,
                    ));
                }
                n = events.next() => n,
            };
            let Some(item) = next else { break };
            let event = item
                .map_err(|e| exec_src(format!("error during execution of sub-agent {name}"), e))?;
            let (code, message) = (&event.response.error_code, &event.response.error_message);
            if !code.is_empty() || !message.is_empty() {
                return Err(exec(format!(
                    "error from sub-agent {} (code: {}, message: {})",
                    go_quote(&name),
                    go_quote(code),
                    go_quote(message)
                )));
            }
            // Partial chunks carry IDs the flow never acts on; a streamed call gets its final
            // ID only on the aggregated event.
            if !event.response.partial {
                pending_ids.extend(event.long_running_tool_ids.iter().cloned());
                if let Some(c) = &event.response.content {
                    for fr in c.parts.iter().filter_map(|p| p.function_response.as_ref()) {
                        pending_ids.remove(&fr.id);
                    }
                }
            }
            if let Some(c) = event.response.content {
                last_content = Some(c);
            }
        }

        if !pending_ids.is_empty() {
            // The parent model must see this error, so don't end its turn here, whatever set
            // skip_summarization on this call.
            ctx.update_actions(|a| a.skip_summarization = false);
            return Err(exec(format!(
                "sub-agent {} stopped waiting on unresolved long-running tool calls [{}], which AgentTool cannot resume",
                go_quote(&name),
                pending_ids.into_iter().collect::<Vec<_>>().join(" ")
            )));
        }

        // Go a8a9b936: an empty reply is the result "" (also with an output schema: there is
        // nothing to validate).
        let empty_result =
            || Map::from_iter([("result".to_string(), Value::String(String::new()))]);
        let Some(last) = last_content else {
            return Ok(empty_result());
        };
        let output_text = last
            .parts
            .iter()
            .filter(|p| !p.thought)
            .map(part_text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if output_text.is_empty() {
            return Ok(empty_result());
        }
        if let Some(schema) = &output_schema {
            return validate_output_schema(&output_text, schema).map_err(|e| {
                exec(format!(
                    "output validation failed for sub-agent {name}: {e}"
                ))
            });
        }
        let mut out = Map::new();
        out.insert("result".to_string(), Value::String(output_text));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::{LlmAgentConfig, agent as llm_agent};
    use crate::testutil::MockModel;

    /// The child's user message for a non-string `request` is Go `fmt.Sprint` of the JSON-decoded
    /// (float64) value; expected texts checked against Go.
    #[tokio::test]
    async fn non_string_request_is_go_sprint_of_float64() {
        let cases = [
            (json!(1000000), "1e+06"),
            (json!({"n": 5}), "map[n:5]"),
            (json!(2.5), "2.5"),
            (json!([1, 1e21, true, null]), "[1 1e+21 true <nil>]"),
        ];
        for (request, want) in cases {
            let model = MockModel::from_contents(vec![Content::from_text("ok", "model")]);
            let agent = llm_agent::new(LlmAgentConfig {
                name: "child".into(),
                model: Some(model.clone()),
                ..Default::default()
            })
            .unwrap();
            let sess = InMemorySessionService::new()
                .create(CreateRequest {
                    app_name: "app".into(),
                    user_id: "user".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            let ctx = Context::builder().session(sess).build();
            AgentTool::new(agent, None)
                .as_runnable()
                .unwrap()
                .run(ctx, json!({ "request": request }))
                .await
                .unwrap();
            let reqs = model.requests();
            let user = reqs[0]
                .contents
                .iter()
                .find(|c| c.role == ROLE_USER)
                .expect("child user message");
            assert_eq!(user.parts[0].text, want, "request {request}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;
    use crate::llm::{LlmAgentConfig, agent as llm_agent};

    #[test]
    fn agent_tool_is_a_sub_agent_tool_of_its_agent() {
        let agent = llm_agent::new(LlmAgentConfig {
            name: "child".into(),
            ..Default::default()
        })
        .unwrap();
        let t = AgentTool::new(agent.clone(), None);
        assert_eq!(t.origin_kind(), ToolOriginKind::SubAgent);
        assert!(Arc::ptr_eq(&t.origin_agent().unwrap(), &agent));
        let agents = t.origin_agents();
        assert_eq!(agents.len(), 1);
        assert!(Arc::ptr_eq(&agents[0], &agent));
        assert!(t.unwrap().is_none());
    }
}
