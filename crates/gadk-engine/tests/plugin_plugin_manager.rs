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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go plugin/plugin_manager_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{
    AfterModelCallback, AfterToolCallback, AgentError, BeforeModelCallback, BeforeToolCallback,
    Context, EventStream, OnModelErrorCallback, OnToolErrorCallback, Plugin, ToolError,
};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::PluginConfig as RunnerPluginConfig;
use gadk_engine::testutil::{MockModel, TestAgentRunner};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall};
use gadk_model::LlmResponse;
use gadk_session::Event;
use serde_json::{Map, Value, json};

const NO_COOKIE: &str = "http: named cookie not present"; // Go http.ErrNoCookie text
const HIJACKED: &str = "http: connection has been hijacked"; // Go http.ErrHijacked text

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn err_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(" | ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

/// Collects events until the first error (Go testutil.Collect* keep the partial result).
async fn drain(mut s: EventStream) -> (Vec<Event>, Option<AgentError>) {
    let mut events = Vec::new();
    while let Some(item) = s.next().await {
        match item {
            Ok(e) => events.push(e),
            Err(e) => return (events, Some(e)),
        }
    }
    (events, None)
}

fn model_text(t: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_text(t, "model")),
        ..Default::default()
    }
}

// ---- tool callback helpers ----

