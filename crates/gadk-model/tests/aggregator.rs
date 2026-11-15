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

//! Port of ADK-Go `internal/llminternal/stream_aggregator_test.go`.

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use gadk_genai::*;
use gadk_model::*;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn fc_part(fc: FunctionCall) -> Part {
    Part {
        function_call: Some(fc),
        ..Default::default()
    }
}

fn text_part(text: &str, thought: bool) -> Part {
    Part {
        text: text.into(),
        thought,
        ..Default::default()
    }
}

fn pa(json_path: &str) -> PartialArg {
    PartialArg {
        json_path: json_path.into(),
        ..Default::default()
    }
}

fn pa_str(json_path: &str, s: &str) -> PartialArg {
    PartialArg {
        string_value: s.into(),
        ..pa(json_path)
    }
}

fn pa_num(json_path: &str, n: f64) -> PartialArg {
    PartialArg {
        number_value: Some(n),
        ..pa(json_path)
    }
}

fn pa_bool(json_path: &str, b: bool) -> PartialArg {
    PartialArg {
        bool_value: Some(b),
        ..pa(json_path)
    }
}

fn model_content(parts: Vec<Part>) -> Option<Content> {
    Some(Content {
        parts,
        role: ROLE_MODEL.into(),
    })
}

fn chunk_with(parts: Vec<Part>, finish_reason: Option<FinishReason>) -> GenerateContentResponse {
    GenerateContentResponse {
        candidates: vec![Candidate {
            content: model_content(parts),
            finish_reason,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Runs every chunk through `agg`, failing on any yielded error, and returns the yielded
/// responses (Go `for resp, err := range aggregator.ProcessResponse(ctx, chunk)`).
fn process_all(
    agg: &mut StreamingResponseAggregator,
    chunks: Vec<GenerateContentResponse>,
) -> Vec<LlmResponse> {
    let mut out = Vec::new();
    for chunk in chunks {
        for item in agg.process_response(chunk) {
            match item {
                Ok(resp) => out.push(resp),
                Err(err) => panic!("unexpected error: {err}"),
            }
        }
    }
    out
}

fn final_parts(agg: &mut StreamingResponseAggregator) -> Vec<Part> {
    let final_response = agg
        .close()
        .expect("expected final response from aggregator");
    final_response
        .content
        .expect("expected content in final response")
        .parts
}

/// Marshals a JSON value the way Go's `encoding/json` marshals a `map[string]any`
/// holding `float64` numbers: object keys sorted, integral floats printed without a
/// fractional part.
fn go_json(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => format!("{f}"),
            _ => n.to_string(),
        },
        Value::String(s) => serde_json::to_string(s).unwrap(),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(go_json).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        go_json(&members[k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Port of Go `streamFunctionCall`: runs one streamed function call the way Vertex sends
/// it — a first chunk that only names the call, then nameless chunks of partial arguments,
/// then the chunk that ends it — and returns the assembled arguments as Go-encoded JSON.
fn stream_function_call(name: &str, chunks: Vec<Vec<PartialArg>>) -> String {
    let mut calls = vec![FunctionCall {
        name: name.into(),
        id: format!("fc_{name}"),
        will_continue: Some(true),
        ..Default::default()
    }];
    for partial_args in chunks {
        calls.push(FunctionCall {
            partial_args,
            will_continue: Some(true),
            ..Default::default()
        });
    }
    calls.push(FunctionCall {
        will_continue: Some(false),
        ..Default::default()
    });

    let mut agg = StreamingResponseAggregator::new();
    let last = calls.len() - 1;
    for (i, call) in calls.into_iter().enumerate() {
        let finish_reason = (i == last).then_some(FinishReason::Stop);
        let chunk = chunk_with(vec![fc_part(call)], finish_reason);
        for item in agg.process_response(chunk) {
            if let Err(err) = item {
                panic!("chunk {i}: unexpected error: {err}");
            }
        }
    }

    let parts = final_parts(&mut agg);
    assert!(
        parts.len() == 1 && parts[0].function_call.is_some(),
        "expected exactly one function call part, got {parts:?}"
    );
    go_json(&Value::Object(
        parts[0].function_call.as_ref().unwrap().args.clone(),
    ))
}

// ---------------------------------------------------------------------------
// Aggregator-level tests
// ---------------------------------------------------------------------------

#[test]
fn test_progressive_sse_streaming_function_call_arguments() {
    let mut agg = StreamingResponseAggregator::new();

    // Chunk 1: FC name + partial location argument ("New ")
    let chunk1 = chunk_with(
        vec![fc_part(FunctionCall {
            name: "get_weather".into(),
            id: "fc_001".into(),
            partial_args: vec![pa_str("$.location", "New ")],
            will_continue: Some(true),
            ..Default::default()
        })],
        None,
    );
    // Chunk 2: Continue location argument ("York")
    let chunk2 = chunk_with(
        vec![fc_part(FunctionCall {
            partial_args: vec![pa_str("$.location", "York")],
            will_continue: Some(true),
            ..Default::default()
        })],
        None,
    );
    // Chunk 3: Add unit argument, FC complete
    let chunk3 = chunk_with(
        vec![fc_part(FunctionCall {
            partial_args: vec![pa_str("$.unit", "celsius")],
            will_continue: Some(false),
            ..Default::default()
        })],
        Some(FinishReason::Stop),
    );

    let _processed = process_all(&mut agg, vec![chunk1, chunk2, chunk3]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 1, "expected 1 part");
    let fc = parts[0]
        .function_call
        .as_ref()
        .expect("expected function call part");
    assert_eq!(fc.name, "get_weather");
    assert_eq!(fc.id, "fc_001");
    assert_eq!(fc.args.get("location"), Some(&json!("New York")));
    assert_eq!(fc.args.get("unit"), Some(&json!("celsius")));
}

#[test]
fn test_thought_signature_propagation_to_first_function_call_separate_responses() {
    let mut agg = StreamingResponseAggregator::new();
    let sig = Bytes::from_static(b"test_signature_123");

    // Emulate an executable code part carrying thought signature
    let chunk1 = chunk_with(
        vec![Part {
            thought_signature: sig.clone(),
            ..Default::default()
        }],
        None,
    );
    let chunk2 = chunk_with(
        vec![
            fc_part(FunctionCall {
                name: "print".into(),
                args: args(&[("message", json!("hello"))]),
                ..Default::default()
            }),
            fc_part(FunctionCall {
                name: "print".into(),
                args: args(&[("message", json!("world"))]),
                ..Default::default()
            }),
        ],
        Some(FinishReason::Stop),
    );

    process_all(&mut agg, vec![chunk1, chunk2]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 3, "expected 3 parts");

    // The first function call should get the propagated thought signature
    assert!(
        parts[1]
            .function_call
            .as_ref()
            .is_some_and(|fc| fc.name == "print"),
        "expected first function call to be print"
    );
    assert_eq!(parts[1].thought_signature, sig);

    // The second function call should NOT get the signature
    assert!(
        parts[2]
            .function_call
            .as_ref()
            .is_some_and(|fc| fc.name == "print"),
        "expected second function call to be print"
    );
    assert!(
        parts[2].thought_signature.is_empty(),
        "expected second function call to have no thought signature, got {:?}",
        parts[2].thought_signature
    );
}

#[test]
fn test_thought_signature_propagation_to_first_function_call_single_response() {
    let mut agg = StreamingResponseAggregator::new();
    let sig = Bytes::from_static(b"test_signature_123");

    let chunk = chunk_with(
        vec![
            // Emulate an executable code part carrying thought signature
            Part {
                thought_signature: sig.clone(),
                ..Default::default()
            },
            fc_part(FunctionCall {
                name: "print".into(),
                args: args(&[("message", json!("hello"))]),
                ..Default::default()
            }),
            fc_part(FunctionCall {
                name: "print".into(),
                args: args(&[("message", json!("world"))]),
                ..Default::default()
            }),
        ],
        Some(FinishReason::Stop),
    );

    process_all(&mut agg, vec![chunk]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 3, "expected 3 parts");

    assert!(
        parts[1]
            .function_call
            .as_ref()
            .is_some_and(|fc| fc.name == "print"),
        "expected first function call to be print"
    );
    assert_eq!(parts[1].thought_signature, sig);

    assert!(
        parts[2]
            .function_call
            .as_ref()
            .is_some_and(|fc| fc.name == "print"),
        "expected second function call to be print"
    );
    assert!(
        parts[2].thought_signature.is_empty(),
        "expected second function call to have no thought signature, got {:?}",
        parts[2].thought_signature
    );
}

#[test]
fn test_progressive_sse_preserves_thought_signature() {
    let mut agg = StreamingResponseAggregator::new();
    let sig = Bytes::from_static(b"test_signature_abc123");

    let chunk = chunk_with(
        vec![Part {
            function_call: Some(FunctionCall {
                name: "add_5_numbers".into(),
                id: "fc_003".into(),
                partial_args: vec![pa_num("$.num1", 10.0), pa_num("$.num2", 20.0)],
                will_continue: Some(false),
                ..Default::default()
            }),
            thought_signature: sig.clone(),
            ..Default::default()
        }],
        Some(FinishReason::Stop),
    );

    process_all(&mut agg, vec![chunk]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 1, "expected 1 part");
    let fc = parts[0]
        .function_call
        .as_ref()
        .expect("expected function call");
    assert_eq!(fc.name, "add_5_numbers");
    assert_eq!(parts[0].thought_signature, sig);
}

/// A thought signature streamed on a text chunk stays on the part the chunks are merged
/// into, as adk-python's StreamingResponseAggregator keeps it.
#[test]
#[ignore = "8a bug: StreamingResponseAggregator::flush_text_buffer_to_sequence drops the thought signature streamed on a text chunk (Go 28201d64 keeps the first one on the merged part)"]
fn test_text_thought_signature_survives_merge() {
    fn text_chunk(
        text: &str,
        thought: bool,
        signature: &'static str,
        finish_reason: Option<FinishReason>,
    ) -> GenerateContentResponse {
        let part = Part {
            thought_signature: Bytes::from_static(signature.as_bytes()),
            ..text_part(text, thought)
        };
        chunk_with(vec![part], finish_reason)
    }
    fn signed_text(text: &str, thought: bool, signature: &'static str) -> Part {
        Part {
            thought_signature: Bytes::from_static(signature.as_bytes()),
            ..text_part(text, thought)
        }
    }
    let call_chunk = || {
        chunk_with(
            vec![fc_part(FunctionCall {
                name: "lookup".into(),
                args: args(&[("q", json!("dog"))]),
                ..Default::default()
            })],
            Some(FinishReason::Stop),
        )
    };
    let stop = Some(FinishReason::Stop);
    let cases: Vec<(&str, Vec<GenerateContentResponse>, Vec<Part>)> = vec![
        (
            "signature_on_first_chunk",
            vec![
                text_chunk("At minute 5 ", false, "text-signature", None),
                text_chunk("the presenter speaks.", false, "", stop.clone()),
            ],
            vec![signed_text(
                "At minute 5 the presenter speaks.",
                false,
                "text-signature",
            )],
        ),
        (
            "signature_on_later_chunk",
            vec![
                text_chunk("At minute 5 ", false, "", None),
                text_chunk(
                    "the presenter speaks.",
                    false,
                    "late-signature",
                    stop.clone(),
                ),
            ],
            vec![signed_text(
                "At minute 5 the presenter speaks.",
                false,
                "late-signature",
            )],
        ),
        (
            "first_signature_of_a_run_wins",
            vec![
                text_chunk("At minute 5 ", false, "first-signature", None),
                text_chunk(
                    "the presenter speaks.",
                    false,
                    "second-signature",
                    stop.clone(),
                ),
            ],
            vec![signed_text(
                "At minute 5 the presenter speaks.",
                false,
                "first-signature",
            )],
        ),
        (
            "thought_and_answer_keep_their_own_signatures",
            vec![
                text_chunk("Let me check.", true, "thought-signature", None),
                text_chunk("It is a dog.", false, "answer-signature", stop.clone()),
            ],
            vec![
                signed_text("Let me check.", true, "thought-signature"),
                signed_text("It is a dog.", false, "answer-signature"),
            ],
        ),
        (
            // The function call still gets the signature too, as it did before the text
            // part kept it.
            "signed_text_then_unsigned_call",
            vec![
                text_chunk("Let me look.", false, "text-signature", None),
                call_chunk(),
            ],
            vec![
                signed_text("Let me look.", false, "text-signature"),
                Part {
                    function_call: Some(FunctionCall {
                        name: "lookup".into(),
                        args: args(&[("q", json!("dog"))]),
                        ..Default::default()
                    }),
                    thought_signature: Bytes::from_static(b"text-signature"),
                    ..Default::default()
                },
            ],
        ),
    ];
    for (name, chunks, want) in cases {
        let mut agg = StreamingResponseAggregator::new();
        process_all(&mut agg, chunks);
        let final_response = agg
            .close()
            .unwrap_or_else(|| panic!("{name}: expected a final aggregated response, got None"));
        assert_eq!(
            want,
            final_response.content.unwrap().parts,
            "{name}: aggregated parts mismatch"
        );
    }
}

#[test]
fn test_progressive_sse_handles_empty_function_call() {
    let mut agg = StreamingResponseAggregator::new();

    let chunk1 = chunk_with(
        vec![fc_part(FunctionCall {
            name: "concat_number_and_string".into(),
            id: "fc_001".into(),
            partial_args: vec![pa_num("$.num", 100.0), pa_str("$.s", "ADK")],
            will_continue: Some(false),
            ..Default::default()
        })],
        None,
    );
    let chunk2 = chunk_with(
        vec![fc_part(FunctionCall::default())],
        Some(FinishReason::Stop),
    );

    process_all(&mut agg, vec![chunk1, chunk2]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 1, "expected 1 part");
    let fc = parts[0]
        .function_call
        .as_ref()
        .expect("expected function call");
    assert_eq!(fc.name, "concat_number_and_string");
    assert_eq!(fc.id, "fc_001");
    assert_eq!(fc.args.get("num"), Some(&json!(100.0)));
    assert_eq!(fc.args.get("s"), Some(&json!("ADK")));
}

#[test]
fn test_streaming_fc_chunk_with_will_continue_but_no_partial_args() {
    let mut agg = StreamingResponseAggregator::new();

    let chunk1 = chunk_with(
        vec![Part {
            function_call: Some(FunctionCall {
                name: "my_tool".into(),
                id: "fc_gemini3".into(),
                will_continue: Some(true),
                ..Default::default()
            }),
            thought_signature: Bytes::from_static(b"test_sig_123"),
            ..Default::default()
        }],
        None,
    );
    let chunk2 = chunk_with(
        vec![fc_part(FunctionCall {
            partial_args: vec![pa_str("$.document", "Once upon ")],
            will_continue: Some(true),
            ..Default::default()
        })],
        None,
    );
    let chunk3 = chunk_with(
        vec![fc_part(FunctionCall {
            partial_args: vec![pa_str("$.document", "a time...")],
            will_continue: Some(true),
            ..Default::default()
        })],
        None,
    );
    let chunk4 = chunk_with(
        vec![fc_part(FunctionCall {
            will_continue: Some(false),
            ..Default::default()
        })],
        Some(FinishReason::Stop),
    );

    let _processed = process_all(&mut agg, vec![chunk1, chunk2, chunk3, chunk4]);

    let parts = final_parts(&mut agg);
    assert_eq!(parts.len(), 1, "expected 1 part");
    let fc = parts[0]
        .function_call
        .as_ref()
        .expect("expected function call");
    assert_eq!(fc.name, "my_tool");
    assert_eq!(fc.id, "fc_gemini3");
    assert_eq!(
        parts[0].thought_signature,
        Bytes::from_static(b"test_sig_123")
    );
    assert_eq!(fc.args.get("document"), Some(&json!("Once upon a time...")));
}

#[test]
#[ignore = "8a bug: StreamingResponseAggregator does not give a nameless continuation chunk of a streamed function call the call's id (Go 3188db98)"]
fn test_streaming_function_call_continuation_carries_call_id() {
    let cases = [
        (
            "closing chunk with partial args",
            FunctionCall {
                partial_args: vec![pa_str("$.y", "val2")],
                will_continue: Some(false),
                ..Default::default()
            },
        ),
        (
            "closing chunk that only ends the call",
            FunctionCall {
                will_continue: Some(false),
                ..Default::default()
            },
        ),
        ("empty closing chunk", FunctionCall::default()),
    ];
    for (name, closing) in cases {
        let mut agg = StreamingResponseAggregator::new();
        let chunks = vec![
            chunk_with(
                vec![fc_part(FunctionCall {
                    name: "my_tool".into(),
                    id: "explicit_fc_id_123".into(),
                    partial_args: vec![pa_str("$.x", "val1")],
                    will_continue: Some(true),
                    ..Default::default()
                })],
                None,
            ),
            chunk_with(vec![fc_part(closing)], Some(FinishReason::Stop)),
        ];
        let partials = process_all(&mut agg, chunks);
        let final_response = agg.close();

        assert_eq!(partials.len(), 2, "{name}: partial responses");
        for (i, resp) in partials.iter().enumerate() {
            let got = &resp.content.as_ref().unwrap().parts[0]
                .function_call
                .as_ref()
                .unwrap()
                .id;
            assert_eq!(
                got, "explicit_fc_id_123",
                "{name}: partial response {i} function call ID"
            );
        }
        let final_response =
            final_response.unwrap_or_else(|| panic!("{name}: Close() = None, want a response"));
        let got = &final_response.content.as_ref().unwrap().parts[0]
            .function_call
            .as_ref()
            .unwrap()
            .id;
        assert_eq!(
            got, "explicit_fc_id_123",
            "{name}: aggregated function call ID"
        );
    }
}

// ---------------------------------------------------------------------------
// Flow-level tests.
//
// In Go these drive an llmagent + runner over a mock model that pipes its chunks through
// the aggregator. gadk has no agent runtime yet (sub-project 1a), so the mock model is
// ported as an `Llm` and the assertions on the model events it produces are kept
// verbatim. Assertions on events the agent flow itself creates (tool execution and the
// follow-up model call) are ported against a minimal stand-in of the flow's rule that only
// non-partial function calls are executed.
// ---------------------------------------------------------------------------

/// Port of Go `streamingMockModel` (first call only: the agent flow's second call, which
/// returns "Task completed.", is not reachable without an agent runtime).
struct StreamingMockModel {
    stream_chunks: Vec<LlmResponse>,
}

/// Port of Go `streamingMockModel.llmResponseToGenerateContentResponse`.
fn llm_response_to_generate_content_response(resp: &LlmResponse) -> GenerateContentResponse {
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        "streaming-mock"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut agg = StreamingResponseAggregator::new();
        let mut out = Vec::new();
        for chunk in &self.stream_chunks {
            let gen_resp = llm_response_to_generate_content_response(chunk);
            for item in agg.process_response(gen_resp) {
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
}

async fn collect_model_events(model: &dyn Llm) -> Vec<LlmResponse> {
    model
        .generate_content(LlmRequest::default(), true)
        .map(|r| r.expect("unexpected error"))
        .collect()
        .await
}

fn fc_of(resp: &LlmResponse, i: usize) -> &FunctionCall {
    resp.content.as_ref().unwrap().parts[i]
        .function_call
        .as_ref()
        .unwrap()
}

fn part_of(resp: &LlmResponse, i: usize) -> &Part {
    &resp.content.as_ref().unwrap().parts[i]
}

fn get_weather_content(location: &str) -> Option<Content> {
    model_content(vec![fc_part(FunctionCall {
        name: "get_weather".into(),
        args: args(&[("location", json!(location))]),
        ..Default::default()
    })])
}

#[tokio::test]
async fn test_progressive_sse_streaming_function_calls() {
    let response1 = LlmResponse {
        content: model_content(vec![text_part("Checking weather...", false)]),
        ..Default::default()
    };
    let response2 = LlmResponse {
        content: get_weather_content("Tokyo"),
        ..Default::default()
    };
    let response3 = LlmResponse {
        content: get_weather_content("New York"),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let mock = StreamingMockModel {
        stream_chunks: vec![response1, response2, response3],
    };

    let events = collect_model_events(&mock).await;

    // Go expects 6 events: these 4 model events, then the flow's function-response event
    // and the follow-up model call's "Task completed." event.
    assert_eq!(events.len(), 4, "expected 4 model events");

    assert!(
        events[0].partial && part_of(&events[0], 0).text == "Checking weather...",
        "expected partial event 0 with text"
    );
    assert!(
        events[1].partial
            && fc_of(&events[1], 0).name == "get_weather"
            && fc_of(&events[1], 0).args.get("location") == Some(&json!("Tokyo")),
        "expected partial event 1 with function call 1"
    );
    assert!(
        events[2].partial
            && fc_of(&events[2], 0).name == "get_weather"
            && fc_of(&events[2], 0).args.get("location") == Some(&json!("New York")),
        "expected partial event 2 with function call 2"
    );
    assert!(
        !events[3].partial
            && part_of(&events[3], 0).text == "Checking weather..."
            && fc_of(&events[3], 1).name == "get_weather"
            && fc_of(&events[3], 2).name == "get_weather",
        "expected final aggregated event 3 with FCs"
    );

    // Go event 4: the flow executes the non-partial function calls of event 3, producing
    // one function response per call, with Tokyo and New York (in either order).
    let executed: Vec<Value> = events
        .iter()
        .filter(|e| !e.partial)
        .flat_map(|e| e.content.iter().flat_map(|c| c.parts.iter()))
        .filter_map(|p| p.function_call.as_ref())
        .filter(|fc| fc.name == "get_weather")
        .map(|fc| fc.args.get("location").cloned().unwrap_or(Value::Null))
        .collect();
    assert!(
        executed == vec![json!("Tokyo"), json!("New York")]
            || executed == vec![json!("New York"), json!("Tokyo")],
        "expected function responses to have Tokyo and New York, got {executed:?}"
    );
}

#[tokio::test]
async fn test_progressive_sse_preserves_part_ordering() {
    let text_resp = |text: &str, thought: bool| LlmResponse {
        content: model_content(vec![text_part(text, thought)]),
        ..Default::default()
    };
    let chunk1 = text_resp("Initial thought part 1. ", true);
    let chunk2 = text_resp("Initial thought part 2.", true);
    let chunk3 = text_resp("Let me check Tokyo. ", false);
    let chunk4 = text_resp("And New York too.", false);
    let chunk5 = LlmResponse {
        content: get_weather_content("Tokyo"),
        ..Default::default()
    };
    let chunk6 = text_resp("Now processing second thought part 1. ", true);
    let chunk7 = text_resp("Second thought part 2.", true);
    let chunk8 = LlmResponse {
        content: get_weather_content("New York"),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let mock = StreamingMockModel {
        stream_chunks: vec![
            chunk1, chunk2, chunk3, chunk4, chunk5, chunk6, chunk7, chunk8,
        ],
    };

    let events = collect_model_events(&mock).await;

    let aggregated = events
        .iter()
        .find(|e| !e.partial && e.content.as_ref().is_some_and(|c| c.parts.len() > 2))
        .expect("Should find an aggregated model event");

    let parts = &aggregated.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 5, "Expected 5 parts");

    assert!(
        parts[0].thought && parts[0].text == "Initial thought part 1. Initial thought part 2.",
        "part 0 mismatch. got text: {:?}, thought: {}",
        parts[0].text,
        parts[0].thought
    );
    assert!(
        !parts[1].thought && parts[1].text == "Let me check Tokyo. And New York too.",
        "part 1 mismatch. got text: {:?}, thought: {}",
        parts[1].text,
        parts[1].thought
    );
    let fc2 = parts[2].function_call.as_ref().unwrap();
    assert!(
        fc2.name == "get_weather" && fc2.args.get("location") == Some(&json!("Tokyo")),
        "part 2 mismatch. got FC: {fc2:?}"
    );
    assert!(
        parts[3].thought
            && parts[3].text == "Now processing second thought part 1. Second thought part 2.",
        "part 3 mismatch. got text: {:?}, thought: {}",
        parts[3].text,
        parts[3].thought
    );
    let fc4 = parts[4].function_call.as_ref().unwrap();
    assert!(
        fc4.name == "get_weather" && fc4.args.get("location") == Some(&json!("New York")),
        "part 4 mismatch. got FC: {fc4:?}"
    );
}

/// Port of Go `partialFunctionCallMockModel` (first call: no function response yet).
struct PartialFunctionCallMockModel;

impl Llm for PartialFunctionCallMockModel {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        "partial-fc-mock"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let track = |call_id: &str, partial: bool, finish_reason: Option<FinishReason>| {
            Ok(LlmResponse {
                content: model_content(vec![fc_part(FunctionCall {
                    name: "track_execution".into(),
                    args: args(&[("call_id", json!(call_id))]),
                    ..Default::default()
                })]),
                partial,
                finish_reason,
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
}

#[tokio::test]
async fn test_partial_function_calls_not_executed_in_none_streaming_mode() {
    let mock = PartialFunctionCallMockModel;
    let model_events = collect_model_events(&mock).await;

    // Stand-in for the flow: execute the function calls of non-partial events only,
    // emitting one function-response event per executed batch.
    let mut execution_log: Vec<String> = Vec::new();
    let mut events: Vec<LlmResponse> = Vec::new();
    for event in model_events {
        let mut responses = Vec::new();
        if !event.partial {
            for part in event.content.iter().flat_map(|c| c.parts.iter()) {
                if let Some(fc) = &part.function_call
                    && fc.name == "track_execution"
                {
                    let call_id = fc.args["call_id"].as_str().unwrap().to_owned();
                    responses.push(Part::from_function_response(
                        &fc.name,
                        args(&[("result", json!(format!("Executed: {call_id}")))]),
                    ));
                    execution_log.push(call_id);
                }
            }
        }
        events.push(event);
        if !responses.is_empty() {
            events.push(LlmResponse {
                content: Some(Content {
                    parts: responses,
                    role: ROLE_USER.into(),
                }),
                ..Default::default()
            });
        }
    }

    assert_eq!(
        execution_log.len(),
        1,
        "Expected 1 execution, got {execution_log:?}"
    );
    assert_eq!(execution_log[0], "final");

    let partial_events = events.iter().filter(|e| e.partial).count();
    assert_eq!(partial_events, 2, "Expected 2 partial events");

    let function_response_events = events
        .iter()
        .filter(|e| {
            e.content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()))
        })
        .count();
    assert_eq!(
        function_response_events, 1,
        "Expected 1 function response event"
    );
}

// ---------------------------------------------------------------------------
// Metadata / finish-reason tests
// ---------------------------------------------------------------------------

#[test]
fn test_metadata_vertex_aisse_stream() {
    let mut agg = StreamingResponseAggregator::new();

    let empty_text_chunk = GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content::from_text("", ROLE_MODEL)),
            ..Default::default()
        }],
        usage_metadata: Some(GenerateContentResponseUsageMetadata::default()),
        ..Default::default()
    };
    // The leading metadata-only SSE chunk Vertex AI emits for gemini-3-flash-preview +
    // googleSearch grounding: no candidates at all.
    let metadata_chunk = GenerateContentResponse {
        usage_metadata: Some(GenerateContentResponseUsageMetadata::default()),
        ..Default::default()
    };
    // The real content arrives in the next chunk.
    let content_chunk = chunk_with(
        vec![text_part("Here are some movie recommendations.", false)],
        Some(FinishReason::Stop),
    );

    let stream = vec![
        empty_text_chunk.clone(),
        metadata_chunk.clone(),
        metadata_chunk.clone(),
        empty_text_chunk,
        metadata_chunk.clone(),
        metadata_chunk,
        content_chunk,
    ];
    process_all(&mut agg, stream);

    let final_response = agg
        .close()
        .expect("expected a final aggregated response, got None");
    let parts = final_response.content.map(|c| c.parts).unwrap_or_default();
    assert!(
        !parts.is_empty(),
        "expected content parts in the final response, got none"
    );
    assert_eq!(parts[0].text, "Here are some movie recommendations.");
}

#[test]
fn test_metadata_only_chunk_does_not_abort_stream() {
    let mut agg = StreamingResponseAggregator::new();

    let metadata_chunk = GenerateContentResponse {
        usage_metadata: Some(GenerateContentResponseUsageMetadata::default()),
        ..Default::default()
    };
    let content_chunk = chunk_with(
        vec![text_part("Here are some movie recommendations.", false)],
        Some(FinishReason::Stop),
    );

    process_all(&mut agg, vec![metadata_chunk, content_chunk]);

    let final_response = agg
        .close()
        .expect("expected a final aggregated response, got None");
    let parts = final_response.content.map(|c| c.parts).unwrap_or_default();
    assert!(
        !parts.is_empty(),
        "expected content parts in the final response, got none"
    );
    assert_eq!(parts[0].text, "Here are some movie recommendations.");
}

#[test]
fn test_usage_metadata_retained_when_later_chunk_has_no_usage_metadata() {
    let mut agg = StreamingResponseAggregator::new();

    let usage_metadata = GenerateContentResponseUsageMetadata {
        prompt_token_count: 3,
        candidates_token_count: 5,
        total_token_count: 8,
        ..Default::default()
    };
    let chunks = vec![
        GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content::from_text("Hello", ROLE_MODEL)),
                ..Default::default()
            }],
            usage_metadata: Some(usage_metadata.clone()),
            ..Default::default()
        },
        GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content::from_text(" world", ROLE_MODEL)),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }],
            ..Default::default()
        },
    ];
    process_all(&mut agg, chunks);

    let final_response = agg
        .close()
        .expect("expected a final aggregated response, got None");
    let got = final_response
        .usage_metadata
        .expect("expected usage metadata in the final aggregated response, got None");
    assert_eq!(got.prompt_token_count, usage_metadata.prompt_token_count);
    assert_eq!(
        got.candidates_token_count,
        usage_metadata.candidates_token_count
    );
    assert_eq!(got.total_token_count, usage_metadata.total_token_count);
}

