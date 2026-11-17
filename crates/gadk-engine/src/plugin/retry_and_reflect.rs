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

//! A plugin that turns tool failures into reflection guidance for the model. It counts
//! consecutive failures per tool within a scope (one invocation, or global), and after
//! `max_retries` failures tells the model to stop using the tool, or returns the original error.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::{BoxFuture, ready};
use gadk_agent::{
    AfterRunCallback, AfterToolCallback, AgentError, Context, InvocationContext,
    OnToolErrorCallback, Plugin, PluginConfig, SharedToolError, Tool, ToolError,
};
use gadk_session::Session;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::llm::processors::{as_go_floats, go_json_marshal};
use crate::tool::utils::model_error_text;

/// The reflection guidance template (`reflection.md`), embedded verbatim.
const REFLECTION_TEMPLATE: &str = include_str!("retry_and_reflect/reflection.md");
/// The retry-exceeded template (`exceeded.md`), embedded verbatim.
const EXCEEDED_TEMPLATE: &str = include_str!("retry_and_reflect/exceeded.md");

/// The `response_type` of every result this plugin produces.
pub(crate) const REFLECT_AND_RETRY_RESPONSE_TYPE: &str =
    "ERROR_HANDLED_BY_REFLECT_AND_RETRY_PLUGIN";
/// The counter scope used with [`TrackingScope::Global`].
pub(crate) const GLOBAL_SCOPE_KEY: &str = "__global_reflect_and_retry_scope__";

const NAME: &str = "RetryAndReflectPlugin";

type MapResult = BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>;

/// The lifetime of the tool-failure counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackingScope {
    /// Failures are counted per invocation.
    #[default]
    Invocation,
    /// Failures are counted across all invocations, turns and users.
    Global,
}

/// The options of the retry-and-reflect plugin.
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
    /// The defaults: 3 retries, no error past the limit, invocation scope.
    fn default() -> Self {
        Self {
            max_retries: 3,
            error_if_retry_exceeded: false,
            tracking_scope: TrackingScope::Invocation,
        }
    }
}

/// Builds a plugin named `RetryAndReflectPlugin` that registers only the after-tool,
/// on-tool-error and after-run callbacks. A negative `max_retries` is rejected with
/// `maxRetries must be a non-negative integer`.
pub fn new_retry_and_reflect_plugin(cfg: RetryAndReflectConfig) -> Result<Plugin, AgentError> {
    if cfg.max_retries < 0 {
        return Err(AgentError::Config(
            "maxRetries must be a non-negative integer".into(),
        ));
    }
    Ok(plugin_for(Arc::new(RetryAndReflect::new(&cfg))))
}

/// The plugin whose three callbacks share `r`.
fn plugin_for(r: Arc<RetryAndReflect>) -> Plugin {
    let after_run: AfterRunCallback = {
        let r = r.clone();
        Arc::new(move |ictx: InvocationContext| -> BoxFuture<'static, ()> {
            r.after_run(&ictx);
            Box::pin(ready(()))
        })
    };
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
    Plugin::new(PluginConfig {
        name: NAME.into(),
        after_tool_callback: Some(after_tool),
        on_tool_error_callback: Some(on_tool_error),
        after_run_callback: Some(after_run),
        ..Default::default()
    })
}

/// The plugin's settings and its failure counters, shared by its callbacks.
pub(crate) struct RetryAndReflect {
    pub(crate) max_retries: i32,
    pub(crate) error_if_retry_exceeded: bool,
    pub(crate) scope: TrackingScope,
    /// Consecutive failures: scope key → tool name → count.
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

    /// The after-tool callback. A success whose result is not this plugin's own reflection result
    /// resets the tool's count in its scope. The result is never changed.
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

