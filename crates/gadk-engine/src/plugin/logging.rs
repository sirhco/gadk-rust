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

//! Port of Go plugin/loggingplugin: a plugin that logs key invocation activity (user messages,
//! agent and model activity, tool calls, events and errors) for terminal debugging.

use std::sync::Arc;

use futures::future::{BoxFuture, ready};
use gadk_agent::{
    AfterAgentCallback, AfterModelCallback, AfterRunCallback, AfterToolCallback, AgentError,
    BeforeAgentCallback, BeforeModelCallback, BeforeRunCallback, BeforeToolCallback, Context,
    InvocationContext, OnEventCallback, OnModelErrorCallback, OnToolErrorCallback,
    OnUserMessageCallback, Plugin, Tool, ToolError,
};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value};

const NAME: &str = "logging_plugin";

type ContentResult = BoxFuture<'static, Result<Option<Content>, AgentError>>;
type ResponseResult = BoxFuture<'static, Result<Option<LlmResponse>, AgentError>>;
type MapResult = BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>;

/// Port of Go `loggingplugin.New("")`: logs invocation activity through `tracing` at INFO under
/// target `gadk_engine::plugin::logging`. Go's `MustNew` and custom name are not ported.
pub fn logging_plugin() -> Plugin {
    let p = Arc::new(LoggingPlugin {
        name: NAME.to_string(),
    });
    let on_user_message: OnUserMessageCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: InvocationContext, msg: Content| -> ContentResult {
                p.on_user_message(&ctx, &msg);
                Box::pin(ready(Ok(None)))
            },
        )
    };
    let before_run: BeforeRunCallback = {
        let p = p.clone();
        Arc::new(move |ctx: InvocationContext| -> ContentResult {
            p.before_run(&ctx);
            Box::pin(ready(Ok(None)))
        })
    };
    let on_event: OnEventCallback = {
        let p = p.clone();
        Arc::new(
            move |_ctx: InvocationContext,
                  event: Event|
                  -> BoxFuture<'static, Result<Option<Event>, AgentError>> {
                p.on_event(&event);
                Box::pin(ready(Ok(None)))
            },
        )
    };
    let after_run: AfterRunCallback = {
        let p = p.clone();
        Arc::new(move |ctx: InvocationContext| -> BoxFuture<'static, ()> {
            p.after_run(&ctx);
            Box::pin(ready(()))
        })
    };
    let before_agent: BeforeAgentCallback = {
        let p = p.clone();
        Arc::new(move |ctx: Context| -> ContentResult {
            p.before_agent(&ctx);
            Box::pin(ready(Ok(None)))
        })
    };
    let after_agent: AfterAgentCallback = {
        let p = p.clone();
        Arc::new(move |ctx: Context| -> ContentResult {
            p.after_agent(&ctx);
            Box::pin(ready(Ok(None)))
        })
    };
    let before_model: BeforeModelCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context,
                  req: LlmRequest|
                  -> BoxFuture<'static, (LlmRequest, Result<Option<LlmResponse>, AgentError>)> {
                p.before_model(&ctx, &req);
                Box::pin(ready((req, Ok(None))))
            },
        )
    };
    let after_model: AfterModelCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context,
                  resp: LlmResponse,
                  err: Option<Arc<ModelError>>|
                  -> BoxFuture<'static, (LlmResponse, Result<Option<LlmResponse>, AgentError>)> {
                p.after_model(&ctx, &resp, err.as_deref());
                Box::pin(ready((resp, Ok(None))))
            },
        )
    };
    let on_model_error: OnModelErrorCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context, _req: LlmRequest, err: Arc<ModelError>| -> ResponseResult {
                p.on_model_error(&ctx, &err);
                Box::pin(ready(Ok(None)))
            },
        )
    };
    let before_tool: BeforeToolCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context, tool: Arc<dyn Tool>, args: Map<String, Value>| -> MapResult {
                p.before_tool(&ctx, &*tool, &args);
                Box::pin(ready(Ok(None)))
            },
        )
    };
    let after_tool: AfterToolCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context,
                  tool: Arc<dyn Tool>,
                  _args: Map<String, Value>,
                  result: Map<String, Value>,
                  err: Option<Arc<ToolError>>|
                  -> MapResult {
                p.after_tool(&ctx, &*tool, &result, err.as_deref());
                Box::pin(ready(Ok(None)))
            },
        )
    };
    let on_tool_error: OnToolErrorCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context,
                  tool: Arc<dyn Tool>,
                  args: Map<String, Value>,
                  err: Arc<ToolError>|
                  -> MapResult {
                p.on_tool_error(&ctx, &*tool, &args, &err);
                Box::pin(ready(Ok(None)))
            },
        )
    };
    // Go plugin.New never fails; Plugin::new is infallible.
    Plugin::new(gadk_agent::PluginConfig {
        name: NAME.to_string(),
        on_user_message_callback: Some(on_user_message),
        before_run_callback: Some(before_run),
        on_event_callback: Some(on_event),
        after_run_callback: Some(after_run),
        before_agent_callback: Some(before_agent),
        after_agent_callback: Some(after_agent),
        // Go's logging plugin registers no OnAgentErrorCallback.
        on_agent_error_callback: None,
        before_model_callback: Some(before_model),
        after_model_callback: Some(after_model),
        on_model_error_callback: Some(on_model_error),
        before_tool_callback: Some(before_tool),
        after_tool_callback: Some(after_tool),
        on_tool_error_callback: Some(on_tool_error),
        close: None,
        state: None,
    })
}

