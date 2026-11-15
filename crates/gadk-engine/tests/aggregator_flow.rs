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

//! Re-port of the three agent-flow tests of ADK-Go `internal/llminternal/stream_aggregator_test.go`
//! (TestProgressiveSSEStreamingFunctionCalls, TestProgressiveSSEPreservesPartOrdering,
//! TestPartialFunctionCallsNotExecutedInNoneStreamingMode) through LlmAgent + TestAgentRunner.
//! The other aggregator tests live in crates/gadk-model/tests/aggregator.rs (1a). Also the
//! event-id tests of ADK-Go 0990f9e (TestProgressiveSSEPartialsShareEventID,
//! TestProgressiveSSETextStreamSharesEventID, TestCompleteResponsesOfOneModelCallGetDistinctEventIDs,
//! TestRecoveredModelErrorGetsItsOwnEventID).

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use gadk_agent::{
    Agent, AgentError, Context, OnModelErrorCallback, RunConfig, StreamingMode, ToolError,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::testutil::{TestAgentRunner, collect_events};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Candidate, Content, FinishReason, FunctionCall, GenerateContentResponse, Part, ROLE_MODEL,
    ROLE_USER,
};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError, StreamingResponseAggregator};
use gadk_session::Event;
use serde_json::json;

/// Port of `testutil.CollectEvents(runner.RunContentWithConfig(t, "session-1", msg, cfg))`.
/// `TestAgentRunner` owns one session ("test_session"); Go's "session-1" ID is not asserted on.
async fn run_with_mode(
    agent: Arc<dyn Agent>,
    msg: Content,
    mode: StreamingMode,
) -> Result<Vec<Event>, AgentError> {
    let runner = TestAgentRunner::new(agent).await;
    collect_events(runner.run_with_config(
        msg,
        RunConfig {
            streaming_mode: mode,
            ..Default::default()
        },
    ))
    .await
}

fn text_part(text: &str, thought: bool) -> Part {
    Part {
        text: text.into(),
        thought,
        ..Default::default()
    }
}

fn weather_call(location: &str) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: "get_weather".into(),
            args: json!({"location": location}).as_object().cloned().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn chunk(parts: Vec<Part>, finish: Option<FinishReason>) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_parts(parts, ROLE_MODEL)),
        finish_reason: finish,
        ..Default::default()
    }
}

/// Port of Go `streamingMockModel`: call 1 pipes its chunks through the aggregator; later calls
/// return "Task completed.".
struct StreamingMockModel {
    stream_chunks: Vec<LlmResponse>,
    call_count: AtomicUsize,
}

/// Port of `streamingMockModel.llmResponseToGenerateContentResponse`.
fn to_generate_content_response(resp: &LlmResponse) -> GenerateContentResponse {
    let mut candidates = Vec::new();
    if let Some(content) = &resp.content {
        candidates.push(Candidate {
            content: Some(content.clone()),
            finish_reason: resp.finish_reason.clone(),
            ..Default::default()
        });
    }
    GenerateContentResponse {
        candidates,
        usage_metadata: resp.usage_metadata.clone(),
        ..Default::default()
    }
}