    /// The on-tool-error callback: counts the failure and returns reflection guidance while the
    /// count is within `max_retries`; past it, the original error or the "retry exceeded" result.
    /// Confirmation requests and rejections are not failures and pass through.
    pub(crate) fn on_tool_error(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
        err: &Arc<ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // A confirmation request or rejection is not a failure.
        if is_confirmation_error(err) {
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

    /// The after-run callback: drops the failure counts of an invocation once its run has ended,
    /// since nothing reads them afterwards. Every scope but `Global` keys its
    /// counts by invocation ID. The counts of a run that paused on an unanswered long-running
    /// call are kept: the run that answers the call reuses the invocation ID and continues the
    /// same retry budget, as in adk-python.
    pub(crate) fn after_run(&self, ictx: &InvocationContext) {
        if self.scope == TrackingScope::Global {
            return;
        }
        let invocation_id = ictx.invocation_id();
        let tracked = self
            .scoped_failure_counters
            .lock()
            .contains_key(invocation_id);
        // Most runs record no failure; skip the scan of the session events for them.
        if !tracked || has_open_long_running_call(&*ictx.session(), invocation_id) {
            return;
        }
        self.scoped_failure_counters.lock().remove(invocation_id);
    }

    /// The counter scope key for the context.
    fn scope_key(&self, ctx: &Context) -> String {
        match self.scope {
            TrackingScope::Global => GLOBAL_SCOPE_KEY.to_string(),
            TrackingScope::Invocation => ctx.invocation_id().to_string(),
        }
    }

    /// Drops the tool's count, and the scope once it is empty.
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

    /// The result for a failure within the retry limit: reflection guidance and the retry count.
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
                error_details: &format_error_details(tool, err),
                args_summary: &format_tool_args(args),
                retry_count,
                max_retries: self.max_retries,
            },
        );
        response_map(tool, err, retry_count, &rendered)
    }

    /// The result for a failure past the retry limit: the template's `RetryCount` and
    /// `MaxRetries` are both `max_retries`, as is the map's `retry_count`.
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
                error_details: &format_error_details(tool, err),
                args_summary: &format_tool_args(args),
                retry_count: self.max_retries,
                max_retries: self.max_retries,
            },
        );
        response_map(tool, err, self.max_retries, &rendered)
    }
}

/// Whether `err` is, or wraps, `ToolError::ConfirmationRequired` or
/// `ToolError::ConfirmationRejected`.
/// The flow hands confirmation sentinels over wrapped (`ToolError::Execution` around a named
/// wrapper whose `source()` is the sentinel), so this walks the chain. Transparent
/// layers (`ToolError::Execution`, `SharedToolError`, `AgentError::Other`) forward `source()` to
/// their inner error's source, skipping the inner error itself, so the walk descends into them
/// directly; any other error is followed through `source()`.
fn is_confirmation_error(err: &ToolError) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        cur = if let Some(te) = e.downcast_ref::<ToolError>() {
            match te {
                ToolError::ConfirmationRequired | ToolError::ConfirmationRejected => return true,
                ToolError::Execution(inner) => Some(&**inner),
                _ => std::error::Error::source(te),
            }
        } else if let Some(shared) = e.downcast_ref::<SharedToolError>() {
            Some(&*shared.0)
        } else if let Some(AgentError::Other(inner)) = e.downcast_ref::<AgentError>() {
            Some(&**inner)
        } else {
            e.source()
        };
    }
    false
}

/// The original tool error, keeping the caller's `Arc` so the very same error value is returned.
fn original_error(err: &Arc<ToolError>) -> AgentError {
    AgentError::Other(Box::new(SharedToolError(Arc::clone(err))))
}

/// Whether the invocation has a long-running call that no user
/// function response has answered yet. A failure response from this plugin also settles the
/// call, since no answer will follow.
fn has_open_long_running_call(sess: &dyn Session, invocation_id: &str) -> bool {
    let mut open: std::collections::HashSet<String> = std::collections::HashSet::new();
    for ev in sess.events() {
        if ev.invocation_id != invocation_id {
            continue;
        }
        open.extend(ev.long_running_tool_ids.iter().cloned());
        let Some(content) = &ev.response.content else {
            continue;
        };
        for fr in content
            .parts
            .iter()
            .filter_map(|p| p.function_response.as_ref())
        {
            if fr.id.is_empty() {
                continue;
            }
            if ev.author == "user"
                || fr.response.get("response_type").and_then(Value::as_str)
                    == Some(REFLECT_AND_RETRY_RESPONSE_TYPE)
            {
                open.remove(&fr.id);
            }
        }
    }
    !open.is_empty()
}

/// The `error_type` of a recovered tool panic.
const PANIC_ERROR_TYPE: &str = "panic";

/// The type and text of a failed call that the plugin may put in its response, which the model,
/// the session and REST clients all see. A tool panic, at any wrapping depth, becomes the type
/// `panic` and a generic message, because the panic's own text carries the panic value.
fn model_error(tool: &dyn Tool, err: &ToolError) -> (String, String) {
    match model_error_text(tool.name(), err) {
        (text, true) => (PANIC_ERROR_TYPE.to_string(), text),
        (text, false) => (error_type(err).to_string(), text),
    }
}

/// The result map shared by the reflection and exceeded outcomes. `reflection_guidance` is the
/// rendered template with surrounding whitespace trimmed.
fn response_map(
    tool: &dyn Tool,
    err: &ToolError,
    retry_count: i32,
    rendered: &str,
) -> Map<String, Value> {
    let (err_type, err_details) = model_error(tool, err);
    let mut m = Map::new();
    m.insert(
        "response_type".into(),
        json!(REFLECT_AND_RETRY_RESPONSE_TYPE),
    );
    m.insert("error_type".into(), json!(err_type));
    m.insert("error_details".into(), json!(err_details));
    m.insert("retry_count".into(), json!(retry_count));
    m.insert("reflection_guidance".into(), json!(rendered.trim()));
    m
}

