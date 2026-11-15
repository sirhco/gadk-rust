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

//! Port of ADK-Go `internal/llminternal/handle_function_calls_async_test.go`.

mod llminternal_support;

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use gadk_agent::{Context, RunnableTool, StreamingMode, Tool, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Content, FunctionCall, FunctionDeclaration, Part, ROLE_MODEL, ROLE_USER, Schema, Type,
};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use llminternal_support::*;
use serde_json::{Map, Value, json};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SleepArgs {
    duration_ms: u64,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct SleepResult {
    success: bool,
}

fn fc_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: args.as_object().cloned().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn model_text(t: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_parts(vec![Part::from_text(t)], ROLE_MODEL)),
        ..Default::default()
    }
}

/// Port of Go `mockModel`: call 1 returns three parallel `sleep` calls, later calls "I am done.".
#[derive(Default)]
struct ParallelSleepModel {
    calls: AtomicUsize,
}
impl Llm for ParallelSleepModel {
    fn name(&self) -> &str {
        "mock-model"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let resp = if call > 1 {
            model_text("I am done.")
        } else {
            LlmResponse {
                content: Some(Content::from_parts(
                    (1..=3)
                        .map(|i| {
                            fc_part(&format!("call_{i}"), "sleep", json!({"duration_ms": 500}))
                        })
                        .collect(),
                    ROLE_MODEL,
                )),
                ..Default::default()
            }
        };
        stream::iter(vec![Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// parity: adapted: wall-clock timing replaced by tokio's paused clock (tool sleeps with tokio::time::sleep); parallel execution shows as exactly one 500ms advance instead of three
#[tokio::test(start_paused = true)]
async fn test_handle_function_calls_async() {
    let sleep_tool = FunctionTool::new(
        FunctionToolConfig::new("sleep", "sleeps for a duration"),
        |_ctx: Context, input: SleepArgs| async move {
            tokio::time::sleep(Duration::from_millis(input.duration_ms)).await;
            Ok::<_, ToolError>(SleepResult { success: true })
        },
    )
    .unwrap();
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "tester".into(),
        description: "Tester agent".into(),
        instruction: "You are a tester agent.".into(),
        model: Some(Arc::new(ParallelSleepModel::default())),
        tools: vec![sleep_tool],
        ..Default::default()
    })
    .unwrap();
    let r = TestRunner::new(agent, "testApp", "testUser", "testSession").await;

    let start = tokio::time::Instant::now();
    let events = r
        .run(
            Content::from_parts(vec![Part::from_text("Test sleep")], ROLE_USER),
            StreamingMode::Sse,
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(events.len(), 3, "Expected 3 events");
    assert_eq!(
        events[0].response.content.as_ref().unwrap().parts.len(),
        3,
        "first event: 3 function calls"
    );
    assert_eq!(
        events[1].response.content.as_ref().unwrap().parts.len(),
        3,
        "second event: 3 function responses"
    );
    assert_eq!(
        events[2].response.content.as_ref().unwrap().parts.len(),
        1,
        "third event: 1 text part"
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "Elapsed time {elapsed:?} is less than expected 500ms"
    );
    assert!(
        elapsed <= Duration::from_millis(1000),
        "Elapsed time {elapsed:?} is greater than expected 1000ms for async execution"
    );
}

/// Port of Go `deferringTool`. Go `result == nil` is an empty Map (mapping decision 2).
struct DeferringTool {
    result: Map<String, Value>,
    defers: bool,
    long_running: bool,
    run_count: AtomicUsize,
}
impl Tool for DeferringTool {
    fn name(&self) -> &str {
        "deferring"
    }
    fn description(&self) -> &str {
        "test deferring tool"
    }
    fn is_long_running(&self) -> bool {
        self.long_running
    }
    fn defers_response(&self) -> bool {
        self.defers
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: "deferring".into(),
            description: "test deferring tool".into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                ..Default::default()
            }),
            ..Default::default()
        })
    }
    // Go ProcessRequest = toolutils.PackTool: the Rust flow packs every declared tool itself (pack_tool), so the default is kept.
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[async_trait]
impl RunnableTool for DeferringTool {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        self.run_count.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

/// Port of Go `singleCallMockModel`: call 1 returns one call `{id: fc-x, name: deferring, args: {}}`, later calls "done".
#[derive(Default)]
struct SingleCallModel {
    calls: AtomicUsize,
}
impl Llm for SingleCallModel {
    fn name(&self) -> &str {
        "single-call-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let resp = if call > 1 {
            model_text("done")
        } else {
            LlmResponse {
                content: Some(Content::from_parts(
                    vec![fc_part("fc-x", "deferring", json!({}))],
                    ROLE_MODEL,
                )),
                ..Default::default()
            }
        };
        stream::iter(vec![Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// parity: adapted: a Rust tool's "nil" result is the empty map (Rust has no nil map), as in the rows below.
#[tokio::test]
async fn test_handle_function_calls_fr_skip_gates() {
    // (name, defers, long_running, tool_result, want_event_count, want_fr_event_index, want_response)
    type GateCase = (
        &'static str,
        bool,
        bool,
        Map<String, Value>,
        usize,
        Option<usize>,
        Value,
    );
    let cases: Vec<GateCase> = vec![
        (
            "deferred + nil: gate fires, no FR event",
            true,
            false,
            Map::new(),
            2,
            None,
            Value::Null,
        ),
        (
            "long-running + nil: gate fires, no FR; node parks after FC only",
            false,
            true,
            Map::new(),
            1,
            None,
            Value::Null,
        ),
        (
            "deferred + long-running + nil: gate fires, no FR; node parks after FC only",
            true,
            true,
            Map::new(),
            1,
            None,
            Value::Null,
        ),
        (
            "plain + nil: no gate, FR wraps nil as {result: nil}",
            false,
            false,
            Map::new(),
            3,
            Some(1),
            json!({"result": null}),
        ),
        (
            "deferred + non-nil: FR event still emitted (gate is nil-only)",
            true,
            false,
            json!({"answer": "ok"}).as_object().cloned().unwrap(),
            3,
            Some(1),
            json!({"answer": "ok"}),
        ),
        (
            "long-running + non-nil (pending dict): FR event still emitted",
            false,
            true,
            json!({"status": "pending"}).as_object().cloned().unwrap(),
            3,
            Some(1),
            json!({"status": "pending"}),
        ),
    ];
    for (name, defers, long_running, result, want_count, want_fr, want_response) in cases {
        let tool = Arc::new(DeferringTool {
            result,
            defers,
            long_running,
            run_count: AtomicUsize::new(0),
        });
        let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
            name: "tester".into(),
            description: "tester".into(),
            model: Some(Arc::new(SingleCallModel::default())),
            tools: vec![tool.clone() as Arc<dyn Tool>],
            ..Default::default()
        })
        .unwrap();
        let r = TestRunner::new(agent, "app", "u", "s").await;
        let events = r
            .run(
                Content::from_parts(vec![Part::from_text("go")], ROLE_USER),
                StreamingMode::Sse,
            )
            .await
            .unwrap();
        assert_eq!(
            tool.run_count.load(Ordering::SeqCst),
            1,
            "{name}: tool.run calls"
        );
        assert_eq!(events.len(), want_count, "{name}: events {events:?}");
        match want_fr {
            Some(i) => {
                let fr = events[i]
                    .response
                    .content
                    .as_ref()
                    .and_then(|c| c.parts.first())
                    .and_then(|p| p.function_response.as_ref())
                    .unwrap_or_else(|| panic!("{name}: event[{i}] should be a FunctionResponse"));
                assert_eq!(
                    Value::Object(fr.response.clone()),
                    want_response,
                    "{name}: FunctionResponse.Response"
                );
            }
            None => assert!(
                events.iter().all(|e| e
                    .response
                    .content
                    .as_ref()
                    .is_none_or(|c| c.parts.iter().all(|p| p.function_response.is_none()))),
                "{name}: an event unexpectedly carries a FunctionResponse"
            ),
        }
    }
}

/// Port of Go `noArgsCallModel`: calls `tool_name` once with `args` (none by default), then
/// answers "done".
struct NoArgsCallModel {
    tool_name: &'static str,
    args: Map<String, Value>,
    calls: AtomicUsize,
}

impl NoArgsCallModel {
    fn new(tool_name: &'static str) -> Self {
        Self {
            tool_name,
            args: Map::new(),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Llm for NoArgsCallModel {
    fn name(&self) -> &str {
        "gemini-2.5-flash"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let part = if call > 1 {
            Part::from_text("done")
        } else {
            Part {
                function_call: Some(FunctionCall {
                    id: "fc-1".into(),
                    name: self.tool_name.into(),
                    args: self.args.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        };
        stream::iter(vec![Ok(LlmResponse {
            content: Some(Content::from_parts(vec![part], ROLE_MODEL)),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go `argsRecordingTool`: records the args it is run with and returns no result.
#[derive(Default)]
struct ArgsRecordingTool {
    got_args: std::sync::Mutex<Option<Value>>,
}

impl Tool for ArgsRecordingTool {
    fn name(&self) -> &str {
        "record"
    }
    fn description(&self) -> &str {
        "records its args"
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            description: self.description().into(),
            ..Default::default()
        })
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for ArgsRecordingTool {
    async fn run(&self, _ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        *self.got_args.lock().unwrap() = Some(args);
        Ok(Map::new())
    }
}

async fn run_go(agent: Arc<dyn gadk_agent::Agent>) -> Vec<gadk_session::Event> {
    let r = TestRunner::new(agent, "app", "u", "s").await;
    r.run(
        Content::from_parts(vec![Part::from_text("go")], ROLE_USER),
        StreamingMode::None,
    )
    .await
    .unwrap()
}

/// A function call without args reaches the tool and its callbacks as an empty map, so
/// neither has to tell a nil map from an empty one.
// parity: adapted: a gadk FunctionCall's args are a map (Rust has no nil map), so "no args" is the empty map.
#[tokio::test]
async fn test_handle_function_calls_no_args_reach_tool_as_empty_map() {
    let rec = Arc::new(ArgsRecordingTool::default());
    let before_args = Arc::new(std::sync::Mutex::new(None::<Map<String, Value>>));
    let after_args = Arc::new(std::sync::Mutex::new(None::<Map<String, Value>>));
    let (b, a) = (before_args.clone(), after_args.clone());
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "tester".into(),
        model: Some(Arc::new(NoArgsCallModel::new("record"))),
        tools: vec![rec.clone() as Arc<dyn Tool>],
        before_tool_callbacks: vec![Arc::new(move |_, _, args| {
            *b.lock().unwrap() = Some(args);
            Box::pin(async { Ok(None) })
        })],
        after_tool_callbacks: vec![Arc::new(move |_, _, args, _, _| {
            *a.lock().unwrap() = Some(args);
            Box::pin(async { Ok(None) })
        })],
        ..Default::default()
    })
    .unwrap();
    run_go(agent).await;
    assert_eq!(
        rec.got_args.lock().unwrap().clone(),
        Some(json!({})),
        "tool args"
    );
    assert_eq!(
        before_args.lock().unwrap().clone(),
        Some(Map::new()),
        "before-tool callback args"
    );
    assert_eq!(
        after_args.lock().unwrap().clone(),
        Some(Map::new()),
        "after-tool callback args"
    );
}

/// Port of Go `argsRecordingStreamTool`.
#[derive(Default)]
struct ArgsRecordingStreamTool {
    got_args: std::sync::Mutex<Option<Value>>,
}

impl Tool for ArgsRecordingStreamTool {
    fn name(&self) -> &str {
        "stream_record"
    }
    fn description(&self) -> &str {
        "records its args"
    }
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            description: self.description().into(),
            ..Default::default()
        })
    }
    fn as_streaming(&self) -> Option<&dyn gadk_agent::StreamingTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl gadk_agent::StreamingTool for ArgsRecordingStreamTool {
    fn run_stream(
        &self,
        _ctx: Context,
        args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>> {
        *self.got_args.lock().unwrap() = Some(args);
        stream::iter(vec![Ok("ok".to_owned())]).boxed()
    }
}

// parity: adapted: a gadk FunctionCall's args are a map (Rust has no nil map), so "no args" is the empty map.
#[tokio::test]
async fn test_handle_function_calls_no_args_reach_streaming_tool_as_empty_map() {
    let st = Arc::new(ArgsRecordingStreamTool::default());
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "tester".into(),
        model: Some(Arc::new(NoArgsCallModel::new("stream_record"))),
        tools: vec![st.clone() as Arc<dyn Tool>],
        ..Default::default()
    })
    .unwrap();
    run_go(agent).await;
    assert_eq!(
        st.got_args.lock().unwrap().clone(),
        Some(json!({})),
        "streaming tool args"
    );
}

// parity: adapted: a gadk FunctionCall's args are a map (Rust has no nil map), so "no args" is the empty map.
#[tokio::test]
async fn test_handle_function_calls_no_args_reach_on_tool_error_for_unknown_tool() {
    let got_args = Arc::new(std::sync::Mutex::new(None::<Map<String, Value>>));
    let g = got_args.clone();
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "tester".into(),
        model: Some(Arc::new(NoArgsCallModel::new("missing"))),
        tools: vec![Arc::new(ArgsRecordingTool::default()) as Arc<dyn Tool>],
        on_tool_error_callbacks: vec![Arc::new(move |_, _, args, _| {
            *g.lock().unwrap() = Some(args);
            Box::pin(async { Ok(Some(json!({"handled": true}).as_object().cloned().unwrap())) })
        })],
        ..Default::default()
    })
    .unwrap();
    run_go(agent).await;
    let got = got_args
        .lock()
        .unwrap()
        .clone()
        .expect("on-tool-error callback not called");
    assert_eq!(got, Map::new(), "on-tool-error args");
}

/// A set_model_response call without args gives the same structured output as a call with
/// empty args, the empty object, rather than the {"result": null} sent for a tool that
/// returns nothing.
// parity: adapted: Go's "nil args" and "empty args" rows are both the empty map in Rust, so one row stands for both.
#[tokio::test]
async fn test_handle_function_calls_set_model_response_without_args() {
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "tester".into(),
        model: Some(Arc::new(NoArgsCallModel::new("set_model_response"))),
        tools: vec![Arc::new(ArgsRecordingTool::default()) as Arc<dyn Tool>],
        output_key: "out".into(),
        output_schema: Some(Schema {
            r#type: Some(Type::Object),
            properties: [(
                "note".to_owned(),
                Schema {
                    r#type: Some(Type::String),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let mut last_text = String::new();
    let mut got_out = None;
    for ev in run_go(agent).await {
        if let Some(t) = ev
            .response
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .map(|p| p.text.clone())
            .filter(|t| !t.is_empty())
        {
            last_text = t;
        }
        if let Some(v) = ev.actions.state_delta.as_ref().and_then(|d| d.get("out")) {
            got_out = Some(v.clone());
        }
    }
    assert_eq!(last_text, "{}", "structured output");
    assert_eq!(got_out, Some(json!("{}")), "state[out]");
}
