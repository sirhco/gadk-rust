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

//! `responses/model_test.go`: the stream_terminal tests (Task 9; see `mod.rs`).

use futures::StreamExt as _;
use gadk_genai::{
    FinishReason, FunctionCall, LogprobsResult, LogprobsResultCandidate,
    LogprobsResultTopCandidates,
};
use gadk_model::LlmResponse;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::blocking::{call, terminal};
use super::{
    BODY_FILTERED, BODY_TOOL_CALL, EV_ARGS_DONE_UNNAMED, EV_BARE_COMPLETED, EV_COMPLETED,
    EV_COMPLETED_NO_STATUS, EV_COMPLETED_REASONING, EV_CREATED, EV_CREATED_FAILED, EV_DELTA1,
    EV_DELTA2, EV_FILTERED, EV_MAX_TOKENS, EV_REASONING, incomplete_event,
};
use crate::error::ErrorKind;
use crate::parity_tests::support::{
    FakeResponse, ScriptedClient, TEST_MODEL, TestClientConfig, all_text, assert_finish_signal,
    assert_turn_shape, collect, function_calls, new_loopback_server, new_test_model, run_blocking,
    run_stream, sse_data, user_request,
};
use crate::shared::response::carries_content;

/// The custom-metadata value under `key`, as a string (`""` when absent or not a string).
fn metadata<'a>(resp: &'a LlmResponse, key: &str) -> &'a str {
    resp.custom_metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn test_model_generate_stream_metadata() {
    let events = [
        r#"{"type": "response.created", "response": {"id": "resp_stream_123", "model": "stream-model"}}"#,
        r#"{"type": "response.output_text.delta", "delta": "chunk1"}"#,
        r#"{"type": "response.completed", "response": {"id": "resp_stream_123", "model": "stream-model", "usage": {"total_tokens": 10}}}"#,
        "[DONE]",
    ];
    let resp = FakeResponse::sse(sse_data(&events))
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive");
    let server = new_loopback_server(move |_| resp.clone());
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));

    let mut chunks = 0;
    let mut final_resp = None;
    let mut items = llm.generate_content(user_request("World?"), true);
    while let Some(item) = items.next().await {
        let resp = item.unwrap_or_else(|err| panic!("GenerateContent() stream err = {err}"));
        chunks += 1;
        assert_eq!(
            metadata(&resp, "openai_response_id"),
            "resp_stream_123",
            "expected chunk to have openai_response_id='resp_stream_123'"
        );
        assert_eq!(
            metadata(&resp, "openai_model"),
            "stream-model",
            "expected chunk to have openai_model='stream-model'"
        );
        final_resp = Some(resp);
    }
    assert_eq!(
        server.last_request().path,
        "/v1/responses",
        "unexpected path"
    );
    // The partial chunk and the final aggregated response.
    assert_eq!(chunks, 2, "expected 2 chunks from stream");
    let usage = final_resp
        .and_then(|r| r.usage_metadata)
        .unwrap_or_else(|| panic!("expected final stream response to have UsageMetadata, got nil"));
    assert_eq!(
        usage.total_token_count, 10,
        "expected final UsageMetadata.TotalTokenCount=10"
    );
}

/// Go `helloLogprobs`.
fn hello_logprobs() -> LogprobsResult {
    let hello = LogprobsResultCandidate {
        token: "hello".to_string(),
        log_probability: -0.5,
        ..LogprobsResultCandidate::default()
    };
    LogprobsResult {
        chosen_candidates: vec![hello.clone()],
        top_candidates: vec![LogprobsResultTopCandidates {
            candidates: vec![hello],
        }],
        ..LogprobsResult::default()
    }
}

/// A `TestModel_GenerateStream_TurnComplete` case: name, events, finish reason, logprobs,
/// model version, text.
type TurnCase<'a> = (
    &'a str,
    Vec<&'a str>,
    FinishReason,
    Option<LogprobsResult>,
    &'a str,
    &'a str,
);

