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

#[tokio::test]
async fn test_call_tool() {
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
    assert_eq!(cases.len(), 14);
    for tc in cases {
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
            obj(json!({"key": "value"})),
        )
        .await;
        assert_eq!(got.map(Value::Object), Some(tc.want), "{}", tc.name);
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