/// The aggregated response reports the model version of the stream's last chunk, as a
/// non-streamed response does and as adk-python's StreamingResponseAggregator.close does.
#[test]
#[ignore = "8a bug: StreamingResponseAggregator::close leaves model_version empty (Go 8be9129e copies the last chunk's ModelVersion)"]
fn test_close_keeps_model_version() {
    let mut agg = StreamingResponseAggregator::new();
    let model_version = "gemini-2.5-flash-001";
    let chunks = vec![
        GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content::from_text("Hello", ROLE_MODEL)),
                ..Default::default()
            }],
            ..Default::default()
        },
        GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content::from_text(" world", ROLE_MODEL)),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }],
            model_version: model_version.into(),
            ..Default::default()
        },
    ];
    process_all(&mut agg, chunks);

    let final_response = agg
        .close()
        .expect("expected a final aggregated response, got None");
    assert_eq!(final_response.model_version, model_version);
}

/// A later chunk with usage metadata overwrites an earlier one.
#[test]
fn test_usage_metadata_updated_by_later_chunk_with_value() {
    let mut agg = StreamingResponseAggregator::new();
    let chunk1 = GenerateContentResponse {
        candidates: vec![Candidate {
            content: model_content(vec![text_part("Hello", false)]),
            ..Default::default()
        }],
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            total_token_count: 100,
            ..Default::default()
        }),
        ..Default::default()
    };
    // Updated (cumulative) usage, as typically sent by Gemini.
    let chunk2 = GenerateContentResponse {
        candidates: vec![Candidate {
            content: model_content(vec![text_part(" world", false)]),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }],
        usage_metadata: Some(GenerateContentResponseUsageMetadata {
            total_token_count: 200,
            ..Default::default()
        }),
        ..Default::default()
    };
    process_all(&mut agg, vec![chunk1, chunk2]);

    let final_response = agg
        .close()
        .expect("expected a final aggregated response, got None");
    let usage = final_response
        .usage_metadata
        .expect("UsageMetadata should not be None");
    assert_eq!(
        usage.total_token_count, 200,
        "expected TotalTokenCount=200 (from last chunk)"
    );
}