/// The error's type name for the result's `error_type`: the `ToolError` variant path, such as
/// `ToolError::Execution`.
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

/// The template's `ErrorDetails`: `<type>: <details>` of [`model_error`].
fn format_error_details(tool: &dyn Tool, err: &ToolError) -> String {
    let (err_type, err_details) = model_error(tool, err);
    format!("{err_type}: {err_details}")
}

/// The tool arguments as JSON indented by two spaces (see [`go_indent`]); an empty map gives
/// `null`. Numbers are decoded as `f64`, as tool arguments are, hence [`as_go_floats`].
fn format_tool_args(args: &Map<String, Value>) -> String {
    if args.is_empty() {
        return "null".to_string();
    }
    go_indent(&go_json_marshal(&as_go_floats(&Value::Object(
        args.clone(),
    ))))
}

/// Indents compact, valid JSON by two spaces per level. `{`/`[` defer their newline until the
/// next element, so empty objects and arrays stay `{}`/`[]`; `:` gains one space; strings are
/// copied untouched.
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
                    // Empty object or array: no indent.
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

/// The values substituted into the reflection and exceeded templates.
struct TemplateData<'a> {
    tool_name: &'a str,
    error_details: &'a str,
    args_summary: &'a str,
    retry_count: i32,
    max_retries: i32,
}