/// Pins the end of a streamed turn: the last response is the only terminal one, and it
/// carries the finish reason and logprobs the same output reports without streaming.
#[tokio::test]
async fn test_model_generate_stream_turn_complete() {
    let incomplete_with_output = r#"{"type":"response.incomplete","response":{"id":"resp_1","model":"stream-model","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}}"#;
    let tests: Vec<TurnCase<'_>> = vec![
        (
            "completed",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, EV_COMPLETED],
            FinishReason::Stop,
            Some(hello_logprobs()),
            "stream-model",
            "hello",
        ),
        (
            "completed response restores missing delta",
            vec![EV_CREATED, EV_DELTA1, EV_COMPLETED],
            FinishReason::Stop,
            Some(hello_logprobs()),
            "stream-model",
            "hello",
        ),
        (
            "truncated by max output tokens",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, EV_MAX_TOKENS],
            FinishReason::MaxTokens,
            None,
            "stream-model",
            "hello",
        ),
        (
            "incomplete response preserves deltas",
            vec![EV_CREATED, EV_DELTA1, incomplete_with_output],
            FinishReason::MaxTokens,
            None,
            "stream-model",
            "hel",
        ),
        (
            "cut short by the content filter",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, EV_FILTERED],
            FinishReason::Safety,
            None,
            "stream-model",
            "hello",
        ),
        // The model never said why it stopped, so report it as unknown rather than invent a
        // clean stop.
        (
            "stream ends without a terminal response",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2],
            FinishReason::Unspecified,
            None,
            "stream-model",
            "hello",
        ),
        // No response object at all, so no model name to read off one.
        (
            "no response object at all",
            vec![EV_DELTA1, EV_DELTA2],
            FinishReason::Unspecified,
            None,
            "",
            "hello",
        ),
        // A provider that batches its output streams no deltas at all.
        (
            "output batched onto the terminal event",
            vec![EV_CREATED, EV_COMPLETED],
            FinishReason::Stop,
            Some(hello_logprobs()),
            "stream-model",
            "hello",
        ),
        // A "completed" after a truncation must not relabel it a clean stop.
        (
            "first terminal event wins",
            vec![EV_CREATED, EV_DELTA1, EV_MAX_TOKENS, EV_COMPLETED],
            FinishReason::MaxTokens,
            None,
            "stream-model",
            "hel",
        ),
        (
            "first completed event wins",
            vec![EV_CREATED, EV_DELTA1, EV_COMPLETED, EV_MAX_TOKENS],
            FinishReason::Stop,
            Some(hello_logprobs()),
            "stream-model",
            "hello",
        ),
        // Nor must a stray "created", whose response has no reason of its own.
        (
            "trailing response.created does not reopen the turn",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, EV_MAX_TOKENS, EV_CREATED],
            FinishReason::MaxTokens,
            None,
            "stream-model",
            "hello",
        ),
        // Reading the status must not cost a provider that omits it its clean stop.
        (
            "terminal body states no status",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, EV_COMPLETED_NO_STATUS],
            FinishReason::Stop,
            None,
            "stream-model",
            "hello",
        ),
        // A failure the turn then recovers from: only a terminal event settles how a turn
        // ended, so this is an answer.
        (
            "failure announced at creation, then completed",
            vec![EV_CREATED_FAILED, EV_DELTA1, EV_DELTA2, EV_COMPLETED],
            FinishReason::Stop,
            Some(hello_logprobs()),
            "stream-model",
            "hello",
        ),
    ];
    for (name, events, want_finish, want_logprobs, want_model_version, want_text) in tests {
        let (got, err) = run_stream(&events).await;
        if let Some(err) = err {
            panic!("{name}: GenerateContent() stream err = {err}");
        }
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            final_resp.finish_reason,
            Some(want_finish),
            "{name}: final FinishReason"
        );
        assert_eq!(
            final_resp.logprobs_result, want_logprobs,
            "{name}: final LogprobsResult"
        );
        assert_eq!(
            final_resp.model_version, want_model_version,
            "{name}: final ModelVersion"
        );
        assert_eq!(
            all_text(final_resp.content.as_ref()),
            want_text,
            "{name}: final text"
        );
    }
}