#[test]
fn test_finish_reason_unexpected_tool_call_preserves_error_code() {
    let mut agg = StreamingResponseAggregator::new();

    // Simulate an LLM chunk that reports UNEXPECTED_TOOL_CALL
    let chunk = chunk_with(
        vec![text_part("Some text", false)],
        Some(FinishReason::UnexpectedToolCall),
    );
    process_all(&mut agg, vec![chunk]);

    let final_response = agg.close().expect("Close should return a valid response");
    assert_eq!(
        final_response.finish_reason,
        Some(FinishReason::UnexpectedToolCall)
    );
    assert_eq!(
        final_response.error_code, "",
        "ErrorCode was unexpectedly overwritten"
    );
    assert_eq!(
        final_response.error_message, "",
        "ErrorMessage was unexpectedly overwritten"
    );
}

// ---------------------------------------------------------------------------
// Streamed JSON-path argument tests
// ---------------------------------------------------------------------------

#[test]
fn test_streamed_array_of_objects_argument() {
    let args = stream_function_call(
        "render_chart",
        vec![
            vec![PartialArg {
                will_continue: Some(true),
                ..pa_str("$.data[0].label", "Jan 20")
            }],
            vec![PartialArg {
                will_continue: Some(true),
                ..pa_str("$.data[0].label", "23")
            }],
            vec![pa("$.data[0].label")],
            vec![pa_num("$.data[0].value", 1250.5)],
            vec![pa_str("$.data[1].label", "Feb 2023")],
            vec![pa_num("$.data[1].value", 990.0)],
            vec![pa_str("$.title", "Revenue")],
        ],
    );
    assert_eq!(
        args,
        r#"{"data":[{"label":"Jan 2023","value":1250.5},{"label":"Feb 2023","value":990}],"title":"Revenue"}"#
    );
}

