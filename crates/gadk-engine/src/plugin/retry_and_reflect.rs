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

//! Port of Go plugin/retryandreflect: a plugin that turns tool failures into reflection guidance
//! for the model. It counts consecutive failures per tool within a scope (one invocation, or
//! global), and after `max_retries` failures tells the model to stop using the tool, or returns
//! the original error.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::{BoxFuture, ready};
use gadk_agent::{
    AfterToolCallback, AgentError, Context, OnToolErrorCallback, Plugin, PluginConfig,
    SharedToolError, Tool, ToolError,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::llm::processors::{as_go_floats, go_json_marshal};

/// Go `//go:embed reflection.md` (byte-for-byte copy).
const REFLECTION_TEMPLATE: &str = include_str!("retry_and_reflect/reflection.md");
/// Go `//go:embed exceeded.md` (byte-for-byte copy).
const EXCEEDED_TEMPLATE: &str = include_str!("retry_and_reflect/exceeded.md");

/// Go `reflectAndRetryResponseType`: the `response_type` of every result this plugin produces.
pub(crate) const REFLECT_AND_RETRY_RESPONSE_TYPE: &str =
    "ERROR_HANDLED_BY_REFLECT_AND_RETRY_PLUGIN";
/// Go `globalScopeKey`: the counter scope used with [`TrackingScope::Global`].
pub(crate) const GLOBAL_SCOPE_KEY: &str = "__global_reflect_and_retry_scope__";

const NAME: &str = "RetryAndReflectPlugin";

type MapResult = BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>;

/// Go `retryandreflect.TrackingScope`: the lifetime of the tool-failure counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackingScope {
    /// Go `Invocation`: failures are counted per invocation.
    #[default]
    Invocation,
    /// Go `Global`: failures are counted across all invocations, turns and users.
    Global,
}

/// Go `retryandreflect.New`'s options (`WithMaxRetries`, `WithErrorIfRetryExceeded`,
/// `WithTrackingScope`) as one config.
#[derive(Debug, Clone)]
pub struct RetryAndReflectConfig {
    /// Consecutive failures of one tool that still get reflection guidance; must be `>= 0`.
    pub max_retries: i32,
    /// Past the limit, return the original tool error instead of the "retry exceeded" result.
    pub error_if_retry_exceeded: bool,
    /// The scope the failures are counted in.
    pub tracking_scope: TrackingScope,
}

impl Default for RetryAndReflectConfig {
    /// Go `New`'s defaults: 3 retries, no error past the limit, invocation scope.
    fn default() -> Self {
        Self {
            max_retries: 3,
            error_if_retry_exceeded: false,
            tracking_scope: TrackingScope::Invocation,
        }
    }
}

/// Port of Go `retryandreflect.New`: a plugin named `RetryAndReflectPlugin` that registers only
/// the after-tool and on-tool-error callbacks. A negative `max_retries` is rejected with Go's
/// text, `maxRetries must be a non-negative integer`.
pub fn new_retry_and_reflect_plugin(cfg: RetryAndReflectConfig) -> Result<Plugin, AgentError> {
    if cfg.max_retries < 0 {
        return Err(AgentError::Config(
            "maxRetries must be a non-negative integer".into(),
        ));
    }
    let r = Arc::new(RetryAndReflect::new(&cfg));
    let after_tool: AfterToolCallback = {
        let r = r.clone();
        Arc::new(
            move |ctx: Context,
                  tool: Arc<dyn Tool>,
                  args: Map<String, Value>,
                  result: Map<String, Value>,
                  err: Option<Arc<ToolError>>|
                  -> MapResult {
                Box::pin(ready(r.after_tool(
                    &ctx,
                    &*tool,
                    &args,
                    &result,
                    err.as_deref(),
                )))
            },
        )
    };
    let on_tool_error: OnToolErrorCallback = {
        let r = r.clone();
        Arc::new(
            move |ctx: Context,
                  tool: Arc<dyn Tool>,
                  args: Map<String, Value>,
                  err: Arc<ToolError>|
                  -> MapResult {
                Box::pin(ready(r.on_tool_error(&ctx, &*tool, &args, &err)))
            },
        )
    };
    Ok(Plugin::new(PluginConfig {
        name: NAME.into(),
        after_tool_callback: Some(after_tool),
        on_tool_error_callback: Some(on_tool_error),
        ..Default::default()
    }))
}

