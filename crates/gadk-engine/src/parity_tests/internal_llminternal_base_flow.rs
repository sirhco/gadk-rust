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

//! Parity port of Go internal/llminternal/base_flow_test.go.

use std::{
    any::Any,
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt, future::BoxFuture, stream::BoxStream};
use gadk_agent::{
    AfterToolCallback, AgentConfig, AgentError, BeforeToolCallback, Context, InvocationContext,
    OnToolErrorCallback, ReadonlyContext, RunConfig, RunnableTool, StreamingMode, Tool, ToolError,
    Toolset, agent,
};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::{
    llm::{
        config::LlmAgentConfig,
        flow::{Flow, MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS, RequestProcessor, is_thought_only_turn},
        functions::{
            call_tool, displayable_tool_result_text, merge_event_actions,
            merge_parallel_function_response_events,
        },
        processors::{capture_logs, test_llm_agent, tools::tools_processor},
    },
    testutil::MockModel,
    workflow::route::go_quote,
};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

type RunFn = Arc<dyn Fn() -> Result<Map<String, Value>, ToolError> + Send + Sync>;

/// Go mockFunctionTool: optional run closure, no declaration.
struct MockFunctionTool {
    name: String,
    run: Option<RunFn>,
}

impl MockFunctionTool {
    fn arc(name: &str, run: Option<RunFn>) -> Arc<dyn Tool> {
        Arc::new(Self {
            name: name.into(),
            run,
        })
    }
}

impl Tool for MockFunctionTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "mock tool"
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for MockFunctionTool {
    async fn run(&self, _: Context, _: Value) -> Result<Map<String, Value>, ToolError> {
        self.run.as_ref().map_or(Ok(Map::new()), |f| f())
    }
}