#[test]
fn test_streamed_array_of_strings_argument() {
    let args = stream_function_call(
        "describe_tables",
        vec![
            vec![PartialArg {
                will_continue: Some(true),
                ..pa_str("$.tables[0]", "ord")
            }],
            vec![pa_str("$.tables[0]", "ers")],
            vec![pa_str("$.tables[1]", "customers")],
        ],
    );
    assert_eq!(args, r#"{"tables":["orders","customers"]}"#);
}

#[test]
fn test_streamed_array_argument_keeps_index_positions() {
    let args = stream_function_call(
        "query_gold_table",
        vec![
            vec![pa_str("$.queries[2]", "third")],
            vec![pa_str("$.queries[0]", "first")],
        ],
    );
    assert_eq!(args, r#"{"queries":["first",null,"third"]}"#);
}

#[test]
fn test_streamed_array_argument_scalar_types() {
    let args = stream_function_call(
        "render_chart",
        vec![
            vec![pa_bool("$.filters[0].enabled", true)],
            vec![PartialArg {
                null_value: "NULL_VALUE".into(),
                ..pa("$.filters[0].value")
            }],
            vec![pa_bool("$.filters[1].enabled", false)],
            vec![pa_bool("$.options.stacked", true)],
            vec![pa_num("$.limit", 10.0)],
        ],
    );
    assert_eq!(
        args,
        r#"{"filters":[{"enabled":true,"value":null},{"enabled":false}],"limit":10,"options":{"stacked":true}}"#
    );
}

#[test]
fn test_streamed_argument_quoted_member_names() {
    let args = stream_function_call(
        "render_chart",
        vec![
            vec![pa_str(r#"$['series.name'][0]["data point"]"#, "x")],
            vec![pa_str(r"$['it\'s']", "y")],
        ],
    );
    assert_eq!(args, r#"{"it's":"y","series.name":[{"data point":"x"}]}"#);
}

#[test]
fn test_streamed_argument_unsupported_paths_dropped() {
    let args = stream_function_call(
        "render_chart",
        vec![
            vec![pa_str("$.data[]", "end of array")],
            vec![pa_str("$.data[*]", "wildcard")],
            vec![pa_str("$.data[0:2]", "slice")],
            vec![pa_str("$..data", "descendant")],
            vec![pa_str("$.data[-1]", "negative")],
            vec![pa_str("", "empty")],
            vec![pa_str("$.title", "kept")],
        ],
    );
    assert_eq!(args, r#"{"title":"kept"}"#);
}

#[test]
fn test_streamed_argument_out_of_range_index_dropped() {
    let args = stream_function_call(
        "render_chart",
        vec![
            vec![pa_str("$.data[65537]", "too large")],
            vec![pa_str("$.data[9999999999999999999]", "overflows int")],
            vec![pa_str("$.title", "kept")],
        ],
    );
    assert_eq!(args, r#"{"title":"kept"}"#);
}

/// Regression test: a streamed chunk whose first candidate is degenerate (Go: a nil
/// `*Candidate`; Rust: `Candidate::default()`) must not panic when checking FinishReason.
#[test]
fn test_process_response_nil_candidate() {
    let mut agg = StreamingResponseAggregator::new();
    let chunk = GenerateContentResponse {
        candidates: vec![Candidate::default()],
        ..Default::default()
    };
    for item in agg.process_response(chunk) {
        if let Err(err) = item {
            panic!("ProcessResponse yielded error: {err}");
        }
    }
}

/// Part-level sibling of the nil-candidate regression above: a degenerate part in a chunk
/// is skipped like the empty part the filter already targets, and skipping it leaves the
/// aggregation of the surrounding text parts undisturbed ("a", nil, "b" stays one part).
// parity: adapted: a Go nil `*genai.Part` has no Rust counterpart; the degenerate part is
// `Part::default()`, the empty part the aggregator filters.
#[test]
fn test_process_response_nil_part() {
    let cases = [
        (
            "nil first",
            vec![Part::default(), Part::from_text("kept")],
            "kept",
        ),
        (
            "nil last",
            vec![Part::from_text("kept"), Part::default()],
            "kept",
        ),
        (
            "text on both sides aggregates across the nil",
            vec![Part::from_text("a"), Part::default(), Part::from_text("b")],
            "ab",
        ),
    ];
    for (name, parts, want_text) in cases {
        let mut agg = StreamingResponseAggregator::new();
        let chunk = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content {
                    parts,
                    role: String::new(),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        for item in agg.process_response(chunk) {
            if let Err(err) = item {
                panic!("{name}: ProcessResponse yielded error: {err}");
            }
        }
        let response = agg.close();
        let parts = response
            .as_ref()
            .and_then(|r| r.content.as_ref())
            .map(|c| c.parts.clone())
            .unwrap_or_default();
        assert!(
            parts.len() == 1 && parts[0].text == want_text,
            "{name}: aggregated response = {response:?}, want the single text part {want_text:?}"
        );
    }
}

// parity: skipped TestStreamingResponseAggregatorDoesNotRecopyText: measures Go runtime.MemStats allocation growth of strings.Builder vs string concatenation; gadk's String::push_str buffer is amortized linear by construction and Rust has no per-test allocation counter