/// An event whose name declares the turn incomplete never closes it as a clean stop;
/// openai-go marks `incomplete_details` required but not its reason, so every shape below is
/// schema-legal.
#[tokio::test]
async fn test_model_generate_stream_incomplete_without_reason() {
    // (name, fields, finish reason, message)
    let tests: Vec<(&str, &str, FinishReason, &str)> = vec![
        (
            "empty incomplete_details",
            r#""status":"incomplete","incomplete_details":{},"#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "incomplete_details omitted",
            r#""status":"incomplete","#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "incomplete_details null",
            r#""status":"incomplete","incomplete_details":null,"#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "reason empty",
            r#""status":"incomplete","incomplete_details":{"reason":""},"#,
            FinishReason::Other,
            "incomplete",
        ),
        // Neither a status nor a reason: only the event's own name says the turn was cut
        // short.
        ("status omitted too", "", FinishReason::Other, "incomplete"),
        // No status, but an incomplete_details object the provider did send.
        (
            "empty incomplete_details, no status",
            r#""incomplete_details":{},"#,
            FinishReason::Other,
            "incomplete",
        ),
        // An unmapped reason still flattens to OTHER, leaving the provider's wording the only
        // account of what happened.
        (
            "unrecognized reason",
            r#""status":"incomplete","incomplete_details":{"reason":"something_new"},"#,
            FinishReason::Other,
            "something_new",
        ),
    ];
    for (name, fields, want_finish, want_message) in tests {
        let event = incomplete_event(fields);
        let (got, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, &event]).await;
        if let Some(err) = err {
            panic!("{name}: GenerateContent() stream err = {err}");
        }
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            final_resp.finish_reason,
            Some(want_finish),
            "{name}: final FinishReason"
        );
        assert_finish_signal(final_resp, want_message);
    }
}

/// The two paths agree on a truncated turn wherever the payload itself carries the news.
#[tokio::test]
async fn test_incomplete_without_reason_matches_blocking() {
    // (name, fields, finish reason, message)
    let tests: Vec<(&str, &str, FinishReason, &str)> = vec![
        (
            "status incomplete",
            r#""status":"incomplete","#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "empty incomplete_details",
            r#""incomplete_details":{},"#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "empty reason",
            r#""incomplete_details":{"reason":""},"#,
            FinishReason::Other,
            "incomplete",
        ),
        (
            "a mapped reason",
            r#""incomplete_details":{"reason":"max_output_tokens"},"#,
            FinishReason::MaxTokens,
            "",
        ),
    ];
    for (name, fields, want, want_message) in tests {
        let event = incomplete_event(fields);
        let (streamed, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, &event]).await;
        if let Some(err) = err {
            panic!("{name}: streaming err = {err}");
        }
        // incompleteEvent wraps exactly this body.
        let body = event
            .strip_prefix(r#"{"type":"response.incomplete","response":"#)
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or_default();
        let (blocking, err) = run_blocking(body).await;
        if let Some(err) = err {
            panic!("{name}: blocking err = {err}");
        }
        assert_eq!(blocking.len(), 1, "{name}: blocking emitted responses");
        for (path, got) in [
            ("streamed", assert_turn_shape(&streamed)),
            ("blocking", &blocking[0]),
        ] {
            assert_eq!(
                got.finish_reason,
                Some(want.clone()),
                "{name}: {path} FinishReason"
            );
            // The wording has to match too, not just the reason.
            assert_eq!(
                metadata(got, crate::FINISH_MESSAGE_KEY),
                want_message,
                "{name}: {path} CustomMetadata[{:?}]",
                crate::FINISH_MESSAGE_KEY
            );
        }
    }
}