struct LoggingPlugin {
    name: String,
}

/// Go's byte slicing `s[:max]`, cut back to a char boundary so it cannot panic.
fn truncate(s: &str, max: usize) -> Option<&str> {
    if s.len() <= max {
        return None;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(&s[..end])
}

/// Go `%v` of a `[]string`.
fn go_list(items: &[String]) -> String {
    format!("[{}]", items.join(" "))
}

/// Port of Go `formatContent`.
fn format_content(content: Option<&Content>, max: usize) -> String {
    let Some(content) = content.filter(|c| !c.parts.is_empty()) else {
        return "None".to_string();
    };
    let parts: Vec<String> = content
        .parts
        .iter()
        .map(|part| {
            if !part.text.is_empty() {
                let text = part.text.trim();
                match truncate(text, max) {
                    Some(cut) => format!("text: '{cut}...'"),
                    None => format!("text: '{text}'"),
                }
            } else if let Some(fc) = &part.function_call {
                format!("function_call: {}", fc.name)
            } else if let Some(fr) = &part.function_response {
                format!("function_response: {}", fr.name)
            } else if part.code_execution_result.is_some() {
                "code_execution_result".to_string()
            } else {
                "other_part".to_string()
            }
        })
        .collect();
    parts.join(" | ")
}

/// Port of Go `formatArgs`. Go prints `%v` of the map; the Rust rendering is JSON.
fn format_args(args: &Map<String, Value>, max: usize) -> String {
    if args.is_empty() {
        return "{}".to_string();
    }
    let formatted = Value::Object(args.clone()).to_string();
    match truncate(&formatted, max) {
        Some(cut) => format!("{cut}...}}"),
        None => formatted,
    }
}

impl LoggingPlugin {
    /// Port of Go `log` (Go's ANSI grey escape is dropped; `tracing` formats colour).
    fn log(&self, msg: impl AsRef<str>) {
        tracing::info!(target: "gadk_engine::plugin::logging", "[{}] {}", self.name, msg.as_ref());
    }

    fn on_user_message(&self, ctx: &InvocationContext, msg: &Content) {
        self.log("🚀 USER MESSAGE RECEIVED");
        self.log(format!("   Invocation ID: {}", ctx.invocation_id()));
        let session = ctx.session();
        self.log(format!("   Session ID: {}", session.id()));
        self.log(format!("   User ID: {}", session.user_id()));
        self.log(format!("   App Name: {}", session.app_name()));
        self.log(format!("   Root Agent: {}", ctx.agent().name()));
        self.log(format!(
            "   User Content: {}",
            format_content(Some(msg), 200)
        ));
        let branch = ctx.branch();
        if !branch.is_empty() {
            self.log(format!("   Branch: {branch}"));
        }
    }

    fn before_run(&self, ctx: &InvocationContext) {
        self.log("🏃 INVOCATION STARTING");
        self.log(format!("   Invocation ID: {}", ctx.invocation_id()));
        self.log(format!("   Starting Agent: {}", ctx.agent().name()));
    }

    fn on_event(&self, event: &Event) {
        self.log("📢 EVENT YIELDED");
        self.log(format!("   Event ID: {}", event.id));
        self.log(format!("   Author: {}", event.author));
        self.log(format!(
            "   Content: {}",
            format_content(event.response.content.as_ref(), 200)
        ));
        self.log(format!("   Final Response: {}", event.is_final_response()));
        let mut func_calls = Vec::new();
        let mut func_responses = Vec::new();
        if let Some(content) = &event.response.content {
            for part in &content.parts {
                if let Some(fc) = &part.function_call {
                    func_calls.push(fc.name.clone());
                }
                if let Some(fr) = &part.function_response {
                    func_responses.push(fr.name.clone());
                }
            }
        }
        if !func_calls.is_empty() {
            self.log(format!("   Function Calls: {}", go_list(&func_calls)));
        }
        if !func_responses.is_empty() {
            self.log(format!(
                "   Function Responses: {}",
                go_list(&func_responses)
            ));
        }
        if !event.long_running_tool_ids.is_empty() {
            self.log(format!(
                "   Long Running Tools: {}",
                go_list(&event.long_running_tool_ids)
            ));
        }
    }

    fn after_run(&self, ctx: &InvocationContext) {
        self.log("✅ INVOCATION COMPLETED");
        self.log(format!("   Invocation ID: {}", ctx.invocation_id()));
        self.log(format!("   Final Agent: {}", ctx.agent().name()));
    }

    fn before_agent(&self, ctx: &Context) {
        self.log("🤖 AGENT STARTING");
        self.log(format!("   Agent Name: {}", ctx.agent_name()));
        self.log(format!("   Invocation ID: {}", ctx.invocation_id()));
        let branch = ctx.branch();
        if !branch.is_empty() {
            self.log(format!("   Branch: {branch}"));
        }
    }

    fn after_agent(&self, ctx: &Context) {
        self.log("🤖 AGENT COMPLETED");
        self.log(format!("   Agent Name: {}", ctx.agent_name()));
        self.log(format!("   Invocation ID: {}", ctx.invocation_id()));
    }

    fn before_model(&self, ctx: &Context, req: &LlmRequest) {
        self.log("🧠 LLM REQUEST");
        let model = if req.model.is_empty() {
            "default"
        } else {
            req.model.as_str()
        };
        self.log(format!("   Model: {model}"));
        self.log(format!("   Agent: {}", ctx.agent_name()));
        if let Some(si) = req
            .config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref())
            .filter(|si| !si.parts.is_empty())
        {
            let instruction: String = si.parts.iter().map(|p| p.text.as_str()).collect();
            let shown = match truncate(&instruction, 200) {
                Some(cut) => format!("{cut}..."),
                None => instruction.clone(),
            };
            self.log(format!("   System Instruction: '{shown}'"));
        }
        if !req.tools.is_empty() {
            // Go map order is random; sorted here.
            let mut names: Vec<String> = req.tools.keys().cloned().collect();
            names.sort();
            self.log(format!("   Available Tools: {}", go_list(&names)));
        }
    }

    fn after_model(&self, ctx: &Context, resp: &LlmResponse, err: Option<&ModelError>) {
        self.log("🧠 LLM RESPONSE");
        self.log(format!("   Agent: {}", ctx.agent_name()));
        if let Some(e) = err {
            self.log(format!("   ❌ ERROR - {e}"));
            return;
        }
        if !resp.error_code.is_empty() {
            self.log(format!("   ❌ ERROR - Code: {}", resp.error_code));
            self.log(format!("   Error Message: {}", resp.error_message));
        } else {
            self.log(format!(
                "   Content: {}",
                format_content(resp.content.as_ref(), 200)
            ));
            if resp.partial {
                self.log(format!("   Partial: {}", resp.partial));
            }
            self.log(format!("   Turn Complete: {}", resp.turn_complete));
        }
        if let Some(usage) = &resp.usage_metadata {
            self.log(format!(
                "   Token Usage - Input: {}, Output: {}",
                usage.prompt_token_count, usage.candidates_token_count
            ));
        }
    }

    fn on_model_error(&self, ctx: &Context, err: &ModelError) {
        self.log("🧠 LLM ERROR");
        self.log(format!("   Agent: {}", ctx.agent_name()));
        self.log(format!("   Error: {err}"));
    }

    fn before_tool(&self, ctx: &Context, tool: &dyn Tool, args: &Map<String, Value>) {
        self.log("🔧 TOOL STARTING");
        self.log(format!("   Tool Name: {}", tool.name()));
        self.log(format!("   Agent: {}", ctx.agent_name()));
        self.log(format!("   Function Call ID: {}", ctx.function_call_id()));
        self.log(format!("   Arguments: {}", format_args(args, 300)));
    }

    fn after_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        result: &Map<String, Value>,
        err: Option<&ToolError>,
    ) {
        self.log("🔧 TOOL COMPLETED");
        self.log(format!("   Tool Name: {}", tool.name()));
        self.log(format!("   Agent: {}", ctx.agent_name()));
        self.log(format!("   Function Call ID: {}", ctx.function_call_id()));
        match err {
            Some(e) => self.log(format!("   Error: {e}")),
            None => self.log(format!("   Result: {}", format_args(result, 300))),
        }
    }

    fn on_tool_error(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        err: &ToolError,
    ) {
        self.log("🔧 TOOL ERROR");
        self.log(format!("   Tool Name: {}", tool.name()));
        self.log(format!("   Agent: {}", ctx.agent_name()));
        self.log(format!("   Function Call ID: {}", ctx.function_call_id()));
        self.log(format!("   Arguments: {}", format_args(args, 300)));
        self.log(format!("   Error: {err}"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use gadk_genai::{CodeExecutionResult, FunctionCall, FunctionResponse, Part};
    use serde_json::json;

    fn content(parts: Vec<Part>) -> Content {
        Content {
            role: "user".into(),
            parts,
        }
    }

    #[test]
    fn format_content_cases() {
        assert_eq!(format_content(None, 200), "None");
        assert_eq!(format_content(Some(&content(vec![])), 200), "None");
        assert_eq!(
            format_content(Some(&content(vec![Part::from_text("  hi  ")])), 200),
            "text: 'hi'"
        );
        let fc = Part {
            function_call: Some(FunctionCall {
                name: "f".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let fr = Part {
            function_response: Some(FunctionResponse {
                name: "g".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cer = Part {
            code_execution_result: Some(CodeExecutionResult::default()),
            ..Default::default()
        };
        assert_eq!(
            format_content(
                Some(&content(vec![
                    Part::from_text("t"),
                    fc,
                    fr,
                    cer,
                    Part::default()
                ])),
                200
            ),
            "text: 't' | function_call: f | function_response: g | code_execution_result | other_part"
        );
    }

    #[test]
    fn format_content_truncates_without_splitting_a_char() {
        let s = format!("{}é{}", "a".repeat(199), "b".repeat(10)); // 'é' straddles byte 200
        let got = format_content(Some(&content(vec![Part::from_text(s)])), 200);
        assert_eq!(got, format!("text: '{}...'", "a".repeat(199)));
        let exact = "a".repeat(200);
        assert_eq!(
            format_content(Some(&content(vec![Part::from_text(exact.clone())])), 200),
            format!("text: '{exact}'")
        );
    }

    #[test]
    fn format_args_cases() {
        assert_eq!(format_args(&Map::new(), 300), "{}");
        let Value::Object(small) = json!({"a": 1}) else {
            unreachable!()
        };
        assert_eq!(format_args(&small, 300), r#"{"a":1}"#);
        let Value::Object(big) = json!({"k": "x".repeat(400)}) else {
            unreachable!()
        };
        let got = format_args(&big, 300);
        assert!(got.ends_with("...}"));
        assert_eq!(got.len(), 300 + 4);
    }
}
