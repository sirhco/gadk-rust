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
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, Context, RunConfig, RunnableTool, StreamingMode, Tool, ToolError,
};
use gadk_genai::{Content, FunctionDeclaration, ROLE_USER};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::{Map, Value};

use crate::artifacts::new_forwarding_service;
use crate::llm::processors::{go_json_marshal, go_sprint};
use crate::llm::state::reveal;
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::single_turn::make_function_declaration;
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

/// `v` with every number as a float: Go decodes tool arguments from JSON, so every number is a
/// `float64` and `fmt.Sprint` prints it in `%v` float form (`1e+06`, not `1000000`). A number
/// with no finite float form is kept as is.
fn as_go_floats(v: &Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Value::Number),
        Value::Array(a) => Value::Array(a.iter().map(as_go_floats).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), as_go_floats(v)))
                .collect(),
        ),
        other => other.clone(),
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

        if self.skip_summarization {
            ctx.update_actions(|a| a.skip_summarization = true);
        }

        // Cloned so no borrow of the agent state is held across an await.
        let (input_schema, output_schema) = match reveal(&*self.agent) {
            Some(s) => (s.cfg.input_schema.clone(), s.cfg.output_schema.clone()),
            None => (None, None),
        };

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
        // Dropping the stream (on return) cancels the child invocation.
        let mut events = runner.run(
            sub_session.user_id(),
            sub_session.id(),
            Some(content),
            run_cfg,
            RunOptions::default(),
        );
        let mut last_content: Option<Content> = None;
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
            if let Some(c) = event.response.content {
                last_content = Some(c);
            }
        }

        let Some(last) = last_content else {
            return Ok(Map::new());
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
            return Ok(Map::new());
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