/// A content-filtered turn reaches the caller with something to gate on: metadata beside a
/// SAFETY finish reason when it still carries an answer, and `error_code` when it does not.
#[tokio::test]
async fn test_blocked_turn_surfaces_block_reason() {
    // "with an answer to read"
    let (streamed, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, EV_FILTERED]).await;
    if let Some(err) = err {
        panic!("with an answer to read: streaming err = {err}");
    }
    let (blocking, err) = run_blocking(BODY_FILTERED).await;
    if let Some(err) = err {
        panic!("with an answer to read: blocking err = {err}");
    }
    assert_eq!(
        blocking.len(),
        1,
        "with an answer to read: blocking emitted responses"
    );
    for (path, got) in [
        ("streamed", assert_turn_shape(&streamed)),
        ("blocking", &blocking[0]),
    ] {
        assert_eq!(
            got.finish_reason,
            Some(FinishReason::Safety),
            "with an answer to read/{path}: FinishReason"
        );
        assert_finish_signal(got, "content_filter");
    }

    // "with nothing to read": the deltas produce a call the aggregator drops, so the turn
    // is blocked with no content at all, the case error_code is for.
    let (got, err) = run_stream(&[EV_CREATED, EV_ARGS_DONE_UNNAMED, EV_FILTERED]).await;
    if let Some(err) = err {
        panic!("with nothing to read: streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert!(
        !carries_content(Some(final_resp)),
        "with nothing to read: final carries content {:?}, so this is not the case being asserted",
        final_resp.content
    );
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Safety),
        "with nothing to read: final FinishReason"
    );
    assert_eq!(
        final_resp.error_code, "SAFETY",
        "with nothing to read: final ErrorCode (genai.BlockedReasonSafety)"
    );
    assert_eq!(
        final_resp.error_message, "content_filter",
        "with nothing to read: final ErrorMessage"
    );
}

/// The arm of `attachFinishSignal` that sets `error_code`: a turn that ended for a reason
/// this package does not map, carrying nothing to read.
#[tokio::test]
async fn test_unfinished_turn_with_nothing_to_read_reports_error_code() {
    // The unnamed call is dropped in aggregation, so the turn carries nothing, and the event
    // names neither a reason nor a status. It still needs a response object: one without
    // never latches.
    const EV_BARE_INCOMPLETE: &str =
        r#"{"type":"response.incomplete","response":{"id":"resp_1","model":"stream-model"}}"#;
    let (got, err) = run_stream(&[EV_CREATED, EV_ARGS_DONE_UNNAMED, EV_BARE_INCOMPLETE]).await;
    if let Some(err) = err {
        panic!("streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert!(
        !carries_content(Some(final_resp)),
        "final carries content {:?}, so this is not the case being asserted",
        final_resp.content
    );
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Other),
        "final FinishReason"
    );
    assert_eq!(final_resp.error_code, "OTHER", "final ErrorCode");
    assert_eq!(final_resp.error_message, "incomplete", "final ErrorMessage");
}

/// Logprobs are only reported against the text they belong to.
#[tokio::test]
async fn test_model_generate_stream_logprobs_describe_the_answer() {
    let other_delta = r#"{"type":"response.output_text.delta","delta":"other"}"#;
    // (name, events, text, logprobs present)
    let tests: Vec<(&str, Vec<&str>, &str, bool)> = vec![
        // Deltas the completed snapshot does not restate: it cannot supersede them, so the
        // answer stays what streamed and the logprobs do not belong to it.
        (
            "delta arrives after the terminal output and disagrees",
            vec![EV_CREATED, EV_COMPLETED, other_delta],
            "other",
            false,
        ),
        // Reasoning is not part of the answer the logprobs describe.
        (
            "thoughts do not count against the answer",
            vec![
                EV_CREATED,
                EV_REASONING,
                EV_DELTA1,
                EV_DELTA2,
                EV_COMPLETED_REASONING,
            ],
            "thinkinghello",
            true,
        ),
    ];
    for (name, events, want_text, want_logprobs) in tests {
        let (got, err) = run_stream(&events).await;
        if let Some(err) = err {
            panic!("{name}: GenerateContent() stream err = {err}");
        }
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            all_text(final_resp.content.as_ref()),
            want_text,
            "{name}: final text"
        );
        assert_eq!(
            final_resp.logprobs_result.is_some(),
            want_logprobs,
            "{name}: final LogprobsResult = {:?}: they describe \"hello\"",
            final_resp.logprobs_result
        );
    }
}