fn before(r: Result<Option<Value>, &'static str>) -> BeforeToolCallback {
    Arc::new(move |_, _, _| {
        let r = r.clone();
        Box::pin(async move { r.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

type AfterFn = dyn Fn(&Map<String, Value>, Option<&ToolError>) -> Result<Option<Value>, &'static str>
    + Send
    + Sync;

/// An after-tool callback that inspects the incoming result/error as the Go closure does.
fn after(
    f: impl Fn(&Map<String, Value>, Option<&ToolError>) -> Result<Option<Value>, &'static str>
    + Send
    + Sync
    + 'static,
) -> AfterToolCallback {
    let f: Arc<AfterFn> = Arc::new(f);
    Arc::new(move |_, _, _, result, err| {
        let r = f(&result, err.as_deref());
        Box::pin(async move { r.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

/// An on-tool-error callback that inspects the incoming error as the Go closure does.
fn on_error(
    f: impl Fn(&ToolError) -> Result<Option<Value>, &'static str> + Send + Sync + 'static,
) -> OnToolErrorCallback {
    let f = Arc::new(f);
    Arc::new(move |_, _, _, err| {
        let r = f(&err);
        Box::pin(async move { r.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

struct CallToolCase {
    name: &'static str,
    tool: Arc<dyn Tool>,
    before: Vec<BeforeToolCallback>,
    after: Vec<AfterToolCallback>,
    on_error: Vec<OnToolErrorCallback>,
    want: Value,
}

/// A tool error wrapped by a callback that handles it and returns its own (Go
/// `fmt.Errorf("lookup failed: %w", err)`).
#[derive(Debug)]
struct WrappedToolError(Arc<ToolError>);

impl std::fmt::Display for WrappedToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lookup failed: {}", self.0)
    }
}

impl std::error::Error for WrappedToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NoArgs {}

/// Go checkPanicCallbackErrs: a callback of each of the given kinds recorded itself in errs,
/// and was handed the panic value.
// parity: adapted: Go also requires "stack:" and "runtime/debug.Stack" in the error; a Rust
// panic payload carries no stack (gadk FunctionTool documents this), so only "boom" is checked.
fn check_panic_callback_errs(
    errs: &parking_lot::Mutex<BTreeMap<&'static str, Option<String>>>,
    kinds: &[&str],
) {
    let errs = errs.lock();
    for kind in kinds {
        match errs.get(kind) {
            None => panic!("{kind} callback was not called for the panic"),
            Some(None) => panic!("{kind} callback got a nil error for the panic"),
            Some(Some(err)) => assert!(
                err.contains("boom"),
                "{kind} callback error is missing \"boom\": {err}"
            ),
        }
    }
}

#[tokio::test]
#[ignore = "8a bug: call_tool sends a tool panic's text to the model; Go e83ab5b0 replaces it with \"Invoking `<tool>()` failed with an internal error.\" (also through a wrapping on-tool-error callback)"]
async fn test_call_tool() {
    let panic_tool = crate::tool::FunctionTool::new(
        crate::tool::FunctionToolConfig::new("panicTool", ""),
        |_ctx: Context, _args: NoArgs| async move {
            if true {
                panic!("boom");
            }
            Ok::<_, ToolError>(Map::new())
        },
    )
    .unwrap();
    // Callbacks run before the flow replaces the panic error with the generic message, so
    // each one records the error it was handed for its own row's check to assert on.
    type Recorded = Arc<parking_lot::Mutex<BTreeMap<&'static str, Option<String>>>>;
    let panic_callback_errs: Recorded = Arc::default();
    let wrapped_panic_callback_errs: Recorded = Arc::default();
    let record_on_error = |errs: &Recorded, wrap: bool| -> OnToolErrorCallback {
        let errs = errs.clone();
        Arc::new(move |_, _, _, err: Arc<ToolError>| {
            errs.lock().insert("OnToolError", Some(err.to_string()));
            let r = if wrap {
                Err(AgentError::Other(Box::new(WrappedToolError(err))))
            } else {
                Ok(None)
            };
            Box::pin(async move { r })
        })
    };
    let record_after = |errs: &Recorded| -> AfterToolCallback {
        let errs = errs.clone();
        Arc::new(move |_, _, _, _, err: Option<Arc<ToolError>>| {
            errs.lock().insert("AfterTool", err.map(|e| e.to_string()));
            Box::pin(async { Ok(None) })
        })
    };
    let ok = |v: Value| -> Option<RunFn> { Some(Arc::new(move || Ok(obj(v.clone())))) };
    let fail = |m: &'static str| -> Option<RunFn> {
        Some(Arc::new(move || Err(ToolError::Execution(m.into()))))
    };
    let never = || -> Option<RunFn> { Some(Arc::new(|| panic!("tool should not be called"))) };
    let tool = |run: Option<RunFn>| MockFunctionTool::arc("testTool", run);
    let cases = vec![
        CallToolCase {
            name: "tool runs successfully",
            tool: tool(ok(json!({"result": "success"}))),
            before: vec![],
            after: vec![],
            on_error: vec![],
            want: json!({"result": "success"}),
        },
        CallToolCase {
            name: "tool error",
            tool: tool(fail("tool error")),
            before: vec![],
            after: vec![],
            on_error: vec![],
            want: json!({"error": "tool error"}),
        },
        CallToolCase {
            name: "tool panic is hidden from the model but not from callbacks",
            tool: panic_tool.clone(),
            before: vec![],
            after: vec![record_after(&panic_callback_errs)],
            on_error: vec![record_on_error(&panic_callback_errs, false)],
            want: json!({"error": "Invoking `panicTool()` failed with an internal error."}),
        },
        CallToolCase {
            // An error on its way out of a tool is wrapped more than once: agenttool wraps
            // what the sub-agent run returned, and a callback that handles the error and
            // returns its own is free to wrap it.
            name: "wrapped tool panic is hidden from the model",
            tool: panic_tool.clone(),
            before: vec![],
            after: vec![],
            on_error: vec![record_on_error(&wrapped_panic_callback_errs, true)],
            want: json!({"error": "Invoking `panicTool()` failed with an internal error."}),
        },
        CallToolCase {
            name: "before callback returns result",
            tool: tool(never()),
            before: vec![
                before(Ok(Some(json!({"result": "intercepted"})))),
                before(Ok(Some(
                    json!({"result": "2nd callback should not be called"}),
                ))),
            ],
            after: vec![],
            on_error: vec![],
            want: json!({"result": "intercepted"}),
        },
        CallToolCase {
            name: "before callback returns error",
            tool: tool(never()),
            before: vec![
                before(Err("before callback error")),
                before(Err("unexpected error")),
            ],
            after: vec![],
            on_error: vec![],
            want: json!({"error": "before callback error"}),
        },
        CallToolCase {
            name: "after callback modifies result",
            tool: tool(ok(json!({"result": "original"}))),
            before: vec![],
            after: vec![after(|_, _| Ok(Some(json!({"result": "modified"}))))],
            on_error: vec![],
            want: json!({"result": "modified"}),
        },
        CallToolCase {
            name: "after callback handles error",
            tool: tool(fail("tool error")),
            before: vec![],
            after: vec![
                after(|_, err| Ok(err.is_some().then(|| json!({"result": "error handled"})))),
                after(|_, _| Ok(Some(json!({"result": "unexpected output"})))),
            ],
            on_error: vec![],
            want: json!({"result": "error handled"}),
        },
        CallToolCase {
            name: "after callback returns error",
            tool: tool(ok(json!({"result": "success"}))),
            before: vec![],
            after: vec![
                after(|_, _| Err("after callback error")),
                after(|_, _| Err("unexpected error")),
            ],
            on_error: vec![],
            want: json!({"error": "after callback error"}),
        },
        CallToolCase {
            name: "no-op callbacks return func results",
            tool: tool(ok(json!({"result": "success"}))),
            before: vec![before(Ok(None))],
            after: vec![after(|_, _| Ok(None))],
            on_error: vec![],
            want: json!({"result": "success"}),
        },
        CallToolCase {
            name: "before callback result passed to after callback",
            tool: tool(never()),
            before: vec![before(Ok(Some(json!({"result": "from_before"}))))],
            after: vec![after(|result, _| {
                if result.get("result") != Some(&json!("from_before")) {
                    return Err("unexpected result in after callback");
                }
                Ok(Some(json!({"result": "from_after"})))
            })],
            on_error: vec![],
            want: json!({"result": "from_after"}),
        },
        CallToolCase {
            name: "before callback error passed to after callback",
            tool: tool(never()),
            before: vec![before(Err("error_from_before"))],
            after: vec![after(|_, err| {
                if err.map(ToString::to_string).as_deref() != Some("error_from_before") {
                    return Err("unexpected error in after callback");
                }
                Ok(Some(json!({"result": "error_handled_in_after"})))
            })],
            on_error: vec![],
            want: json!({"result": "error_handled_in_after"}),
        },
        CallToolCase {
            name: "before callback error passed to on tool error callback",
            tool: tool(never()),
            before: vec![before(Err("error_from_before"))],
            after: vec![],
            on_error: vec![on_error(|err| {
                assert_eq!(
                    err.to_string(),
                    "error_from_before",
                    "unexpected error in on tool error callback"
                );
                Ok(Some(
                    json!({"result": "error_handled_in_on_tool_error_callback"}),
                ))
            })],
            want: json!({"result": "error_handled_in_on_tool_error_callback"}),
        },
        CallToolCase {
            name: "before callback error passed to on tool error callback and after tool called",
            tool: tool(never()),
            before: vec![before(Err("error_from_before"))],
            after: vec![after(|_, err| {
                if err.is_some() {
                    return Err("unexpected error in after callback");
                }
                Ok(Some(json!({"result": "from_after"})))
            })],
            on_error: vec![on_error(|err| {
                assert_eq!(
                    err.to_string(),
                    "error_from_before",
                    "unexpected error in on tool error callback"
                );
                Ok(Some(
                    json!({"result": "error_handled_in_on_tool_error_callback"}),
                ))
            })],
            want: json!({"result": "from_after"}),
        },
        CallToolCase {
            name: "before callback error passed to on tool error callback and passed to after tool called",
            tool: tool(never()),
            before: vec![before(Err("error_from_before"))],
            after: vec![after(|_, err| {
                if err.map(ToString::to_string).as_deref() != Some("error_from_on_tool_error") {
                    return Err("unexpected error in after callback");
                }
                Err("error_from_after_tool")
            })],
            on_error: vec![on_error(|err| {
                assert_eq!(
                    err.to_string(),
                    "error_from_before",
                    "unexpected error in on tool error callback"
                );
                Err("error_from_on_tool_error")
            })],
            want: json!({"error": "error_from_after_tool"}),
        },
        CallToolCase {
            name: "before callback error passed to on tool error callback and passed to after tool called and handled",
            tool: tool(never()),
            before: vec![before(Err("error_from_before"))],
            after: vec![after(|_, err| {
                if err.map(ToString::to_string).as_deref() != Some("error_from_on_tool_error") {
                    return Err("unexpected error in after callback");
                }
                Ok(Some(
                    json!({"result": "error_handled_in_on_tool_error_callback"}),
                ))
            })],
            on_error: vec![on_error(|err| {
                assert_eq!(
                    err.to_string(),
                    "error_from_before",
                    "unexpected error in on tool error callback"
                );
                Err("error_from_on_tool_error")
            })],
            want: json!({"result": "error_handled_in_on_tool_error_callback"}),
        },
    ];
    assert_eq!(cases.len(), 16);
    for tc in cases {
        let is_panic_row = tc.name.contains("panic");
        let flow = Flow {
            before_tool_callbacks: tc.before,
            after_tool_callbacks: tc.after,
            on_tool_error_callbacks: tc.on_error,
            ..Default::default()
        };
        let tool_ctx = InvocationContext::builder()
            .build_invocation()
            .into_context();
        let got = call_tool(
            &flow.tool_callbacks(),
            tool_ctx,
            tc.tool,
            if is_panic_row {
                Map::new()
            } else {
                obj(json!({"key": "value"}))
            },
        )
        .await;
        assert_eq!(got.map(Value::Object), Some(tc.want), "{}", tc.name);
        match tc.name {
            "tool panic is hidden from the model but not from callbacks" => {
                check_panic_callback_errs(&panic_callback_errs, &["OnToolError", "AfterTool"]);
            }
            "wrapped tool panic is hidden from the model" => {
                check_panic_callback_errs(&wrapped_panic_callback_errs, &["OnToolError"]);
            }
            _ => {}
        }
    }
}

/// Go fakeTool: a tool that cannot run (not a function tool).
struct PlainTool;

impl Tool for PlainTool {
    fn name(&self) -> &str {
        "plain"
    }
    fn description(&self) -> &str {
        ""
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go mockFunctionTool `echo`: returns `{"args": args}`.
struct EchoTool;

impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "mock tool"
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for EchoTool {
    async fn run(&self, _: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        Ok(obj(json!({"args": args})))
    }
}

type ArgEdit = fn(&mut Map<String, Value>);

#[tokio::test]
#[ignore = "8a bug: a before-tool callback's argument edits do not reach the tool (gadk BeforeToolCallback gets an owned copy and has no way to hand edited args back; Go e0cbfd30 runs the tool with the callbacks' copy)"]
async fn test_handle_function_calls_callback_arg_edits_do_not_reach_the_call_event() {
    let set_input: ArgEdit = |args| {
        args.insert("input_str".into(), json!("modified_input"));
    };
    let before_cb = |edit: ArgEdit| -> BeforeToolCallback {
        Arc::new(move |_, _, mut args| {
            edit(&mut args);
            Box::pin(async { Ok(None) })
        })
    };
    let echo_args: OnToolErrorCallback = Arc::new(move |_, _, mut args, _| {
        set_input(&mut args);
        Box::pin(async move { Ok(Some(obj(json!({"args": args})))) })
    });
    // (name, tool name, call args, before, on tool error, want response, want call args)
    type ArgEditCase = (
        &'static str,
        &'static str,
        Value,
        Option<BeforeToolCallback>,
        Option<OnToolErrorCallback>,
        Value,
        Value,
    );
    let cases: Vec<ArgEditCase> = vec![
        (
            "before-tool callback adds an argument",
            "echo",
            json!({}),
            Some(before_cb(set_input)),
            None,
            json!({"args": {"input_str": "modified_input"}}),
            json!({}),
        ),
        (
            "before-tool callback adds an argument to a call without arguments",
            "echo",
            Value::Null,
            Some(before_cb(set_input)),
            None,
            json!({"args": {"input_str": "modified_input"}}),
            Value::Null,
        ),
        (
            "before-tool callback edits a nested object",
            "echo",
            json!({"filter": {"limit": 1}}),
            Some(before_cb(|args| {
                args["filter"]["limit"] = json!(2);
            })),
            None,
            json!({"args": {"filter": {"limit": 2}}}),
            json!({"filter": {"limit": 1}}),
        ),
        (
            "before-tool callback edits a nested list",
            "echo",
            json!({"tags": ["a", {"k": "v"}]}),
            Some(before_cb(|args| {
                args["tags"][0] = json!("b");
                args["tags"][1]["k"] = json!("w");
            })),
            None,
            json!({"args": {"tags": ["b", {"k": "w"}]}}),
            json!({"tags": ["a", {"k": "v"}]}),
        ),
        (
            "nil nested values stay nil",
            "echo",
            json!({"filter": null, "tags": null}),
            None,
            None,
            json!({"args": {"filter": null, "tags": null}}),
            json!({"filter": null, "tags": null}),
        ),
        (
            "on-tool-error callback edits the arguments of an unknown tool",
            "missing",
            json!({"input_str": "original"}),
            None,
            Some(echo_args.clone()),
            json!({"args": {"input_str": "modified_input"}}),
            json!({"input_str": "original"}),
        ),
        (
            "on-tool-error callback edits the arguments of a tool that cannot run",
            "plain",
            json!({"input_str": "original"}),
            None,
            Some(echo_args.clone()),
            json!({"args": {"input_str": "modified_input"}}),
            json!({"input_str": "original"}),
        ),
    ];
    for (name, tool_name, call_args, before, on_tool_error, want_response, want_call_args) in cases
    {
        let flow = Flow {
            before_tool_callbacks: before.into_iter().collect(),
            on_tool_error_callbacks: on_tool_error.into_iter().collect(),
            ..Default::default()
        };
        let call_ev = {
            let mut ev = Event::new(&Platform::default(), "inv_1");
            ev.response.content = Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: "call_1".into(),
                        name: tool_name.into(),
                        args: obj(call_args),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            });
            ev
        };
        let tools: std::collections::HashMap<String, Arc<dyn Tool>> = [
            ("echo".to_owned(), Arc::new(EchoTool) as Arc<dyn Tool>),
            ("plain".to_owned(), Arc::new(PlainTool) as Arc<dyn Tool>),
        ]
        .into_iter()
        .collect();
        let ctx = agent_1_ctx();
        let ev = crate::llm::functions::handle_function_calls(&ctx, &flow, &tools, &call_ev, None)
            .await
            .unwrap_or_else(|e| panic!("{name}: handleFunctionCalls() error = {e}"))
            .unwrap_or_else(|| panic!("{name}: handleFunctionCalls() = None"));
        let got = &ev.response.content.as_ref().unwrap().parts[0]
            .function_response
            .as_ref()
            .unwrap()
            .response;
        assert_eq!(
            Value::Object(got.clone()),
            want_response,
            "{name}: function response"
        );
        let call_args_after = &call_ev.response.content.as_ref().unwrap().parts[0]
            .function_call
            .as_ref()
            .unwrap()
            .args;
        assert_eq!(
            Value::Object(call_args_after.clone()),
            if want_call_args.is_null() {
                json!({})
            } else {
                want_call_args
            },
            "{name}: function call args"
        );
    }
}

#[test]
fn test_displayable_tool_result_text() {
    for (name, result, want) in [
        ("empty result is not displayable", json!({}), None),
        (
            "error result is not displayable",
            json!({"error": "boom"}),
            None,
        ),
        (
            "empty string result is not displayable",
            json!({"result": ""}),
            None,
        ),
        (
            "nil result is not displayable",
            json!({"result": null}),
            None,
        ),
        (
            "single plain-text result key is shown as-is",
            json!({"result": "sub-agent output"}),
            Some("sub-agent output"),
        ),
        (
            "non-string result key is rendered as JSON",
            json!({"result": 42}),
            Some(r#"{"result":42}"#),
        ),
        (
            "structured result is rendered as JSON",
            json!({"status": "ok", "count": 3}),
            Some(r#"{"count":3,"status":"ok"}"#),
        ),
        (
            "JSON output is not HTML-escaped",
            json!({"url": "https://example.com/a&b<c>"}),
            Some(r#"{"url":"https://example.com/a&b<c>"}"#),
        ),
    ] {
        assert_eq!(
            displayable_tool_result_text(&obj(result)).as_deref(),
            want,
            "{name}"
        );
    }
}

fn sd(v: Value) -> Option<Map<String, Value>> {
    Some(obj(v))
}

fn ad(kv: &[(&str, i64)]) -> Option<BTreeMap<String, i64>> {
    Some(kv.iter().map(|(k, v)| (k.to_string(), *v)).collect())
}

/// (name, base, other, want).
type MergeCase = (
    &'static str,
    Option<EventActions>,
    Option<EventActions>,
    Option<EventActions>,
);

#[test]
fn test_merge_event_actions() {
    let d = EventActions::default;
    let cases: Vec<MergeCase> = vec![
        ("both nil", None, None, None),
        (
            "other nil returns base",
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
            None,
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
        ),
        (
            "base nil returns other",
            None,
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
        ),
        (
            "state delta merged with non-overlapping keys",
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key2": "value2"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1", "key2": "value2"})),
                ..d()
            }),
        ),
        (
            "state delta merged with overlapping keys - later wins",
            Some(EventActions {
                state_delta: sd(json!({"key1": "original"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "overwritten"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "overwritten"})),
                ..d()
            }),
        ),
        (
            "state delta merged with nested map values",
            Some(EventActions {
                state_delta: sd(json!({"outer": {"key1": "value1", "key2": "value2"}})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"outer": {"key2": "updated", "key3": "value3"}})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(
                    json!({"outer": {"key1": "value1", "key2": "updated", "key3": "value3"}}),
                ),
                ..d()
            }),
        ),
        (
            "state delta merged with multiple keys from multiple tools",
            Some(EventActions {
                state_delta: sd(json!({"tool1_key": "tool1_value"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"tool2_key": "tool2_value", "tool3_key": "tool3_value"})),
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(
                    json!({"tool1_key": "tool1_value", "tool2_key": "tool2_value", "tool3_key": "tool3_value"}),
                ),
                ..d()
            }),
        ),
        (
            "base has nil state delta, other has values",
            Some(EventActions {
                skip_summarization: true,
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
            Some(EventActions {
                skip_summarization: true,
                state_delta: sd(json!({"key1": "value1"})),
                ..d()
            }),
        ),
        (
            "base has artifact delta, other has nil",
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: None,
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                ..d()
            }),
        ),
        (
            "base has nil artifact delta, other has values",
            Some(EventActions {
                artifact_delta: None,
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                ..d()
            }),
        ),
        (
            "artifact delta merged with non-overlapping keys",
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file3", 3), ("file4", 4)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 2), ("file3", 3), ("file4", 4)]),
                ..d()
            }),
        ),
        (
            "artifact delta merged with overlapping keys",
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 100)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 10), ("file2", 10)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 10), ("file2", 100)]),
                ..d()
            }),
        ),
        (
            "artifact deltas merged with partially overlapping keys",
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 100)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file2", 2), ("file3", 3)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", 1), ("file2", 100), ("file3", 3)]),
                ..d()
            }),
        ),
        (
            "artifact deltas merged with negative version preserved",
            Some(EventActions {
                artifact_delta: None,
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", -1)]),
                ..d()
            }),
            Some(EventActions {
                artifact_delta: ad(&[("file1", -1)]),
                ..d()
            }),
        ),
        (
            "skip summarization merging - any true wins",
            Some(EventActions {
                skip_summarization: false,
                ..d()
            }),
            Some(EventActions {
                skip_summarization: true,
                ..d()
            }),
            Some(EventActions {
                skip_summarization: true,
                ..d()
            }),
        ),
        (
            "escalate merging - any true wins",
            Some(EventActions {
                escalate: false,
                ..d()
            }),
            Some(EventActions {
                escalate: true,
                ..d()
            }),
            Some(EventActions {
                escalate: true,
                ..d()
            }),
        ),
        (
            "transfer to agent - last wins",
            Some(EventActions {
                transfer_to_agent: "agent1".into(),
                ..d()
            }),
            Some(EventActions {
                transfer_to_agent: "agent2".into(),
                ..d()
            }),
            Some(EventActions {
                transfer_to_agent: "agent2".into(),
                ..d()
            }),
        ),
        (
            "all fields merged correctly",
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1"})),
                artifact_delta: ad(&[("file1", 1)]),
                skip_summarization: false,
                transfer_to_agent: "agent1".into(),
                escalate: false,
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key2": "value2"})),
                artifact_delta: ad(&[("file2", 2)]),
                skip_summarization: true,
                transfer_to_agent: "agent2".into(),
                escalate: true,
                ..d()
            }),
            Some(EventActions {
                state_delta: sd(json!({"key1": "value1", "key2": "value2"})),
                artifact_delta: ad(&[("file1", 1), ("file2", 2)]),
                skip_summarization: true,
                transfer_to_agent: "agent2".into(),
                escalate: true,
                ..d()
            }),
        ),
    ];
    assert_eq!(cases.len(), 18);
    for (name, base, other, want) in cases {
        assert_eq!(merge_event_actions(base, other), want, "{name}");
    }
}

