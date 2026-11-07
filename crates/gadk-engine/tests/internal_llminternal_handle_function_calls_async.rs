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

#[tokio::test]
async fn test_handle_function_calls_fr_skip_gates() {
    // (name, defers, long_running, tool_result, want_event_count, want_fr_event_index)
    type GateCase = (
        &'static str,
        bool,
        bool,
        Map<String, Value>,
        usize,
        Option<usize>,
    );
    let cases: Vec<GateCase> = vec![
        (
            "deferred + nil: gate fires, no FR event",
            true,
            false,
            Map::new(),
            2,
            None,
        ),
        (
            "long-running + nil: gate fires, no FR; node parks after FC only",
            false,
            true,
            Map::new(),
            1,
            None,
        ),
        (
            "deferred + long-running + nil: gate fires, no FR; node parks after FC only",
            true,
            true,
            Map::new(),
            1,
            None,
        ),
        (
            "plain + nil: no gate, FR(nil) still emitted (no-op tool legacy)",
            false,
            false,
            Map::new(),
            3,
            Some(1),
        ),
        (
            "deferred + non-nil: FR event still emitted (gate is nil-only)",
            true,
            false,
            json!({"answer": "ok"}).as_object().cloned().unwrap(),
            3,
            Some(1),
        ),
        (
            "long-running + non-nil (pending dict): FR event still emitted",
            false,
            true,
            json!({"status": "pending"}).as_object().cloned().unwrap(),
            3,
            Some(1),
        ),
    ];
    for (name, defers, long_running, result, want_count, want_fr) in cases {
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
            Some(i) => assert!(
                events[i].response.content.as_ref().is_some_and(|c| c
                    .parts
                    .first()
                    .is_some_and(|p| p.function_response.is_some())),
                "{name}: event[{i}] should be a FunctionResponse"
            ),
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