/// Go `retryAndReflect`: the plugin's settings and its failure counters, shared by both
/// callbacks.
pub(crate) struct RetryAndReflect {
    pub(crate) max_retries: i32,
    pub(crate) error_if_retry_exceeded: bool,
    pub(crate) scope: TrackingScope,
    /// Go `scopedFailureCounters`: scope key → tool name → consecutive failures.
    pub(crate) scoped_failure_counters: Mutex<HashMap<String, HashMap<String, i32>>>,
}

impl RetryAndReflect {
    pub(crate) fn new(cfg: &RetryAndReflectConfig) -> Self {
        Self {
            max_retries: cfg.max_retries,
            error_if_retry_exceeded: cfg.error_if_retry_exceeded,
            scope: cfg.tracking_scope,
            scoped_failure_counters: Mutex::new(HashMap::new()),
        }
    }

    /// Port of plugin.go:128 `afterTool`. A success whose result is not this plugin's own
    /// reflection result resets the tool's count in its scope. The result is never changed.
    pub(crate) fn after_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        _args: &Map<String, Value>,
        result: &Map<String, Value>,
        err: Option<&ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        if err.is_none() {
            let is_reflect_response = result.get("response_type").and_then(Value::as_str)
                == Some(REFLECT_AND_RETRY_RESPONSE_TYPE);
            if !is_reflect_response {
                self.reset_failures_for_tool(ctx, tool.name());
            }
        }
        Ok(None)
    }

    /// Port of plugin.go:143 `onToolError` / `handleToolError`.
    pub(crate) fn on_tool_error(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        err: &Arc<ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // Go: errors.Is(err, tool.ErrConfirmationRequired) || errors.Is(err, tool.ErrConfirmationRejected).
        if matches!(
            **err,
            ToolError::ConfirmationRequired | ToolError::ConfirmationRejected
        ) {
            return Ok(None);
        }

        if self.max_retries == 0 {
            if self.error_if_retry_exceeded {
                return Err(original_error(err));
            }
            return Ok(Some(self.retry_exceeded_msg(tool, args, err)));
        }

        let scope_key = self.scope_key(ctx);
        let current_retries = {
            let mut counters = self.scoped_failure_counters.lock();
            let n = counters
                .entry(scope_key)
                .or_default()
                .entry(tool.name().to_string())
                .or_insert(0);
            *n = n.saturating_add(1);
            *n
        };

        if current_retries <= self.max_retries {
            return Ok(Some(self.reflection_response(
                tool,
                args,
                err,
                current_retries,
            )));
        }

        // Max retry exceeded.
        if self.error_if_retry_exceeded {
            return Err(original_error(err));
        }
        Ok(Some(self.retry_exceeded_msg(tool, args, err)))
    }

    /// Go `scopeKey`.
    fn scope_key(&self, ctx: &Context) -> String {
        match self.scope {
            TrackingScope::Global => GLOBAL_SCOPE_KEY.to_string(),
            TrackingScope::Invocation => ctx.invocation_id().to_string(),
        }
    }

    /// Go `resetFailuresForTool`: drops the tool's count, and the scope once it is empty.
    fn reset_failures_for_tool(&self, ctx: &Context, tool_name: &str) {
        let scope_key = self.scope_key(ctx);
        let mut counters = self.scoped_failure_counters.lock();
        if let Some(scope) = counters.get_mut(&scope_key) {
            scope.remove(tool_name);
            if scope.is_empty() {
                counters.remove(&scope_key);
            }
        }
    }

    /// Go `createToolReflectionResponse`.
    fn reflection_response(
        &self,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        err: &ToolError,
        retry_count: i32,
    ) -> Map<String, Value> {
        let rendered = render(
            REFLECTION_TEMPLATE,
            &TemplateData {
                tool_name: tool.name(),
                error_details: &format_error_details(err),
                args_summary: &format_tool_args(args),
                retry_count,
                max_retries: self.max_retries,
            },
        );
        response_map(err, retry_count, &rendered)
    }

    /// Go `createToolRetryExceedMsg`. Go-parity note 1: Go never sets `RetryCount` or `MaxRetries`
    /// in the template data, so the message always says "failed consecutively 0 times". The
    /// map's `retry_count` is still `max_retries`.
    fn retry_exceeded_msg(
        &self,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        err: &ToolError,
    ) -> Map<String, Value> {
        let rendered = render(
            EXCEEDED_TEMPLATE,
            &TemplateData {
                tool_name: tool.name(),
                error_details: &format_error_details(err),
                args_summary: &format_tool_args(args),
                retry_count: 0,
                max_retries: 0,
            },
        );
        response_map(err, self.max_retries, &rendered)
    }
}