/// Go mockToolset: no tools, no request processing.
struct MockToolset(&'static str);

#[async_trait]
impl Toolset for MockToolset {
    fn name(&self) -> &str {
        self.0
    }
    async fn tools(&self, _: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(Vec::new())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

type ProcessFn = Arc<dyn Fn(&mut LlmRequest) -> Result<(), ToolError> + Send + Sync>;

/// Go mockRequestProcessorToolset.
struct MockRequestProcessorToolset {
    name: &'static str,
    process: ProcessFn,
}

#[async_trait]
impl Toolset for MockRequestProcessorToolset {
    fn name(&self) -> &str {
        self.name
    }
    async fn tools(&self, _: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(Vec::new())
    }
    async fn process_request(&self, _: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        (self.process)(req)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_preprocess_toolset() {
    let no_op = agent::new(AgentConfig {
        name: "no-op".into(),
        ..Default::default()
    })
    .unwrap();
    let failing: Arc<dyn Toolset> = Arc::new(MockRequestProcessorToolset {
        name: "toolset",
        process: Arc::new(|_| Err(ToolError::Execution("process error".into()))),
    });
    let modifying: Arc<dyn Toolset> = Arc::new(MockRequestProcessorToolset {
        name: "toolset_with_processor",
        process: Arc::new(|req| {
            req.model = "modified-model".into();
            Ok(())
        }),
    });
    let cases = vec![
        ("agent not llminternal.Agent", no_op, false, ""),
        (
            "agent has no toolsets",
            test_llm_agent(LlmAgentConfig::default()),
            false,
            "",
        ),
        (
            "toolset implements RequestProcessor, error",
            test_llm_agent(LlmAgentConfig {
                toolsets: vec![failing],
                ..Default::default()
            }),
            true,
            "",
        ),
        (
            "toolsets, success",
            test_llm_agent(LlmAgentConfig {
                toolsets: vec![
                    Arc::new(MockToolset("toolset_without_processor")),
                    modifying,
                ],
                ..Default::default()
            }),
            false,
            "modified-model",
        ),
    ];
    for (name, agent, want_error, want_model) in cases {
        let ctx = InvocationContext::builder().agent(agent).build_invocation();
        let mut req = LlmRequest::default();
        let flow = Flow::default();
        let res: Result<Vec<Event>, AgentError> =
            flow.preprocess(&ctx, &mut req).try_collect().await;
        assert_eq!(res.is_err(), want_error, "{name}: {res:?}");
        assert_eq!(req.model, want_model, "{name}");
    }
}

/// Go fnRespEvent: a function-response event carrying a single text part.
/// A request processor that yields two events, as the auth preprocessor does when a tool it
/// runs again asks for another credential.
fn two_events<'a>(
    _: &'a InvocationContext,
    _: &'a mut LlmRequest,
    _: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async {
        Ok(vec![
            Event::new(&Platform::default(), "inv"),
            Event::new(&Platform::default(), "inv"),
        ])
    })
}

// parity: adapted: Go panics when a range-over-func body runs after the loop returned; a
// Rust consumer that stops simply drops the stream, so the port checks that taking one event
// and dropping the stream completes.
#[tokio::test]
async fn test_preprocess_stops_when_the_consumer_does() {
    let flow = Flow {
        request_processors: vec![two_events],
        ..Default::default()
    };
    let ctx = InvocationContext::builder()
        .agent(test_llm_agent(LlmAgentConfig::default()))
        .build_invocation();
    let mut req = LlmRequest::default();
    let mut events = flow.preprocess(&ctx, &mut req);
    if let Some(item) = events.next().await {
        item.expect("preprocess() error");
    }
    drop(events);
}

fn fn_resp_event(text: &str) -> Event {
    let mut ev = Event::new(&Platform::default(), "inv");
    ev.response = LlmResponse {
        content: Some(Content {
            role: "user".into(),
            parts: vec![Part {
                text: text.into(),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    ev
}

fn part_count(ev: &Event) -> usize {
    ev.response.content.as_ref().map_or(0, |c| c.parts.len())
}

#[test]
fn test_merge_parallel_function_response_events_nil_entries() {
    // first entry nil
    let got = merge_parallel_function_response_events(vec![None, Some(fn_resp_event("b"))])
        .unwrap()
        .expect("merged event");
    assert_eq!(part_count(&got), 1);
    // all entries nil
    assert!(
        merge_parallel_function_response_events(vec![None, None])
            .unwrap()
            .is_none()
    );
    // mixed nil and non-nil
    let got = merge_parallel_function_response_events(vec![
        Some(fn_resp_event("a")),
        None,
        Some(fn_resp_event("c")),
    ])
    .unwrap()
    .expect("merged event");
    assert_eq!(part_count(&got), 2);
}

#[test]
fn test_is_thought_only_turn() {
    let event = |partial: bool, parts: Vec<Part>| Event {
        response: LlmResponse {
            content: (!parts.is_empty()).then(|| Content {
                role: "model".into(),
                parts,
            }),
            partial,
            ..Default::default()
        },
        ..Default::default()
    };
    let thought = |t: &str| Part {
        text: t.into(),
        thought: true,
        ..Default::default()
    };
    let text = |t: &str| Part {
        text: t.into(),
        ..Default::default()
    };
    let call = || FunctionCall {
        name: "f".into(),
        ..Default::default()
    };
    let cases = vec![
        (
            "thought_text_only",
            Some(event(false, vec![thought("thinking")])),
            true,
        ),
        (
            "thought_plus_answer",
            Some(event(false, vec![thought("t"), text("answer")])),
            false,
        ),
        (
            "answer_only",
            Some(event(false, vec![text("answer")])),
            false,
        ),
        (
            "function_call",
            Some(event(
                false,
                vec![Part {
                    function_call: Some(call()),
                    ..Default::default()
                }],
            )),
            false,
        ),
        (
            "thought_then_signed_call",
            Some(event(
                false,
                vec![
                    thought("t"),
                    Part {
                        function_call: Some(call()),
                        thought_signature: b"sig".to_vec().into(),
                        ..Default::default()
                    },
                ],
            )),
            false,
        ),
        (
            "thought_plus_signature_only_part",
            Some(event(
                false,
                vec![
                    thought("t"),
                    Part {
                        thought_signature: b"sig".to_vec().into(),
                        ..Default::default()
                    },
                ],
            )),
            false,
        ),
        (
            "partial_thought",
            Some(event(true, vec![thought("t")])),
            false,
        ),
        ("empty_content", Some(event(false, vec![])), false),
        ("nil_event", None, false),
    ];
    for (name, ev, want) in cases {
        assert_eq!(is_thought_only_turn(ev.as_ref()), want, "{name}");
    }
}

/// Counts calls and records the `stream` argument (Go mockModelForTest with a recording closure).
#[derive(Default)]
struct StreamFlagModel {
    calls: AtomicUsize,
    stream: AtomicBool,
}

impl Llm for StreamFlagModel {
    fn name(&self) -> &str {
        "test-model"
    }
    fn generate_content(
        &self,
        _: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.stream.store(stream, Ordering::SeqCst);
        Box::pin(futures::stream::empty())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// parity: adapted: Go's nil RunConfig case folds into RunConfig::default() (Rust contexts always carry a RunConfig)
#[tokio::test]
async fn test_call_llm_streaming_mode_from_run_config() {
    for (name, rc, want) in [
        (
            "SSE streams",
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
            true,
        ),
        (
            "none does not stream",
            RunConfig {
                streaming_mode: StreamingMode::None,
                ..Default::default()
            },
            false,
        ),
        (
            "unset streaming mode does not stream",
            RunConfig::default(),
            false,
        ),
    ] {
        let m = Arc::new(StreamFlagModel::default());
        let model: Arc<dyn Llm> = m.clone();
        let flow = Arc::new(Flow {
            model: Some(model.clone()),
            ..Default::default()
        });
        let ctx = InvocationContext::builder()
            .run_config(rc)
            .build_invocation();
        let items: Vec<_> = flow
            .call_model(
                ctx,
                model,
                LlmRequest::default(),
                Arc::new(Default::default()),
            )
            .collect()
            .await;
        assert!(
            items.iter().all(Result::is_ok),
            "{name}: {:?}",
            items
                .iter()
                .filter_map(|r| r.as_ref().err())
                .collect::<Vec<_>>()
        );
        assert_eq!(m.calls.load(Ordering::SeqCst), 1, "{name}");
        assert_eq!(m.stream.load(Ordering::SeqCst), want, "{name}");
    }
}

/// Go alwaysThinkingModel: every call returns one numbered thought-only turn.
#[derive(Default)]
struct AlwaysThinkingModel {
    calls: AtomicUsize,
}

impl Llm for AlwaysThinkingModel {
    fn name(&self) -> &str {
        "always-thinking"
    }
    fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(futures::stream::iter([Ok(LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    text: format!("thinking {n}"),
                    thought: true,
                    ..Default::default()
                }],
            }),
            ..Default::default()
        })]))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn agent_1_ctx() -> InvocationContext {
    InvocationContext::builder()
        .invocation_id("inv_1")
        .agent(
            agent::new(AgentConfig {
                name: "agent_1".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .build_invocation()
}

/// Collects at most `limit` events, failing on any error.
async fn collect_bounded(flow: Arc<Flow>, ctx: InvocationContext, limit: usize) -> Vec<Event> {
    let mut s = flow.run(ctx);
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        out.push(item.expect("Run() yielded error"));
        if out.len() > limit {
            break;
        }
    }
    out
}

#[tokio::test]
async fn test_run_thought_only_turns_terminate() {
    let m = Arc::new(AlwaysThinkingModel::default());
    let flow = Arc::new(Flow {
        model: Some(m.clone()),
        ..Default::default()
    });
    let events = collect_bounded(flow, agent_1_ctx(), 50).await;
    assert!(
        events.len() <= 50,
        "Run() did not terminate on repeated thought-only turns"
    );
    assert_eq!(
        m.calls.load(Ordering::SeqCst),
        MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS
    );
    assert_eq!(events.len(), MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS);
    let last = events.last().unwrap();
    assert_eq!(
        last.response.content.as_ref().unwrap().parts,
        vec![Part {
            text: "thinking 10".into(),
            thought: true,
            ..Default::default()
        }]
    );
}

/// Go scriptedThinkingModel: replays `turns`, then repeats the last one forever.
struct ScriptedThinkingModel {
    turns: Vec<LlmResponse>,
    calls: AtomicUsize,
}

impl Llm for ScriptedThinkingModel {
    fn name(&self) -> &str {
        "scripted-thinking"
    }
    fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let i = self
            .calls
            .fetch_add(1, Ordering::SeqCst)
            .min(self.turns.len() - 1);
        Box::pin(futures::stream::iter([Ok(self.turns[i].clone())]))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn thought_turn() -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts: vec![Part {
                text: "thinking".into(),
                thought: true,
                ..Default::default()
            }],
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_run_thought_only_turn_count_resets() {
    let call = LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts: vec![Part {
                function_call: Some(FunctionCall {
                    id: "fc-1".into(),
                    name: "noop".into(),
                    args: Map::new(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let m = Arc::new(ScriptedThinkingModel {
        turns: vec![thought_turn(), thought_turn(), call, thought_turn()],
        calls: AtomicUsize::new(0),
    });
    let flow = Arc::new(Flow {
        model: Some(m.clone()),
        tools: parking_lot::RwLock::new(vec![MockFunctionTool::arc("noop", None)]),
        ..Default::default()
    });
    let events = collect_bounded(flow, agent_1_ctx(), 50).await;
    assert!(events.len() <= 50, "Run() did not terminate");
    // 2 thought-only turns, the function call resets the count, then a full run of thoughts.
    assert_eq!(
        m.calls.load(Ordering::SeqCst),
        2 + 1 + MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS
    );
}

/// A response without content is dropped unless it carries something the caller needs, such
/// as grounding metadata from a search tool.
#[tokio::test]
#[ignore = "8a bug: Flow::run drops a model response that carries only grounding metadata (Go fc6de07d keeps it)"]
async fn test_run_keeps_grounding_only_response() {
    let grounding = gadk_genai::GroundingMetadata {
        web_search_queries: vec!["test query".into()],
        ..Default::default()
    };
    let cases = vec![
        (
            "grounding metadata only",
            LlmResponse {
                grounding_metadata: Some(grounding.clone()),
                ..Default::default()
            },
            vec![Some(grounding.clone())],
        ),
        ("empty response", LlmResponse::default(), vec![]),
    ];
    for (name, resp, want) in cases {
        let flow = Arc::new(Flow {
            model: Some(MockModel::new(vec![Ok(resp)])),
            ..Default::default()
        });
        let mut s = flow.run(agent_1_ctx());
        let mut got = Vec::new();
        while let Some(item) = s.next().await {
            let ev = item.unwrap_or_else(|e| panic!("{name}: Run() yielded error: {e}"));
            got.push(ev.response.grounding_metadata);
        }
        assert_eq!(got, want, "{name}: Run() event grounding metadata");
    }
}

fn text_content(t: &str) -> Option<Content> {
    Some(Content {
        role: "model".into(),
        parts: vec![Part {
            text: t.into(),
            ..Default::default()
        }],
    })
}

/// Case 3's request processor: yields a trailing partial event from a sub-agent.
fn sub_agent_partial<'a>(
    ctx: &'a InvocationContext,
    _: &'a mut LlmRequest,
    _: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.author = "sub-agent".into();
        ev.response = LlmResponse {
            content: text_content("I am a sub-agent"),
            partial: true,
            ..Default::default()
        };
        Ok(vec![ev])
    })
}

/// (name, model responses, request processors, want texts, want log author).
type PartialCase = (
    &'static str,
    Vec<Result<LlmResponse, ModelError>>,
    Vec<RequestProcessor>,
    Vec<&'static str>,
    &'static str,
);

// parity: adapted: Go log.Printf output is captured from tracing target "adk" via capture_logs
#[tokio::test]
async fn test_flow_run_partial_last_event() {
    let partial = |t: &str| {
        Ok(LlmResponse {
            content: text_content(t),
            partial: true,
            ..Default::default()
        })
    };
    let cases: Vec<PartialCase> = vec![
        (
            "single partial response completes without error",
            vec![partial("Hello")],
            vec![],
            vec!["Hello"],
            "test-agent",
        ),
        (
            "multiple partial responses complete without error",
            vec![partial("Hello"), partial(" World")],
            vec![],
            vec!["Hello", " World"],
            "test-agent",
        ),
        (
            "trailing partial from sub-agent completes without error",
            vec![],
            vec![sub_agent_partial as RequestProcessor],
            vec!["I am a sub-agent"],
            "sub-agent",
        ),
    ];
    for (name, responses, processors, want_texts, want_author) in cases {
        let agent = agent::new(AgentConfig {
            name: "test-agent".into(),
            ..Default::default()
        })
        .unwrap();
        let ctx = InvocationContext::builder()
            .agent(agent)
            .run_config(RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            })
            .build_invocation();
        let flow = Arc::new(Flow {
            model: Some(MockModel::with_stream(vec![responses]).name_as("test-model")),
            request_processors: processors,
            ..Default::default()
        });
        let (events, log) = capture_logs(collect_bounded(flow, ctx.clone(), 50)).await;
        assert!(events.len() <= 50, "{name}: Flow.Run() did not terminate");
        let texts: Vec<String> = events
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| {
                c.parts
                    .iter()
                    .filter(|p| !p.text.is_empty())
                    .map(|p| p.text.clone())
            })
            .collect();
        assert_eq!(texts, want_texts, "{name}");
        let want_log = format!(
            "adk: agent {} (invocation {}): step ended on a partial event from {}; the producer did not close its stream with an aggregated final event, so the turn will not appear in session history\n",
            go_quote("test-agent"),
            go_quote(ctx.invocation_id()),
            go_quote(want_author)
        );
        assert_eq!(log, want_log, "{name}");
    }
}

/// Go dynamicToolset: call 0 returns no tools, call 1+ returns `extra`.
struct DynamicToolset {
    calls: AtomicUsize,
    extra: Arc<dyn Tool>,
}

#[async_trait]
impl Toolset for DynamicToolset {
    fn name(&self) -> &str {
        "dynamic"
    }
    async fn tools(&self, _: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if n == 0 {
            Vec::new()
        } else {
            vec![self.extra.clone()]
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_tool_processor_re_evaluates_toolsets_each_step() {
    let ts = Arc::new(DynamicToolset {
        calls: AtomicUsize::new(0),
        extra: MockFunctionTool::arc("extra_tool", None),
    });
    let agent = test_llm_agent(LlmAgentConfig {
        tools: vec![MockFunctionTool::arc("base_tool", None)],
        toolsets: vec![ts.clone()],
        ..Default::default()
    });
    let ctx = InvocationContext::builder().agent(agent).build_invocation();
    let flow = Flow::default();

    tools_processor(&ctx, &mut LlmRequest::default(), &flow)
        .await
        .unwrap();
    assert_eq!(flow.tools.read().len(), 1, "after call 1");

    tools_processor(&ctx, &mut LlmRequest::default(), &flow)
        .await
        .unwrap();
    let names: Vec<String> = flow
        .tools
        .read()
        .iter()
        .map(|t| t.name().to_owned())
        .collect();
    assert_eq!(names, ["base_tool", "extra_tool"], "after call 2");

    assert_eq!(ts.calls.load(Ordering::SeqCst), 2);
}

// parity: skipped TestCallLLMSkipsNilResponseAndContinues: a Rust Llm stream item is Result<LlmResponse, ModelError>; a nil response cannot be expressed

/// A model response event's id comes from the UUID provider on the context. generateContent
/// draws an id when a response arrives, never ahead of one, and draws one id for each distinct
/// event id across the responses it yields.
// parity: adapted: Go's "an empty response draws no ids" row yields a nil response, which a
// gadk model stream cannot; Go's erroring row yields a response with its error, a gadk stream
// item is one or the other, so the error item stands alone.
#[tokio::test]
#[ignore = "8a bug: flow::generate_content draws a fresh event id for every model stream item; Go 3188db98 shares one id across partials and the response that completes them, and ends it at an error"]
async fn test_generate_content_draws_one_id_per_event() {
    let text = |s: &str, partial: bool| -> Result<LlmResponse, ModelError> {
        Ok(LlmResponse {
            content: Some(Content::from_text(s, "model")),
            partial,
            ..Default::default()
        })
    };
    let boom = || Err(ModelError::Other("boom".into()));
    // (name, model stream items, want ids, want errors, want draws)
    type DrawCase = (
        &'static str,
        Vec<Result<LlmResponse, ModelError>>,
        Vec<&'static str>,
        Vec<bool>,
        usize,
    );
    let cases: Vec<DrawCase> = vec![
        ("no responses draw no ids", vec![], vec![], vec![], 0),
        (
            "one complete response",
            vec![text("done", false)],
            vec!["id-1"],
            vec![false],
            1,
        ),
        (
            "partials share the id of the response that completes them",
            vec![text("a", true), text("b", true), text("ab", false)],
            vec!["id-1", "id-1", "id-1"],
            vec![false, false, false],
            1,
        ),
        (
            "each complete response gets its own id",
            vec![text("first", false), text("second", false)],
            vec!["id-1", "id-2"],
            vec![false, false],
            2,
        ),
        (
            "an error ends the id",
            vec![text("a", true), boom(), text("after", false)],
            vec!["id-1", "id-1", "id-2"],
            vec![false, true, false],
            2,
        ),
    ];
    for (name, responses, want_ids, want_errs, want_draws) in cases {
        let draws = Arc::new(AtomicUsize::new(0));
        let counter = draws.clone();
        let platform = Platform::default().with_uuid(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            format!("id-{n}")
        });
        let ctx = InvocationContext::builder()
            .invocation_id("inv-1")
            .platform(platform)
            .build_invocation();
        let model = MockModel::with_stream(vec![responses]);
        let items: Vec<(Result<LlmResponse, ModelError>, String)> =
            crate::llm::flow::generate_content(&ctx, model, LlmRequest::default(), true)
                .collect()
                .await;
        let got_ids: Vec<&str> = items.iter().map(|(_, id)| id.as_str()).collect();
        let got_errs: Vec<bool> = items.iter().map(|(r, _)| r.is_err()).collect();
        assert_eq!(got_ids, want_ids, "{name}: event ids");
        assert_eq!(got_errs, want_errs, "{name}: errors");
        assert_eq!(
            draws.load(Ordering::SeqCst),
            want_draws,
            "{name}: ids drawn from the provider"
        );
    }
}