/// An event omitting the response object the schema marks required does not close the turn.
#[tokio::test]
async fn test_model_generate_stream_terminal_event_without_response() {
    // "a well-formed event later in the turn still wins"
    let (got, err) = run_stream(&[
        EV_CREATED,
        EV_DELTA1,
        EV_DELTA2,
        EV_BARE_COMPLETED,
        EV_COMPLETED,
    ])
    .await;
    if let Some(err) = err {
        panic!("a well-formed event later in the turn still wins: stream err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.model_version, "stream-model",
        "final ModelVersion"
    );
    assert_eq!(
        final_resp
            .usage_metadata
            .as_ref()
            .map(|u| u.total_token_count),
        Some(7),
        "final UsageMetadata: want the terminal event's 7 total tokens"
    );
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Stop),
        "final FinishReason"
    );

    // "alone it says nothing about why the turn ended"
    let (got, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, EV_BARE_COMPLETED]).await;
    if let Some(err) = err {
        panic!("alone it says nothing about why the turn ended: stream err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Unspecified),
        "final FinishReason"
    );
    assert_eq!(final_resp.usage_metadata, None, "final UsageMetadata");
}

/// A turn the aggregator emptied is rebuilt from the terminal event rather than closed as a
/// clean stop with nothing in it.
#[tokio::test]
async fn test_model_generate_stream_empty_aggregate_uses_terminal_event() {
    // "a tool call"
    let ev_completed_tool_call = terminal("response.completed", BODY_TOOL_CALL);
    let (streamed, err) =
        run_stream(&[EV_CREATED, EV_ARGS_DONE_UNNAMED, &ev_completed_tool_call]).await;
    if let Some(err) = err {
        panic!("a tool call: streaming err = {err}");
    }
    let (blocking, err) = run_blocking(BODY_TOOL_CALL).await;
    if let Some(err) = err {
        panic!("a tool call: blocking err = {err}");
    }
    assert_eq!(blocking.len(), 1, "a tool call: blocking emitted responses");
    let want: Vec<FunctionCall> = vec![call("get_weather", "call_1", json!({"city": "SF"}))];
    assert_eq!(
        function_calls(&blocking[0]),
        want,
        "a tool call: blocking function calls"
    );
    assert_eq!(
        function_calls(assert_turn_shape(&streamed)),
        want,
        "a tool call: streamed function calls"
    );

    // "a whole message the deltas never carried": text has no adoption path of its own.
    let (streamed, err) = run_stream(&[EV_CREATED, EV_ARGS_DONE_UNNAMED, EV_COMPLETED]).await;
    if let Some(err) = err {
        panic!("a whole message the deltas never carried: streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&streamed);
    assert_eq!(
        all_text(final_resp.content.as_ref()),
        "hello",
        "streamed text: want the terminal event's"
    );
    assert!(
        final_resp.logprobs_result.is_some(),
        "streamed LogprobsResult = nil, want the terminal event's logprobs for that text"
    );
}

/// A stream that announces a response and then produces nothing yields no turn at all;
/// blocking fails with `ErrNoOutputItems` on the same body (an asymmetry recorded, not
/// endorsed).
#[tokio::test]
async fn test_model_generate_stream_created_only() {
    let (got, err) = run_stream(&[EV_CREATED]).await;
    if let Some(err) = err {
        panic!("GenerateContent() stream err = {err}");
    }
    assert_eq!(got.len(), 0, "stream emitted responses, want none");
    let (_, err) = run_blocking(r#"{"id":"resp_1","model":"stream-model"}"#).await;
    assert!(
        err.as_ref().is_some_and(|e| e.is(ErrorKind::NoOutputItems)),
        "blocking err = {err:?}, want ErrNoOutputItems"
    );
}

/// A consumer that stops mid-turn ends the stream and the upstream response is torn down.
// parity: adapted: Go's closeSpy counts Close calls on the response body after a `break`; gadk cancels by drop (D9), so the stream is dropped mid-turn and the server sees the abandoned body (`FakeServer::drops`).
#[tokio::test]
async fn test_model_generate_stream_stops_when_the_consumer_stops() {
    // The body stays open after the first delta, so the consumer stops mid-turn with the
    // response still open.
    let resp = FakeResponse::sse(sse_data(&[EV_CREATED, EV_DELTA1])).hang();
    let server = new_loopback_server(move |_| resp.clone());
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));
    let mut items = llm.generate_content(user_request("World?"), true);
    let mut got = 0;
    if let Some(item) = items.next().await {
        let resp = item.unwrap_or_else(|err| panic!("GenerateContent() stream err = {err}"));
        got += 1;
        assert!(
            !resp.turn_complete,
            "turn completed before the consumer stopped, so the break is untested"
        );
    }
    assert_eq!(got, 1, "consumer saw responses before breaking");
    drop(items);
    server.wait_for_drops(1).await;
    assert_eq!(
        server.drops(),
        1,
        "upstream body abandoned after the consumer stopped"
    );
}

/// A turn the aggregator emptied still reports why it ended when the terminal event has
/// nothing to rebuild from.
#[tokio::test]
async fn test_model_generate_stream_outputless_terminal_event_keeps_the_reason() {
    // EV_ARGS_DONE_UNNAMED is dropped in aggregation, so the turn exists but holds nothing;
    // EV_MAX_TOKENS carries no output.
    let (got, err) = run_stream(&[EV_CREATED, EV_ARGS_DONE_UNNAMED, EV_MAX_TOKENS]).await;
    if let Some(err) = err {
        panic!("streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::MaxTokens),
        "final FinishReason"
    );

    // With nothing in the aggregator either, the call fails the way blocking does.
    let (_, err) = run_stream(&[EV_CREATED, EV_MAX_TOKENS]).await;
    assert!(
        err.as_ref().is_some_and(|e| e.is(ErrorKind::NoOutputItems)),
        "streaming err = {err:?}, want ErrNoOutputItems"
    );
}

/// An event carrying an empty response object is no more terminal than one omitting it.
#[tokio::test]
async fn test_model_generate_stream_terminal_event_with_empty_response() {
    const EV_EMPTY_RESPONSE: &str = r#"{"type":"response.completed","response":{}}"#;
    let (got, err) = run_stream(&[
        EV_CREATED,
        EV_DELTA1,
        EV_DELTA2,
        EV_EMPTY_RESPONSE,
        EV_COMPLETED,
    ])
    .await;
    if let Some(err) = err {
        panic!("streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.model_version, "stream-model",
        "final ModelVersion"
    );
    assert_eq!(
        final_resp
            .usage_metadata
            .as_ref()
            .map(|u| u.total_token_count),
        Some(7),
        "final UsageMetadata: want the well-formed event's 7 total tokens"
    );
}

/// A response that decoded but omits `id` still closes the turn.
#[tokio::test]
async fn test_model_generate_stream_terminal_event_without_id() {
    // "output batched onto the terminal event still reaches the caller"
    const EV_IDLESS_BATCHED: &str = r#"{"type":"response.completed","response":{"model":"stream-model","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}}"#;
    let (got, err) = run_stream(&[EV_IDLESS_BATCHED]).await;
    if let Some(err) = err {
        panic!("output batched onto the terminal event: streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        all_text(final_resp.content.as_ref()),
        "hello",
        "streamed text: want the terminal event's"
    );
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Stop),
        "final FinishReason"
    );

    // "a turn cut short still reports why"
    const EV_IDLESS_FILTERED: &str = r#"{"type":"response.incomplete","response":{"model":"stream-model","status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}}"#;
    let (got, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, EV_IDLESS_FILTERED]).await;
    if let Some(err) = err {
        panic!("a turn cut short still reports why: streaming err = {err}");
    }
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Safety),
        "final FinishReason"
    );
}