#[derive(Clone)]
enum ToolBehavior {
    Returns(Value),
    Fails(&'static str),
    MustNotRun,
}

fn before_tool(out: Result<Option<Value>, &'static str>) -> BeforeToolCallback {
    Arc::new(move |_ctx, _tool, _args| {
        let out = out.clone();
        Box::pin(async move { out.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

fn before_tool_unexpected(
    bad: &Arc<AtomicBool>,
    out: Result<Option<Value>, &'static str>,
) -> BeforeToolCallback {
    let bad = bad.clone();
    let inner = before_tool(out);
    Arc::new(move |ctx, tool, args| {
        bad.store(true, SeqCst);
        inner(ctx, tool, args)
    })
}

/// `f(result, err_text)`; Go AfterToolCallback(ctx, tool, args, result, err).
fn after_tool(
    f: impl Fn(&Map<String, Value>, Option<String>) -> Result<Option<Value>, &'static str>
    + Send
    + Sync
    + 'static,
) -> AfterToolCallback {
    let f = Arc::new(f);
    Arc::new(move |_ctx, _tool, _args, result, err| {
        let out = f(&result, err.map(|e| e.to_string()));
        Box::pin(async move { out.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

/// `f(err_text)`; Go OnToolErrorCallback(ctx, tool, args, err).
fn on_tool_error(
    f: impl Fn(String) -> Result<Option<Value>, &'static str> + Send + Sync + 'static,
) -> OnToolErrorCallback {
    let f = Arc::new(f);
    Arc::new(move |_ctx, _tool, _args, err| {
        let out = f(err.to_string());
        Box::pin(async move { out.map(|o| o.map(obj)).map_err(AgentError::other) })
    })
}

struct CallToolCase {
    name: &'static str,
    tool: ToolBehavior,
    args: Value,
    before: Vec<BeforeToolCallback>,
    after: Vec<AfterToolCallback>,
    on_error: Vec<OnToolErrorCallback>,
    want: Value,
    skip_before: bool,
    skip_after: bool,
    skip_on_error: bool,
}

fn call_tool_cases(bad: &Arc<AtomicBool>) -> Vec<CallToolCase> {
    let bad_after = |bad: &Arc<AtomicBool>, out: Result<Option<Value>, &'static str>| {
        let bad = bad.clone();
        after_tool(move |_, _| {
            bad.store(true, SeqCst);
            out.clone()
        })
    };
    let base = |name| CallToolCase {
        name,
        tool: ToolBehavior::MustNotRun,
        args: json!({"key": "value"}),
        before: vec![],
        after: vec![],
        on_error: vec![],
        want: Value::Null,
        skip_before: false,
        skip_after: false,
        skip_on_error: false,
    };
    vec![
        CallToolCase {
            tool: ToolBehavior::Returns(json!({"result": "success"})),
            skip_on_error: true,
            want: json!({"result": "success"}),
            ..base("tool runs successfully")
        },
        CallToolCase {
            tool: ToolBehavior::Fails("tool error"),
            want: json!({"error": "tool error"}),
            ..base("tool error")
        },
        CallToolCase {
            before: vec![
                before_tool(Ok(Some(json!({"result": "intercepted"})))),
                before_tool_unexpected(
                    bad,
                    Ok(Some(json!({"result": "2nd callback should not be called"}))),
                ),
            ],
            skip_on_error: true,
            skip_before: true,
            want: json!({"result": "intercepted"}),
            ..base("before callback returns result")
        },
        CallToolCase {
            before: vec![
                before_tool(Err("before callback error")),
                before_tool_unexpected(bad, Err("unexpected error")),
            ],
            skip_before: true,
            want: json!({"error": "before callback error"}),
            ..base("before callback returns error")
        },
        CallToolCase {
            tool: ToolBehavior::Returns(json!({"result": "original"})),
            after: vec![after_tool(|_, _| Ok(Some(json!({"result": "modified"}))))],
            skip_after: true,
            skip_on_error: true,
            want: json!({"result": "modified"}),
            ..base("after callback modifies result")
        },
        CallToolCase {
            tool: ToolBehavior::Fails("tool error"),
            after: vec![
                after_tool(|_, err| Ok(err.map(|_| json!({"result": "error handled"})))),
                bad_after(bad, Ok(Some(json!({"result": "unexpected output"})))),
            ],
            skip_after: true,
            want: json!({"result": "error handled"}),
            ..base("after callback handles error and are run in symmetrical order")
        },
        CallToolCase {
            tool: ToolBehavior::Returns(json!({"result": "success"})),
            after: vec![
                after_tool(|_, _| Err("after callback error")),
                bad_after(bad, Err("unexpected error")),
            ],
            skip_on_error: true,
            skip_after: true,
            want: json!({"error": "after callback error"}),
            ..base("after callback returns error and are run in symmetrical order")
        },
        CallToolCase {
            tool: ToolBehavior::Returns(json!({"result": "success"})),
            before: vec![before_tool(Ok(None))],
            after: vec![after_tool(|_, _| Ok(None))],
            skip_on_error: true,
            want: json!({"result": "success"}),
            ..base("no-op callbacks return func results")
        },
        CallToolCase {
            before: vec![before_tool(Ok(Some(json!({"result": "from_before"}))))],
            after: vec![after_tool(|result, _| match result.get("result") {
                Some(v) if v == "from_before" => Ok(Some(json!({"result": "from_after"}))),
                _ => Err("unexpected result in after callback"),
            })],
            skip_on_error: true,
            skip_before: true,
            skip_after: true,
            want: json!({"result": "from_after"}),
            ..base("before callback result passed to after callback")
        },
        CallToolCase {
            before: vec![before_tool(Err("error_from_before"))],
            after: vec![after_tool(|_, err| match err.as_deref() {
                Some("error_from_before") => Ok(Some(json!({"result": "error_handled_in_after"}))),
                _ => Err("unexpected error in after callback"),
            })],
            skip_before: true,
            skip_after: true,
            want: json!({"result": "error_handled_in_after"}),
            ..base("before callback error passed to after callback")
        },
        CallToolCase {
            before: vec![before_tool(Err("error_from_before"))],
            on_error: vec![on_tool_error(|err| {
                if err == "error_from_before" {
                    Ok(Some(
                        json!({"result": "error_handled_in_on_tool_error_callback"}),
                    ))
                } else {
                    Err("unexpected error in on tool error callback")
                }
            })],
            skip_before: true,
            skip_on_error: true,
            want: json!({"result": "error_handled_in_on_tool_error_callback"}),
            ..base("before callback error passed to on tool error callback")
        },
        CallToolCase {
            before: vec![before_tool(Err("error_from_before"))],
            on_error: vec![on_tool_error(|err| {
                if err == "error_from_before" {
                    Ok(Some(
                        json!({"result": "error_handled_in_on_tool_error_callback"}),
                    ))
                } else {
                    Err("unexpected error in on tool error callback")
                }
            })],
            after: vec![after_tool(|_, err| match err {
                Some(_) => Err("unexpected error in after callback"),
                None => Ok(Some(json!({"result": "from_after"}))),
            })],
            skip_after: true,
            skip_before: true,
            skip_on_error: true,
            want: json!({"result": "from_after"}),
            ..base("before callback error passed to on tool error callback and after tool called")
        },
        CallToolCase {
            before: vec![before_tool(Err("error_from_before"))],
            on_error: vec![on_tool_error(|err| {
                if err == "error_from_before" {
                    Err("error_from_on_tool_error")
                } else {
                    Err("unexpected error in on tool error callback")
                }
            })],
            after: vec![after_tool(|_, err| match err.as_deref() {
                Some("error_from_on_tool_error") => Err("error_from_after_tool"),
                _ => Err("unexpected error in after callback"),
            })],
            skip_after: true,
            skip_on_error: true,
            skip_before: true,
            want: json!({"error": "error_from_after_tool"}),
            ..base(
                "before callback error passed to on tool error callback and passed to after tool called",
            )
        },
        CallToolCase {
            before: vec![before_tool(Err("error_from_before"))],
            on_error: vec![on_tool_error(|err| {
                if err == "error_from_before" {
                    Err("error_from_on_tool_error")
                } else {
                    Err("unexpected error in on tool error callback")
                }
            })],
            after: vec![after_tool(|_, err| match err.as_deref() {
                Some("error_from_on_tool_error") => Ok(Some(
                    json!({"result": "error_handled_in_after_tool_callback"}),
                )),
                _ => Err("unexpected error in after tool callback"),
            })],
            skip_after: true,
            skip_before: true,
            skip_on_error: true,
            want: json!({"result": "error_handled_in_after_tool_callback"}),
            ..base(
                "before callback error passed to on tool error callback and passed to after tool called and handled",
            )
        },
    ]
}

#[tokio::test]
async fn test_call_tool() {
    let bad = Arc::new(AtomicBool::new(false));
    for case in call_tool_cases(&bad) {
        bad.store(false, SeqCst);
        let n = case
            .before
            .len()
            .max(case.after.len())
            .max(case.on_error.len());
        let plugins: Vec<Plugin> = (0..n)
            .map(|i| {
                Plugin::new(gadk_agent::PluginConfig {
                    name: format!("plugin-{i}"),
                    before_tool_callback: case.before.get(i).cloned(),
                    after_tool_callback: case.after.get(i).cloned(),
                    on_tool_error_callback: case.on_error.get(i).cloned(),
                    ..Default::default()
                })
            })
            .collect();

        let model = MockModel::from_contents(vec![Content::from_function_call(
            FunctionCall {
                name: "testTool".into(),
                args: obj(case.args.clone()),
                ..Default::default()
            },
            "model",
        )]);
        let behavior = case.tool.clone();
        let tool_bad = bad.clone();
        let ft = FunctionTool::new(
            FunctionToolConfig::new("testTool", ""),
            move |_ctx: Context, _args: Map<String, Value>| {
                let out = match &behavior {
                    ToolBehavior::Returns(v) => Ok(obj(v.clone())),
                    ToolBehavior::Fails(m) => Err(ToolError::Execution((*m).into())),
                    ToolBehavior::MustNotRun => {
                        tool_bad.store(true, SeqCst);
                        Ok(Map::new())
                    }
                };
                async move { out }
            },
        )
        .unwrap();

        let (on_err_called, before_called, after_called) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (oe, bc, ac) = (
            on_err_called.clone(),
            before_called.clone(),
            after_called.clone(),
        );
        let agent = llm_agent::new(LlmAgentConfig {
            name: "test_agent".into(),
            model: Some(model),
            tools: vec![ft],
            on_tool_error_callbacks: vec![Arc::new(move |_c, _t, _a, _e| {
                oe.store(true, SeqCst);
                Box::pin(async { Ok(None) })
            })],
            before_tool_callbacks: vec![Arc::new(move |_c, _t, _a| {
                bc.store(true, SeqCst);
                Box::pin(async { Ok(None) })
            })],
            after_tool_callbacks: vec![Arc::new(move |_c, _t, _a, _r, _e| {
                ac.store(true, SeqCst);
                Box::pin(async { Ok(None) })
            })],
            ..Default::default()
        })
        .unwrap();

        let runner = TestAgentRunner::with_plugins(
            agent,
            RunnerPluginConfig {
                plugins,
                close_timeout: Duration::from_secs(1),
            },
        )
        .await;
        let (events, err) = drain(runner.run("user input")).await;
        if let Some(e) = err {
            // Go tolerates only the MockModel's end-of-script error ("no data").
            assert!(
                matches!(e, AgentError::Model(_)),
                "{}: agent returned error {e}",
                case.name
            );
        }
        let got = events
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_response.as_ref())
            .map(|fr| fr.response.clone())
            .last();
        assert_eq!(
            got,
            Some(obj(case.want.clone())),
            "{}: callTool() mismatch",
            case.name
        );
        assert!(
            !bad.load(SeqCst),
            "{}: a callback or tool that must not run was called",
            case.name
        );
        assert_eq!(
            on_err_called.load(SeqCst),
            !case.skip_on_error,
            "{}: on tool error callback",
            case.name
        );
        assert_eq!(
            before_called.load(SeqCst),
            !case.skip_before,
            "{}: before tool callback",
            case.name
        );
        assert_eq!(
            after_called.load(SeqCst),
            !case.skip_after,
            "{}: after tool callback",
            case.name
        );
    }
}

// ---- model callback helpers ----

type ModelOut = Result<Option<&'static str>, String>;

fn before_model(out: ModelOut) -> BeforeModelCallback {
    Arc::new(move |_ctx, req| {
        let out = out.clone();
        Box::pin(async move {
            (
                req,
                out.map(|t| t.map(model_text)).map_err(AgentError::other),
            )
        })
    })
}

fn after_model(out: ModelOut) -> AfterModelCallback {
    Arc::new(move |_ctx, resp, _err| {
        let out = out.clone();
        Box::pin(async move {
            (
                resp,
                out.map(|t| t.map(model_text)).map_err(AgentError::other),
            )
        })
    })
}

fn on_model_error(out: ModelOut) -> OnModelErrorCallback {
    Arc::new(move |_ctx, _req, _err| {
        let out = out.clone();
        Box::pin(async move { out.map(|t| t.map(model_text)).map_err(AgentError::other) })
    })
}

struct ModelCase {
    name: &'static str,
    responses: Vec<&'static str>,
    before: Vec<BeforeModelCallback>,
    after: Vec<AfterModelCallback>,
    on_error: Vec<OnModelErrorCallback>,
    want_texts: Vec<&'static str>,
    want_err: Option<&'static str>,
    skip_before: bool,
    skip_after: bool,
    skip_on_error: bool,
}

fn model_cases() -> Vec<ModelCase> {
    let base = |name| ModelCase {
        name,
        responses: vec!["hello from model"],
        before: vec![],
        after: vec![],
        on_error: vec![],
        want_texts: vec![],
        want_err: None,
        skip_before: false,
        skip_after: false,
        skip_on_error: false,
    };
    // Go cases whose callback returns BOTH a response and an error are ported with the error only
    // (a Rust Result cannot carry both); Go's outcome for them is the error as well.
    vec![
        ModelCase {
            before: vec![before_model(Ok(None))],
            skip_on_error: true,
            want_texts: vec!["hello from model"],
            ..base("before model callback doesn't modify anything")
        },
        ModelCase {
            before: vec![
                before_model(Err(format!("before_model_callback_error: {NO_COOKIE}"))),
                before_model(Err(format!("before_model_callback_error: {HIJACKED}"))),
            ],
            skip_before: true,
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("before model callback returns an error")
        },
        ModelCase {
            before: vec![
                before_model(Ok(Some("hello from before_model_callback"))),
                before_model(Ok(Some("unexpected text"))),
            ],
            skip_before: true,
            skip_on_error: true,
            skip_after: true,
            want_texts: vec!["hello from before_model_callback"],
            ..base("before model callback returns new LLMResponse")
        },
        ModelCase {
            before: vec![before_model(Err(format!(
                "before_model_callback_error: {NO_COOKIE}"
            )))],
            skip_before: true,
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("before model callback returns both new LLMResponse and error")
        },
        ModelCase {
            after: vec![after_model(Ok(None))],
            skip_on_error: true,
            want_texts: vec!["hello from model"],
            ..base("after model callback doesn't modify anything")
        },
        ModelCase {
            after: vec![
                after_model(Ok(Some("hello from after_model_callback"))),
                after_model(Ok(Some("unexpected text"))),
            ],
            skip_on_error: true,
            skip_after: true,
            want_texts: vec!["hello from after_model_callback"],
            ..base("after model callback returns new LLMResponse and are run in symmetrical order")
        },
        ModelCase {
            after: vec![
                after_model(Err(format!("error from after_model_callback: {NO_COOKIE}"))),
                after_model(Err(format!("error from after_model_callback: {HIJACKED}"))),
            ],
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("after model callback returns error and are run in symmetrical order")
        },
        ModelCase {
            after: vec![after_model(Err(format!(
                "error from after_model_callback: {NO_COOKIE}"
            )))],
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("after model callback returns both new LLMResponse and error")
        },
        ModelCase {
            on_error: vec![on_model_error(Err(format!(
                "on_model_error_callback: {NO_COOKIE}"
            )))],
            skip_on_error: true,
            want_texts: vec!["hello from model"],
            ..base("on model error callback is not called")
        },
        ModelCase {
            responses: vec![],
            on_error: vec![on_model_error(Ok(Some(
                "hello from on_model_error_callback",
            )))],
            skip_on_error: true,
            want_texts: vec!["hello from on_model_error_callback"],
            ..base("on model error callback changes message")
        },
        ModelCase {
            responses: vec![],
            on_error: vec![
                on_model_error(Err(format!(
                    "error from on_model_error_callback: {NO_COOKIE}"
                ))),
                on_model_error(Err(format!(
                    "error from on_model_error_callback: {HIJACKED}"
                ))),
            ],
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("on model error callback changes err")
        },
        ModelCase {
            responses: vec![],
            on_error: vec![on_model_error(Err(format!(
                "error from on_model_error_callback: {NO_COOKIE}"
            )))],
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("on model error callback returns both new LLMResponse and error")
        },
        ModelCase {
            before: vec![before_model(Err(format!(
                "before_model_callback_error: {NO_COOKIE}"
            )))],
            on_error: vec![on_model_error(Err(format!(
                "error from on_model_error_callback: {HIJACKED}"
            )))],
            skip_before: true,
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("on model error callback does not process before model callback error")
        },
        ModelCase {
            before: vec![before_model(Ok(Some("hello from before_model_callback")))],
            on_error: vec![on_model_error(Err(format!(
                "error from on_model_error_callback: {HIJACKED}"
            )))],
            skip_before: true,
            skip_on_error: true,
            skip_after: true,
            want_texts: vec!["hello from before_model_callback"],
            ..base("on model error callback does not process before model callback message")
        },
        ModelCase {
            responses: vec![],
            on_error: vec![on_model_error(Ok(Some(
                "hello from on_model_error_callback",
            )))],
            after: vec![after_model(Ok(Some("hello from after_model_callback")))],
            skip_on_error: true,
            skip_after: true,
            want_texts: vec!["hello from after_model_callback"],
            ..base("after error callback process on model error callback message")
        },
        ModelCase {
            responses: vec![],
            on_error: vec![on_model_error(Err(format!(
                "error from on_model_error_callback: {NO_COOKIE}"
            )))],
            after: vec![after_model(Err(format!(
                "error from after_model_callback: {HIJACKED}"
            )))],
            skip_on_error: true,
            skip_after: true,
            want_err: Some(NO_COOKIE),
            ..base("after error callback does not process on model error callback error")
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_model_callbacks() {
    for case in model_cases() {
        let n = case
            .before
            .len()
            .max(case.after.len())
            .max(case.on_error.len());
        let plugins: Vec<Plugin> = (0..n)
            .map(|i| {
                Plugin::new(gadk_agent::PluginConfig {
                    name: format!("plugin-{i}"),
                    before_model_callback: case.before.get(i).cloned(),
                    after_model_callback: case.after.get(i).cloned(),
                    on_model_error_callback: case.on_error.get(i).cloned(),
                    ..Default::default()
                })
            })
            .collect();
        let (on_err_called, before_called, after_called) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (oe, bc, ac) = (
            on_err_called.clone(),
            before_called.clone(),
            after_called.clone(),
        );
        let model = MockModel::from_contents(
            case.responses
                .iter()
                .map(|t| Content::from_text(*t, "model"))
                .collect(),
        );
        let agent = llm_agent::new(LlmAgentConfig {
            name: "hello_world_agent".into(),
            model: Some(model),
            on_model_error_callbacks: vec![Arc::new(move |_c, _r, _e| {
                oe.store(true, SeqCst);
                Box::pin(async { Ok(None) })
            })],
            before_model_callbacks: vec![Arc::new(move |_c, req| {
                bc.store(true, SeqCst);
                Box::pin(async move { (req, Ok(None)) })
            })],
            after_model_callbacks: vec![Arc::new(move |_c, r, _e| {
                ac.store(true, SeqCst);
                Box::pin(async move { (r, Ok(None)) })
            })],
            ..Default::default()
        })
        .unwrap();
        let runner = TestAgentRunner::with_plugins(
            agent,
            RunnerPluginConfig {
                plugins,
                close_timeout: Duration::from_secs(1),
            },
        )
        .await;
        let (events, err) = drain(runner.run("")).await;
        let texts: Vec<String> = events
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| c.parts.iter())
            .filter(|p| !p.text.is_empty())
            .map(|p| p.text.clone())
            .collect();
        match (case.want_err, &err) {
            (Some(want), Some(e)) => assert!(
                err_chain(e).contains(want),
                "{}: error {} does not wrap {want}",
                case.name,
                err_chain(e)
            ),
            (None, None) => {}
            (want, got) => panic!("{}: got error {got:?}, want error {want:?}", case.name),
        }
        assert_eq!(texts, case.want_texts, "{}: unexpected texts", case.name);
        assert_eq!(
            on_err_called.load(SeqCst),
            !case.skip_on_error,
            "{}: on model error callback",
            case.name
        );
        assert_eq!(
            before_called.load(SeqCst),
            !case.skip_before,
            "{}: before model callback",
            case.name
        );
        assert_eq!(
            after_called.load(SeqCst),
            !case.skip_after,
            "{}: after model callback",
            case.name
        );
    }
}

/// R1: a response edited in place by the first after-model plugin callback (Go mutates
/// `*model.LLMResponse` and returns `(nil, nil)`) reaches the second plugin, the agent's
/// after-model callback, and the event the flow yields.
#[tokio::test]
async fn after_model_edit_reaches_later_callbacks_and_the_flow() {
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let first_text = |r: &LlmResponse| {
        r.content
            .as_ref()
            .map(|c| c.parts[0].text.clone())
            .unwrap_or_default()
    };
    let editor: AfterModelCallback = Arc::new(|_ctx, mut resp: LlmResponse, _err| {
        Box::pin(async move {
            resp.content = Some(Content::from_text("edited by plugin-0", "model"));
            (resp, Ok(None))
        })
    });
    let observer = |who: &'static str| -> AfterModelCallback {
        let seen = seen.clone();
        Arc::new(move |_ctx, resp: LlmResponse, _err| {
            seen.lock().push(format!("{who}: {}", first_text(&resp)));
            Box::pin(async move { (resp, Ok(None)) })
        })
    };
    let plugins = vec![
        Plugin::new(gadk_agent::PluginConfig {
            name: "plugin-0".into(),
            after_model_callback: Some(editor),
            ..Default::default()
        }),
        Plugin::new(gadk_agent::PluginConfig {
            name: "plugin-1".into(),
            after_model_callback: Some(observer("plugin-1")),
            ..Default::default()
        }),
    ];
    let agent = llm_agent::new(LlmAgentConfig {
        name: "hello_world_agent".into(),
        model: Some(MockModel::from_contents(vec![Content::from_text(
            "from the model",
            "model",
        )])),
        after_model_callbacks: vec![observer("agent")],
        ..Default::default()
    })
    .unwrap();
    let runner = TestAgentRunner::with_plugins(
        agent,
        RunnerPluginConfig {
            plugins,
            close_timeout: Duration::from_secs(1),
        },
    )
    .await;
    let (events, err) = drain(runner.run("")).await;
    assert!(err.is_none(), "{err:?}");
    let texts: Vec<String> = events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.clone())
        .collect();
    assert_eq!(texts, ["edited by plugin-0"]);
    assert_eq!(
        *seen.lock(),
        ["plugin-1: edited by plugin-0", "agent: edited by plugin-0"]
    );
}