/// The original tool error, keeping the caller's `Arc` (Go returns the same `err` value).
fn original_error(err: &Arc<ToolError>) -> AgentError {
    AgentError::Other(Box::new(SharedToolError(Arc::clone(err))))
}

/// The result map shared by the reflection and exceeded outcomes. `reflection_guidance` is the
/// rendered template trimmed as Go's `strings.TrimSpace` trims it.
fn response_map(err: &ToolError, retry_count: i32, rendered: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "response_type".into(),
        json!(REFLECT_AND_RETRY_RESPONSE_TYPE),
    );
    m.insert("error_type".into(), json!(error_type(err)));
    m.insert("error_details".into(), json!(err.to_string()));
    m.insert("retry_count".into(), json!(retry_count));
    m.insert("reflection_guidance".into(), json!(rendered.trim()));
    m
}

/// Stands in for Go's `fmt.Sprintf("%T", err)`: the Rust variant path (the plugin's one accepted
/// divergence; no Go test asserts it).
fn error_type(err: &ToolError) -> &'static str {
    match err {
        ToolError::ConfirmationRequired => "ToolError::ConfirmationRequired",
        ToolError::ConfirmationRejected => "ToolError::ConfirmationRejected",
        ToolError::InvalidArgument(_) => "ToolError::InvalidArgument",
        ToolError::Panic(_) => "ToolError::Panic",
        ToolError::Execution(_) => "ToolError::Execution",
        _ => "ToolError",
    }
}

/// Go `formatErrorDetails`: `"%T: %v"`, with [`error_type`] for `%T`.
fn format_error_details(err: &ToolError) -> String {
    format!("{}: {err}", error_type(err))
}

/// Go `formatToolArgs`: `json.MarshalIndent(args, "", "  ")`. Go's nil args map gives `null`; a
/// Rust map cannot be nil, so an empty one gives `null` too. Go decodes tool args into
/// `float64`, hence [`as_go_floats`].
fn format_tool_args(args: &Map<String, Value>) -> String {
    if args.is_empty() {
        return "null".to_string();
    }
    go_indent(&go_json_marshal(&as_go_floats(&Value::Object(
        args.clone(),
    ))))
}