/// A tool call the terminal event carries reaches the caller even when the deltas already
/// built the turn's text.
#[tokio::test]
async fn test_model_generate_stream_adopts_terminal_tool_calls() {
    const BODY_TEXT_AND_CALL: &str = r#"{"id":"resp_1","model":"stream-model","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]},{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}]}"#;
    let ev_text_and_call = terminal("response.completed", BODY_TEXT_AND_CALL);

    let (streamed, err) = run_stream(&[
        EV_CREATED,
        EV_DELTA1,
        EV_DELTA2,
        EV_ARGS_DONE_UNNAMED,
        &ev_text_and_call,
    ])
    .await;
    if let Some(err) = err {
        panic!("streaming err = {err}");
    }
    let (blocking, err) = run_blocking(BODY_TEXT_AND_CALL).await;
    if let Some(err) = err {
        panic!("blocking err = {err}");
    }
    assert_eq!(blocking.len(), 1, "blocking emitted responses");
    let final_resp = assert_turn_shape(&streamed);
    assert_eq!(
        all_text(final_resp.content.as_ref()),
        "hello",
        "streamed text: want the deltas'"
    );
    let want: Vec<FunctionCall> = vec![call("get_weather", "call_1", json!({"city": "SF"}))];
    assert_eq!(
        function_calls(&blocking[0]),
        want,
        "blocking function calls"
    );
    assert_eq!(function_calls(final_resp), want, "streamed function calls");

    // "a call the deltas already carry is not duplicated"
    const EV_NAMED_ARGS_DONE: &str = r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","name":"get_weather","call_id":"call_1"}}"#;
    let (got, err) = run_stream(&[
        EV_CREATED,
        EV_NAMED_ARGS_DONE,
        EV_ARGS_DONE_UNNAMED,
        &ev_text_and_call,
    ])
    .await;
    if let Some(err) = err {
        panic!("a call the deltas already carry is not duplicated: streaming err = {err}");
    }
    assert_eq!(
        function_calls(assert_turn_shape(&got)),
        want,
        "a call the deltas already carry is not duplicated: streamed function calls"
    );

    // "unusable arguments fail the call as they do unstreamed"
    const BODY_BAD_ARGS: &str = r#"{"id":"resp_1","model":"stream-model","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]},{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{"}]}"#;
    let ev_bad_args = terminal("response.completed", BODY_BAD_ARGS);
    let (_, err) = run_stream(&[EV_CREATED, EV_DELTA1, EV_DELTA2, &ev_bad_args]).await;
    assert!(
        err.as_ref()
            .is_some_and(|e| e.is(ErrorKind::FunctionCallArgs)),
        "unusable arguments: streaming err = {err:?}, want ErrFunctionCallArgs"
    );
    let (_, err) = run_blocking(BODY_BAD_ARGS).await;
    assert!(
        err.as_ref()
            .is_some_and(|e| e.is(ErrorKind::FunctionCallArgs)),
        "unusable arguments: blocking err = {err:?}, want ErrFunctionCallArgs"
    );
}

