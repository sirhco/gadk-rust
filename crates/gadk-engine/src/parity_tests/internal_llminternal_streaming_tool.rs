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

//! Parity port of Go internal/llminternal/streaming_tool_test.go.

use std::{
    any::Any,
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use gadk_agent::{
    AgentConfig, AgentError, Context, InvocationContext, LiveRequest, LiveSession, Tool, agent,
};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::LlmResponse;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    llm::{
        flow::Flow,
        functions::{handle_function_calls, handle_function_calls_live},
        live::session::LiveSessionImpl,
    },
    tool::{FunctionToolConfig, StreamingFunctionTool},
};

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    count: i64,
}

/// A model event carrying the given function calls `(id, name, args)`.
fn call_event(calls: &[(&str, &str, Value)]) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: calls
                    .iter()
                    .map(|(id, name, args)| Part {
                        function_call: Some(FunctionCall {
                            id: (*id).into(),
                            name: (*name).into(),
                            args: args.as_object().unwrap().clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect(),
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn inv_ctx() -> InvocationContext {
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

/// The only function response of `ev`, as its `response` map.
fn single_response(ev: &Event) -> serde_json::Map<String, Value> {
    let parts = &ev.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 1, "expected 1 part");
    parts[0]
        .function_response
        .as_ref()
        .expect("FunctionResponse part")
        .response
        .clone()
}

/// Go mockLiveSession: records every request it is sent.
struct MockLiveSession {
    tx: tokio::sync::mpsc::UnboundedSender<LiveRequest>,
}

#[async_trait]
impl LiveSession for MockLiveSession {
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        let _ = self.tx.send(req);
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_handle_function_calls_streaming() {
    let stream_tool: Arc<dyn Tool> = StreamingFunctionTool::new(
        FunctionToolConfig::new("test_stream", "streams chunks"),
        |_ctx: Context, a: Args| {
            futures::stream::iter((0..a.count).map(|i| Ok(format!("chunk {i}"))))
        },
    )
    .unwrap();
    let tools = HashMap::from([("test_stream".to_owned(), stream_tool.clone())]);
    let call_ev = call_event(&[("call_1", "test_stream", json!({"count": 3}))]);

    // Subtest "Live Mode (Streaming)".
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mock: Arc<dyn LiveSession> = Arc::new(MockLiveSession { tx });
        let flow = Flow {
            tools: parking_lot::RwLock::new(vec![stream_tool.clone()]),
            ..Default::default()
        };
        let merged = handle_function_calls_live(&inv_ctx(), &flow, &tools, &call_ev, mock)
            .await
            .unwrap()
            .expect("non-None merged event");
        // The immediate response says the call is pending.
        assert_eq!(
            single_response(&merged).get("status"),
            Some(&json!(
                "The function is running asynchronously and the results are pending."
            ))
        );
        // Wait for the background stream (Go wg.Wait for 3 sends).
        let mut received = Vec::new();
        while received.len() < 3 {
            let req = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for a streamed chunk")
                .expect("session dropped");
            if let Some(c) = &req.content
                && let Some(p) = c.parts.first()
            {
                assert_eq!(c.role, "user");
                received.push(p.text.clone());
            }
        }
        assert_eq!(
            received,
            vec![
                "Function test_stream returned: chunk 0",
                "Function test_stream returned: chunk 1",
                "Function test_stream returned: chunk 2",
            ]
        );
    }

    // Subtest "Non-Live Mode (Aggregation)".
    {
        let flow = Flow {
            tools: parking_lot::RwLock::new(vec![stream_tool.clone()]),
            ..Default::default()
        };
        let merged = handle_function_calls(&inv_ctx(), &flow, &tools, &call_ev, None)
            .await
            .unwrap()
            .expect("non-None merged event");
        assert_eq!(
            single_response(&merged).get("result"),
            Some(&json!("chunk 0chunk 1chunk 2"))
        );
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // Go's handler ignores DelayMS too; the field only shapes the schema.
struct DelayArgs {
    delay_ms: i64,
}

/// Counts one cancellation when the handler's stream ends, whether it saw its context cancelled
/// (Go `case <-ctx.Done()`) or was dropped by its consumer (Go `!yield(...)`).
struct CountOnDrop(Arc<AtomicUsize>);

impl Drop for CountOnDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_handle_function_calls_live_control_plane() {
    let cancel_count = Arc::new(AtomicUsize::new(0));
    let counter = cancel_count.clone();
    let stream_tool: Arc<dyn Tool> = StreamingFunctionTool::new(
        FunctionToolConfig::new("count_forever", "counts indefinitely"),
        move |ctx: Context, _a: DelayArgs| {
            let guard = CountOnDrop(counter.clone());
            futures::stream::unfold((ctx, guard, 0u64), |(ctx, guard, i)| async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                if ctx.cancellation_token().is_cancelled() {
                    return None;
                }
                Some((Ok(format!("number {i}")), (ctx, guard, i + 1)))
            })
        },
    )
    .unwrap();
    let tools = HashMap::from([("count_forever".to_owned(), stream_tool.clone())]);
    let ctx = inv_ctx();
    let flow = Flow {
        tools: parking_lot::RwLock::new(vec![stream_tool]),
        ..Default::default()
    };

    let live_sess = LiveSessionImpl::new();
    // Go: `go func() { for range liveSess.inputCh {} }()` — drain what the tools send.
    let drain = live_sess.clone();
    let drainer = tokio::spawn(async move {
        let never = CancellationToken::new();
        while drain.next_input(&never).await.is_some() {}
    });
    let live: Arc<dyn LiveSession> = live_sess.clone();

    let start = call_event(&[
        ("call_forever_1", "count_forever", json!({"delay_ms": 1})),
        ("call_forever_2", "count_forever", json!({"delay_ms": 1})),
    ]);
    handle_function_calls_live(&ctx, &flow, &tools, &start, live.clone())
        .await
        .unwrap();
    assert_eq!(
        live_sess.active_streaming_tool_count("count_forever"),
        2,
        "expected 2 active tasks"
    );

    let stop = call_event(&[(
        "call_stop_1",
        "stop_streaming",
        json!({"function_name": "count_forever"}),
    )]);
    let merged = handle_function_calls_live(&ctx, &flow, &tools, &stop, live.clone())
        .await
        .unwrap()
        .expect("non-None mergedStopEvent");
    assert_eq!(
        single_response(&merged).get("status"),
        Some(&json!(
            "Successfully stopped all running instances of count_forever"
        ))
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        cancel_count.load(Ordering::SeqCst),
        2,
        "expected exactly 2 streams to be cancelled"
    );
    assert_eq!(
        live_sess.active_streaming_tool_count("count_forever"),
        0,
        "expected registry to be empty after cancellation"
    );

    live.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), drainer)
        .await
        .expect("drainer ends once the session is closed")
        .unwrap();
}