impl Llm for StreamingMockModel {
    fn name(&self) -> &str {
        "streaming-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        if call > 1 {
            return stream::iter(vec![Ok(chunk(
                vec![text_part("Task completed.", false)],
                None,
            ))])
            .boxed();
        }
        let mut agg = StreamingResponseAggregator::new();
        let mut out = Vec::new();
        for c in &self.stream_chunks {
            for item in agg.process_response(to_generate_content_response(c)) {
                let failed = item.is_err();
                out.push(item);
                if failed {
                    return stream::iter(out).boxed();
                }
            }
        }
        if let Some(final_resp) = agg.close() {
            out.push(Ok(final_resp));
        }
        stream::iter(out).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GetWeatherArgs {
    location: String,
}
#[derive(serde::Serialize, schemars::JsonSchema)]
struct GetWeatherResult {
    temperature: i64,
    condition: String,
    location: String,
}

fn weather_agent(name: &str, chunks: Vec<LlmResponse>) -> Arc<dyn Agent> {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("get_weather", "get weather for location"),
        |_ctx: Context, args: GetWeatherArgs| async move {
            Ok::<_, ToolError>(GetWeatherResult {
                temperature: 22,
                condition: "sunny".into(),
                location: args.location,
            })
        },
    )
    .unwrap();
    gadk_engine::llm::agent::new(LlmAgentConfig {
        name: name.into(),
        model: Some(Arc::new(StreamingMockModel {
            stream_chunks: chunks,
            call_count: AtomicUsize::new(0),
        })),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap()
}

fn part(ev: &Event, i: usize) -> &Part {
    &ev.response.content.as_ref().unwrap().parts[i]
}

#[tokio::test]
async fn test_progressive_sse_streaming_function_calls() {
    let agent = weather_agent(
        "weather_agent",
        vec![
            chunk(vec![text_part("Checking weather...", false)], None),
            chunk(vec![weather_call("Tokyo")], None),
            chunk(vec![weather_call("New York")], Some(FinishReason::Stop)),
        ],
    );
    let events = run_with_mode(
        agent,
        Content::from_text("What is the weather?", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    assert_eq!(events.len(), 6, "expected 6 events");
    assert!(
        events[0].response.partial && part(&events[0], 0).text == "Checking weather...",
        "expected partial event 0 with text"
    );
    let fc1 = part(&events[1], 0).function_call.as_ref().unwrap();
    assert!(
        events[1].response.partial
            && fc1.name == "get_weather"
            && fc1.args["location"] == json!("Tokyo"),
        "expected partial event 1 with function call 1"
    );
    let fc2 = part(&events[2], 0).function_call.as_ref().unwrap();
    assert!(
        events[2].response.partial
            && fc2.name == "get_weather"
            && fc2.args["location"] == json!("New York"),
        "expected partial event 2 with function call 2"
    );
    assert!(
        !events[3].response.partial
            && part(&events[3], 0).text == "Checking weather..."
            && part(&events[3], 1).function_call.as_ref().unwrap().name == "get_weather"
            && part(&events[3], 2).function_call.as_ref().unwrap().name == "get_weather",
        "expected final aggregated event 3 with FCs"
    );
    let fr1 = part(&events[4], 0).function_response.as_ref().unwrap();
    let fr2 = part(&events[4], 1).function_response.as_ref().unwrap();
    assert!(
        !events[4].response.partial && fr1.name == "get_weather" && fr2.name == "get_weather",
        "expected function response event 4"
    );
    let locs = (
        fr1.response.get("location").cloned(),
        fr2.response.get("location").cloned(),
    );
    assert!(
        locs == (Some(json!("Tokyo")), Some(json!("New York")))
            || locs == (Some(json!("New York")), Some(json!("Tokyo"))),
        "expected function responses to have Tokyo and New York, got {locs:?}"
    );
    assert!(
        !events[5].response.partial && part(&events[5], 0).text == "Task completed.",
        "expected task completed event 5"
    );
}

#[tokio::test]
async fn test_progressive_sse_preserves_part_ordering() {
    let agent = weather_agent(
        "ordering_test_agent",
        vec![
            chunk(vec![text_part("Initial thought part 1. ", true)], None),
            chunk(vec![text_part("Initial thought part 2.", true)], None),
            chunk(vec![text_part("Let me check Tokyo. ", false)], None),
            chunk(vec![text_part("And New York too.", false)], None),
            chunk(vec![weather_call("Tokyo")], None),
            chunk(
                vec![text_part("Now processing second thought part 1. ", true)],
                None,
            ),
            chunk(vec![text_part("Second thought part 2.", true)], None),
            chunk(vec![weather_call("New York")], Some(FinishReason::Stop)),
        ],
    );
    let events = run_with_mode(
        agent,
        Content::from_text("What is the weather?", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    let aggregated = events
        .iter()
        .find(|e| {
            !e.response.partial
                && e.author == "ordering_test_agent"
                && e.response
                    .content
                    .as_ref()
                    .is_some_and(|c| c.parts.len() > 2)
        })
        .expect("Should find an aggregated model event");
    let parts = &aggregated.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 5, "Expected 5 parts");
    assert!(
        parts[0].thought && parts[0].text == "Initial thought part 1. Initial thought part 2.",
        "part 0 mismatch: {:?}",
        parts[0]
    );
    assert!(
        !parts[1].thought && parts[1].text == "Let me check Tokyo. And New York too.",
        "part 1 mismatch: {:?}",
        parts[1]
    );
    let fc2 = parts[2].function_call.as_ref().unwrap();
    assert!(
        fc2.name == "get_weather" && fc2.args["location"] == json!("Tokyo"),
        "part 2 mismatch: {fc2:?}"
    );
    assert!(
        parts[3].thought
            && parts[3].text == "Now processing second thought part 1. Second thought part 2.",
        "part 3 mismatch: {:?}",
        parts[3]
    );
    let fc4 = parts[4].function_call.as_ref().unwrap();
    assert!(
        fc4.name == "get_weather" && fc4.args["location"] == json!("New York"),
        "part 4 mismatch: {fc4:?}"
    );
}

/// Port of Go `partialFunctionCallMockModel`.
struct PartialFunctionCallMockModel;
impl Llm for PartialFunctionCallMockModel {
    fn name(&self) -> &str {
        "partial-fc-mock"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let has_function_response = req
            .contents
            .iter()
            .flat_map(|c| c.parts.iter())
            .any(|p| p.function_response.is_some());
        if has_function_response {
            return stream::iter(vec![Ok(chunk(
                vec![text_part("Function executed once.", false)],
                None,
            ))])
            .boxed();
        }
        let track = |call_id: &str, partial: bool, finish: Option<FinishReason>| {
            Ok(LlmResponse {
                content: Some(Content::from_parts(
                    vec![Part {
                        function_call: Some(FunctionCall {
                            name: "track_execution".into(),
                            args: json!({"call_id": call_id}).as_object().cloned().unwrap(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    ROLE_MODEL,
                )),
                partial,
                finish_reason: finish,
                ..Default::default()
            })
        };
        stream::iter(vec![
            track("partial_1", true, None),
            track("partial_2", true, None),
            track("final", false, Some(FinishReason::Stop)),
        ])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TrackExecutionArgs {
    call_id: String,
}

#[tokio::test]
async fn test_partial_function_calls_not_executed_in_none_streaming_mode() {
    let execution_log = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = execution_log.clone();
    let track_tool = FunctionTool::new(
        FunctionToolConfig::new("track_execution", "A tool that logs execution"),
        move |_ctx: Context, args: TrackExecutionArgs| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(args.call_id.clone());
                Ok::<_, ToolError>(format!("Executed: {}", args.call_id))
            }
        },
    )
    .unwrap();
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "partial_fc_test_agent".into(),
        model: Some(Arc::new(PartialFunctionCallMockModel)),
        tools: vec![track_tool],
        ..Default::default()
    })
    .unwrap();
    // Mode None, to ensure partial handling acts the same.
    let events = run_with_mode(
        agent,
        Content::from_text("Test partial FC handling", ROLE_USER),
        StreamingMode::None,
    )
    .await
    .unwrap();

    let log = execution_log.lock().unwrap().clone();
    assert_eq!(log.len(), 1, "Expected 1 execution, got {log:?}");
    assert_eq!(log[0], "final", "Expected 'final' execution");
    assert_eq!(
        events.iter().filter(|e| e.response.partial).count(),
        2,
        "Expected 2 partial events"
    );
    let fr_events = events
        .iter()
        .filter(|e| {
            e.response
                .content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()))
        })
        .count();
    assert_eq!(fr_events, 1, "Expected 1 function response event");
}

#[tokio::test]
#[ignore = "8a bug: the LLM flow gives each event of one SSE model call a fresh id; Go 3188db98 keeps one id across a call's partial events and the aggregated response that completes them"]
async fn test_progressive_sse_partials_share_event_id() {
    let agent = weather_agent(
        "weather_agent",
        vec![
            chunk(vec![text_part("Checking weather...", false)], None),
            chunk(vec![weather_call("Tokyo")], None),
            chunk(vec![weather_call("New York")], Some(FinishReason::Stop)),
        ],
    );
    let events = run_with_mode(
        agent,
        Content::from_text("What is the weather?", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    // Events 0-2 are the partial chunks of the first model call and event 3 is the
    // aggregated response that completes them. Event 4 is the function response and event 5
    // comes from a second model call.
    assert_eq!(events.len(), 6, "events");
    for i in 1..=3 {
        assert_eq!(
            events[i].id, events[0].id,
            "events[{i}].ID, want the id of the partial events[0]"
        );
    }
    assert_ne!(
        events[4].id, events[0].id,
        "function response events[4].ID, want an id different from the model response"
    );
    assert_ne!(
        events[5].id, events[0].id,
        "second model call events[5].ID, want an id different from the first call"
    );
}

#[tokio::test]
#[ignore = "8a bug: the LLM flow gives each partial text event of one SSE model call a fresh id; Go 3188db98 keeps one id across the call's partials and its aggregated response"]
async fn test_progressive_sse_text_stream_shares_event_id() {
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "text_stream_agent".into(),
        model: Some(Arc::new(StreamingMockModel {
            stream_chunks: vec![
                chunk(vec![text_part("Hello ", false)], None),
                chunk(vec![text_part("world", false)], None),
                chunk(vec![text_part("!", false)], Some(FinishReason::Stop)),
            ],
            call_count: AtomicUsize::new(0),
        })),
        ..Default::default()
    })
    .unwrap();
    let events = run_with_mode(
        agent,
        Content::from_text("Say hello.", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    // Three partial chunks, then the aggregated "Hello world!".
    assert_eq!(events.len(), 4, "events");
    assert!(
        !events[3].response.partial,
        "events[3].Partial = true, want the aggregated final response"
    );
    for i in 1..=3 {
        assert_eq!(
            events[i].id, events[0].id,
            "events[{i}].ID, want the id of the partial events[0]"
        );
    }
}

/// Port of Go `twoFinalResponsesModel`: two complete responses from its first call, text and
/// then a function call, and a closing text from any later call.
struct TwoFinalResponsesModel {
    calls: AtomicUsize,
}

impl Llm for TwoFinalResponsesModel {
    fn name(&self) -> &str {
        "two-finals-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let responses = if call == 1 {
            vec![
                Ok(chunk(
                    vec![text_part("Let me check the weather.", false)],
                    None,
                )),
                Ok(chunk(vec![weather_call("Tokyo")], Some(FinishReason::Stop))),
            ]
        } else {
            vec![Ok(chunk(
                vec![text_part("Done.", false)],
                Some(FinishReason::Stop),
            ))]
        };
        stream::iter(responses).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_complete_responses_of_one_model_call_get_distinct_event_i_ds() {
    let tool = FunctionTool::new(
        FunctionToolConfig::new("get_weather", "get weather for location"),
        |_ctx: Context, args: GetWeatherArgs| async move {
            Ok::<_, ToolError>(GetWeatherResult {
                temperature: 22,
                condition: "sunny".into(),
                location: args.location,
            })
        },
    )
    .unwrap();
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "distinct_id_agent".into(),
        model: Some(Arc::new(TwoFinalResponsesModel {
            calls: AtomicUsize::new(0),
        })),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap();
    let events = run_with_mode(
        agent,
        Content::from_text("What is the weather?", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    // The text, the function call, the function response and "Done.".
    assert_eq!(events.len(), 4, "events");
    assert_ne!(
        events[0].id, events[1].id,
        "text and function call events share an ID, want distinct ids"
    );
}

/// Port of Go `errorMidStreamModel`: streams a partial response, then an error, then a
/// complete response.
struct ErrorMidStreamModel;

impl Llm for ErrorMidStreamModel {
    fn name(&self) -> &str {
        "error-mid-stream-mock"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        stream::iter(vec![
            Ok(LlmResponse {
                partial: true,
                ..chunk(vec![text_part("par1", false)], None)
            }),
            Err(ModelError::Other("boom".into())),
            Ok(chunk(
                vec![text_part("after error", false)],
                Some(FinishReason::Stop),
            )),
        ])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// parity: adapted: Go's "error with a partial response" case yields a response together with
// the error; a gadk model stream item is a response or an error, so only the case without a
// response is expressible.
#[tokio::test]
async fn test_recovered_model_error_gets_its_own_event_id() {
    let recover: OnModelErrorCallback = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(Some(LlmResponse {
                content: Some(Content::from_text("recovered", ROLE_MODEL)),
                ..Default::default()
            }))
        })
    });
    let agent = gadk_engine::llm::agent::new(LlmAgentConfig {
        name: "recovering_agent".into(),
        model: Some(Arc::new(ErrorMidStreamModel)),
        on_model_error_callbacks: vec![recover],
        ..Default::default()
    })
    .unwrap();
    let events = run_with_mode(
        agent,
        Content::from_text("Hi.", ROLE_USER),
        StreamingMode::Sse,
    )
    .await
    .unwrap();

    // The partial "par1", the recovered response, then "after error". The last two are
    // saved, so their ids must differ.
    assert_eq!(events.len(), 3, "events");
    assert_eq!(part(&events[1], 0).text, "recovered", "events[1] text");
    assert_ne!(
        events[2].id, events[1].id,
        "events[2].ID, want an id different from the recovered events[1]"
    );
}