/// Renders `reflection.md` / `exceeded.md`. Both templates use only the five `{{.Field}}`
/// actions, so one left-to-right substitution pass renders them exactly. Substituted values are
/// never re-scanned. An unknown action, which neither template has, is
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

    // With max_retries 0 the guidance says "failed consecutively 0 times".
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
    fn exceeded_after_the_limit_reports_max_retries_times() {
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
        // Go 4417f2d4: the template's RetryCount and MaxRetries are max_retries.
        assert_eq!(
            res["reflection_guidance"],
            json!(WANT_EXCEEDED.replace("consecutively 0 times", "consecutively 1 times"))
        );
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

    /// Stands in for the flow's private `llm::functions::NamedToolError` (Go `fmt.Errorf("error
    /// tool %q %w", name, sentinel)`): its `source()` is the sentinel.
    #[derive(Debug)]
    struct Wrapped(ToolError);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error tool \"t\" {}", self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// The confirmation sentinels as the flow hands them over (`wrap_confirmation_error`), plus
    /// further layers (`SharedToolError`, `AgentError::Other`) a wrapping tool might add.
    fn wrapped_sentinels() -> Vec<ToolError> {
        let mut v = Vec::new();
        for s in [
            || ToolError::ConfirmationRequired,
            || ToolError::ConfirmationRejected,
        ] {
            v.push(ToolError::Execution(Box::new(Wrapped(s()))));
            v.push(ToolError::Execution(Box::new(SharedToolError(Arc::new(
                ToolError::Execution(Box::new(Wrapped(s()))),
            )))));
            v.push(ToolError::Execution(Box::new(AgentError::Other(Box::new(
                SharedToolError(Arc::new(s())),
            )))));
        }
        v
    }

    // Go errors.Is unwraps %w, so the flow-wrapped sentinels pass through uncounted too.
    #[test]
    fn wrapped_confirmation_errors_pass_through_uncounted() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig {
            max_retries: 0,
            error_if_retry_exceeded: true,
            ..Default::default()
        });
        let ctx = Context::builder().build();
        for e in wrapped_sentinels() {
            let shown = e.to_string();
            let res = r.on_tool_error(&ctx, &NamedTool("t"), &Map::new(), &Arc::new(e));
            assert!(matches!(res, Ok(None)), "{shown}: {res:?}");
        }
        let r = RetryAndReflect::new(&RetryAndReflectConfig::default());
        for e in wrapped_sentinels() {
            let res = r.on_tool_error(&ctx, &NamedTool("t"), &Map::new(), &Arc::new(e));
            assert!(matches!(res, Ok(None)), "{res:?}");
        }
        assert!(r.scoped_failure_counters.lock().is_empty());
    }

    #[test]
    fn non_confirmation_wrapped_error_is_counted() {
        let r = RetryAndReflect::new(&RetryAndReflectConfig::default());
        let err = Arc::new(ToolError::Execution(Box::new(Wrapped(
            ToolError::InvalidArgument("bad".into()),
        ))));
        let res = r
            .on_tool_error(
                &Context::builder().build(),
                &NamedTool("t"),
                &Map::new(),
                &err,
            )
            .unwrap()
            .unwrap();
        assert_eq!(res["retry_count"], json!(1));
    }

    // Covers the plugin's closures: they share one state and pass the original error through.
    #[tokio::test]
    async fn plugin_callbacks_share_state_and_skip_confirmation() {
        let p = new_retry_and_reflect_plugin(RetryAndReflectConfig {
            max_retries: 1,
            error_if_retry_exceeded: true,
            ..Default::default()
        })
        .unwrap();
        let on_err = p.on_tool_error_callback().unwrap();
        let after = p.after_tool_callback().unwrap();
        let ctx = Context::builder().invocation_id("inv").build();
        let tool: Arc<dyn Tool> = Arc::new(NamedTool("t"));

        let confirm = Arc::new(ToolError::Execution(Box::new(Wrapped(
            ToolError::ConfirmationRequired,
        ))));
        let res = on_err(ctx.clone(), tool.clone(), Map::new(), confirm).await;
        assert!(matches!(res, Ok(None)), "{res:?}");

        let err = fail();
        let res = on_err(ctx.clone(), tool.clone(), Map::new(), err.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res["retry_count"], json!(1));
        // The reflection result does not reset; a real success does.
        let res = after(ctx.clone(), tool.clone(), Map::new(), res, None).await;
        assert!(matches!(res, Ok(None)), "{res:?}");
        let res = after(ctx.clone(), tool.clone(), Map::new(), Map::new(), None).await;
        assert!(matches!(res, Ok(None)), "{res:?}");
        let res = on_err(ctx.clone(), tool.clone(), Map::new(), err.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(res["retry_count"], json!(1), "success reset the count");

        let got = on_err(ctx, tool, Map::new(), err.clone())
            .await
            .unwrap_err();
        let AgentError::Other(inner) = &got else {
            panic!("want AgentError::Other, got {got:?}");
        };
        let shared = inner.downcast_ref::<SharedToolError>().unwrap();
        assert!(Arc::ptr_eq(&shared.0, &err));
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

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct NoArgs {}

    /// Runner-level: a confirmation-requiring FunctionTool's error, as the flow really wraps it
    /// (`error tool "<name>" requires confirmation, ...`), passes the guard. The model's
    /// function response is Go's confirmation error, not reflection guidance, and no failure
    /// is counted.
    #[tokio::test]
    async fn runner_confirmation_error_passes_through_uncounted() {
        use gadk_genai::{Content, FunctionCall};
        use gadk_model::LlmResponse;

        use crate::llm::{LlmAgentConfig, agent as llm_agent};
        use crate::testutil::{MockModel, TestAgentRunner, collect_events};
        use crate::tool::{FunctionTool, FunctionToolConfig};

        let secure = FunctionTool::new(
            FunctionToolConfig {
                require_confirmation: true,
                ..FunctionToolConfig::new("secure_action", "needs approval")
            },
            |_ctx: Context, _: NoArgs| async move { Ok::<_, ToolError>(json!({"done": true})) },
        )
        .unwrap();
        let call = Content::from_function_call(
            FunctionCall {
                id: "fc1".into(),
                name: "secure_action".into(),
                ..Default::default()
            },
            "model",
        );
        let model = MockModel::new(vec![
            Ok(LlmResponse {
                content: Some(call),
                ..Default::default()
            }),
            Ok(LlmResponse {
                content: Some(Content::from_text("done", "model")),
                ..Default::default()
            }),
        ]);
        let agent = llm_agent::new(LlmAgentConfig {
            name: "a".into(),
            model: Some(model.clone()),
            tools: vec![secure],
            ..Default::default()
        })
        .unwrap();
        let state = Arc::new(RetryAndReflect::new(&RetryAndReflectConfig::default()));
        let runner = TestAgentRunner::with_plugins(
            agent,
            crate::runner::PluginConfig {
                plugins: vec![plugin_for(state.clone())],
                close_timeout: std::time::Duration::from_secs(1),
            },
        )
        .await;
        let events = collect_events(runner.run("go")).await.unwrap();

        let want = json!({
            "error": "error tool \"secure_action\" requires confirmation, please approve or reject"
        });
        let responses: Vec<Value> = events
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_response.as_ref())
            .filter(|fr| fr.name == "secure_action")
            .map(|fr| Value::Object(fr.response.clone()))
            .collect();
        assert_eq!(responses, vec![want.clone()]);
        // The run pauses for the confirmation (as in Go); the model sees the response on the
        // next turn.
        assert_eq!(model.requests().len(), 1);
        collect_events(runner.run("next")).await.unwrap();
        let requests = model.requests();
        assert_eq!(requests.len(), 2);
        let sent: Vec<Value> = requests[1]
            .contents
            .iter()
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_response.as_ref())
            .filter(|fr| fr.name == "secure_action")
            .map(|fr| Value::Object(fr.response.clone()))
            .collect();
        assert_eq!(sent, vec![want]);
        let counters = state.scoped_failure_counters.lock();
        assert!(
            counters.values().all(|m| m.values().all(|&n| n == 0)),
            "{counters:?}"
        );
    }
}