// Not ports of Go tests: they pin the streaming error texts, which Go reports unwrapped (no
// `openai: call failed: ` prefix; ADK-Go yields the stream's error as it is).

/// A streaming request answered with HTTP 500 (after openai-go's two retries) yields
/// openai-go's API error text, unwrapped.
#[tokio::test(start_paused = true)]
async fn stream_server_error_text_is_unwrapped() {
    let fake = ScriptedClient::always(FakeResponse::json_status(
        500,
        r#"{"error":{"message":"boom"}}"#,
    ));
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_scripted(&fake));
    let (got, err) = collect(llm.as_ref(), user_request("hi"), true).await;
    assert!(got.is_empty(), "{got:?}");
    let Some(err) = err else {
        panic!("want the 500 as an error");
    };
    assert_eq!(
        err.to_string(),
        "OpenAI API error: 500 Internal Server Error"
    );
    assert_eq!(err.status(), Some(500));
    assert_eq!(fake.hits(), 3, "attempts (openai-go retries twice)");
}

/// `http_options.timeout` expiring while the body streams yields exactly Go's
/// `context deadline exceeded`.
#[tokio::test(start_paused = true)]
async fn stream_timeout_mid_stream_text_is_context_deadline_exceeded() {
    let fake = ScriptedClient::always(
        FakeResponse::sse(&format!("data: {}\n\ndata: {}\n\n", EV_CREATED, EV_DELTA1)).hang(),
    );
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_scripted(&fake));
    let req = gadk_model::LlmRequest {
        http_options: Some(gadk_model::HttpOptions {
            timeout: Some(std::time::Duration::from_secs(5)),
            ..gadk_model::HttpOptions::default()
        }),
        ..user_request("hi")
    };
    let (got, err) = collect(llm.as_ref(), req, true).await;
    assert!(
        got.iter().any(|r| r.partial),
        "a partial before the deadline: {got:?}"
    );
    let Some(err) = err else {
        panic!("want the deadline as an error");
    };
    assert_eq!(err.to_string(), "context deadline exceeded");
    assert!(err.is_deadline_exceeded());
}