/// Port of Go encoding/json `Indent(dst, src, "", "  ")` over compact, valid JSON. `{`/`[`
/// defer their newline until the next element, so empty objects and arrays stay `{}`/`[]`;
/// `:` gains one space; strings are copied untouched.
fn go_indent(compact: &str) -> String {
    fn newline(out: &mut String, depth: usize) {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    }
    let mut out = String::with_capacity(compact.len() * 2);
    let mut depth = 0usize;
    let mut need_indent = false;
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if need_indent && c != '}' && c != ']' {
            need_indent = false;
            depth += 1;
            newline(&mut out, depth);
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                need_indent = true;
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            '}' | ']' => {
                if need_indent {
                    // Empty object or array: Go suppresses the indent.
                    need_indent = false;
                } else {
                    depth = depth.saturating_sub(1);
                    newline(&mut out, depth);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Go `templateData`.
struct TemplateData<'a> {
    tool_name: &'a str,
    error_details: &'a str,
    args_summary: &'a str,
    retry_count: i32,
    max_retries: i32,
}

/// Go `text/template` execution of reflection.md / exceeded.md. Both templates use only the
/// five `{{.Field}}` actions, so one left-to-right substitution pass gives Go's exact output.
/// Substituted values are never re-scanned. An unknown action, which neither template has, is
/// copied through unchanged.
fn render(template: &str, d: &TemplateData<'_>) -> String {
    let mut out = String::with_capacity(template.len() + d.args_summary.len() + 64);
    let mut rest = template;
    while let Some(start) = rest.find("{{.") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        match &after[..end] {
            "ToolName" => out.push_str(d.tool_name),
            "ErrorDetails" => out.push_str(d.error_details),
            "ArgsSummary" => out.push_str(d.args_summary),
            "RetryCount" => out.push_str(&d.retry_count.to_string()),
            "MaxRetries" => out.push_str(&d.max_retries.to_string()),
            other => {
                out.push_str("{{.");
                out.push_str(other);
                out.push_str("}}");
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;

    use super::*;

    struct NamedTool(&'static str);

    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            ""
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    const WANT_REFLECTION: &str = r#"The call to tool `test-tool` failed.

**Error Details:**
```
ToolError::Execution: fail
```

**Tool Arguments Used:**
```json
{
  "arg1": "val1"
}
```

**Reflection Guidance:**
This is retry attempt **1 of 2**. Analyze the error and the arguments you provided. Do not repeat the exact same call. Consider the following before your next attempt:

1.  **Invalid Parameters**: Does the error suggest that one or more arguments are incorrect, badly formatted, or missing? Review the tool's schema and your arguments.
2.  **State or Preconditions**: Did a previous step fail or not produce the necessary state/resource for this tool to succeed?
3.  **Alternative Approach**: Is this the right tool for the job? Could another tool or a different sequence of steps achieve the goal?
4.  **Simplify the Task**: Can you break the problem down into smaller, simpler steps?
5.  **Wrong Function Name**: Does the error indicates the tool is not found? Please check again and only use available tools.

Formulate a new plan based on your analysis and try a corrected or different approach."#;

    const WANT_EXCEEDED: &str = r#"The tool `test-tool` has failed consecutively 0 times and the retry limit has been exceeded.

**Last Error:**
```
ToolError::Execution: fail
```

**Last Arguments Used:**
```json
null
```

**Final Instruction:**
**Do not attempt to use the `test-tool` tool again for this task.** You must now try a different approach. Acknowledge the failure and devise a new strategy, potentially using other available tools or informing the user that the task cannot be completed."#;

    fn fail() -> Arc<ToolError> {
        Arc::new(ToolError::Execution("fail".into()))
    }

    #[test]
    fn templates_are_go_bytes() {
        assert!(REFLECTION_TEMPLATE.starts_with("\nThe call to tool `{{.ToolName}}` failed.\n"));
        assert!(REFLECTION_TEMPLATE.ends_with("try a corrected or different approach.\n"));
        assert!(EXCEEDED_TEMPLATE.starts_with(
            "\nThe tool `{{.ToolName}}` has failed consecutively {{.MaxRetries}} times"
        ));
        assert!(EXCEEDED_TEMPLATE.ends_with("the task cannot be completed.\n"));
    }

    #[test]
    fn reflection_result_renders_go_template() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig {
            max_retries: 2,
            ..Default::default()
        });
        let args = json!({"arg1": "val1"}).as_object().cloned().unwrap();
        let res = r
            .on_tool_error(
                &Context::builder().build(),
                &NamedTool("test-tool"),
                &args,
                &fail(),
            )
            .unwrap()
            .unwrap();
        let want = json!({
            "response_type": "ERROR_HANDLED_BY_REFLECT_AND_RETRY_PLUGIN",
            "error_type": "ToolError::Execution",
            "error_details": "fail",
            "retry_count": 1,
            "reflection_guidance": WANT_REFLECTION,
        });
        assert_eq!(Value::Object(res), want);
    }

    // Go-parity note 1: "failed consecutively 0 times", whatever max_retries is.
    #[test]
    fn exceeded_result_renders_go_template_with_zero_times() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig {
            max_retries: 0,
            ..Default::default()
        });
        let res = r
            .on_tool_error(
                &Context::builder().build(),
                &NamedTool("test-tool"),
                &Map::new(),
                &fail(),
            )
            .unwrap()
            .unwrap();
        let want = json!({
            "response_type": "ERROR_HANDLED_BY_REFLECT_AND_RETRY_PLUGIN",
            "error_type": "ToolError::Execution",
            "error_details": "fail",
            "retry_count": 0,
            "reflection_guidance": WANT_EXCEEDED,
        });
        assert_eq!(Value::Object(res), want);
        // max_retries == 0 counts nothing.
        assert!(r.scoped_failure_counters.lock().is_empty());
    }

    #[test]
    fn exceeded_after_the_limit_keeps_zero_times_and_reports_max_retries() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig {
            max_retries: 1,
            ..Default::default()
        });
        let ctx = Context::builder().build();
        let tool = NamedTool("test-tool");
        let _ = r.on_tool_error(&ctx, &tool, &Map::new(), &fail());
        let res = r
            .on_tool_error(&ctx, &tool, &Map::new(), &fail())
            .unwrap()
            .unwrap();
        assert_eq!(res["retry_count"], json!(1));
        assert_eq!(res["reflection_guidance"], json!(WANT_EXCEEDED));
    }

    #[test]
    fn max_retries_zero_with_error_returns_the_original_error() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig {
            max_retries: 0,
            error_if_retry_exceeded: true,
            ..Default::default()
        });
        let err = fail();
        let got = r
            .on_tool_error(
                &Context::builder().build(),
                &NamedTool("t"),
                &Map::new(),
                &err,
            )
            .unwrap_err();
        let AgentError::Other(inner) = &got else {
            panic!("want AgentError::Other, got {got:?}");
        };
        let shared = inner.downcast_ref::<SharedToolError>().unwrap();
        assert!(Arc::ptr_eq(&shared.0, &err));
        assert!(r.scoped_failure_counters.lock().is_empty());
    }

    #[test]
    fn confirmation_errors_pass_through_uncounted() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig::default());
        let ctx = Context::builder().build();
        for e in [
            ToolError::ConfirmationRequired,
            ToolError::ConfirmationRejected,
        ] {
            let res = r.on_tool_error(&ctx, &NamedTool("t"), &Map::new(), &Arc::new(e));
            assert!(matches!(res, Ok(None)), "{res:?}");
        }
        assert!(r.scoped_failure_counters.lock().is_empty());
    }

    #[test]
    fn config_defaults_match_go() {
        let d = RetryAndReflectConfig::default();
        assert_eq!(
            (d.max_retries, d.error_if_retry_exceeded, d.tracking_scope),
            (3, false, TrackingScope::Invocation)
        );
    }

    // Expected value printed by Go: json.MarshalIndent of the same JSON decoded into map[string]any.
    #[test]
    fn args_summary_matches_go_marshal_indent() {
        let args = json!({
            "b": [1, {}],
            "a": "<x>&",
            "c": [],
            "d": {"z": 1.5, "y": null},
            "e": 1e21,
            "f": {"g": [true, "\u{2028}"]}
        });
        assert_eq!(
            format_tool_args(args.as_object().unwrap()),
            "{\n  \"a\": \"\\u003cx\\u003e\\u0026\",\n  \"b\": [\n    1,\n    {}\n  ],\n  \"c\": [],\n  \"d\": {\n    \"y\": null,\n    \"z\": 1.5\n  },\n  \"e\": 1e+21,\n  \"f\": {\n    \"g\": [\n      true,\n      \"\\u2028\"\n    ]\n  }\n}"
        );
        assert_eq!(format_tool_args(&Map::new()), "null");
    }

    #[test]
    fn render_does_not_rescan_substituted_values() {
        let d = TemplateData {
            tool_name: "{{.ArgsSummary}}",
            error_details: "",
            args_summary: "ARGS",
            retry_count: 0,
            max_retries: 0,
        };
        assert_eq!(
            render("{{.ToolName}}/{{.ArgsSummary}}", &d),
            "{{.ArgsSummary}}/ARGS"
        );
    }
}
