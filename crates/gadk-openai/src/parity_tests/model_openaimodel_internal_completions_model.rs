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

//! Port of ADK-Go `model/openaimodel/internal/completions/model_test.go` (Task 8).
//!
//! Go's `testRig`, `serveJSON`, `serveSSE`, `ask`, `responseText`, `onlyCall`,
//! `newLoopbackServer` and `newTestModel` are [`super::support`]'s `TestRig`, `serve_json`,
//! `serve_sse`, `ask`, `response_text`, `only_call`, `new_loopback_server` and
//! `new_completions_test_model`.

use std::time::Duration;

use futures::StreamExt as _;
use gadk_genai::{Content, FinishReason, GenerateContentConfig};
use gadk_model::{HttpOptions, LlmRequest, LlmResponse};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::support::{
    FakeResponse, ScriptedClient, TestClientConfig, TestRig, WAIT_TIMEOUT, ask, collect, only_call,
    response_text, serve_json, serve_sse, test_model, user_request,
};
use crate::completions::stream::TOOL_CALL_CHUNK_REJECTED;
use crate::config::Api;
use crate::error::{ErrorKind, OpenAiError};

/// `ask`, failing the test on an error.
async fn ask_ok(rig: &TestRig, stream: bool) -> Vec<LlmResponse> {
    match ask(rig.model().as_ref(), stream).await {
        (got, None) => got,
        (_, Some(err)) => panic!("GenerateContent() err = {err}"),
    }
}

/// `ask`, failing the test unless it yields an error.
async fn ask_err(rig: &TestRig, stream: bool) -> (Vec<LlmResponse>, OpenAiError) {
    match ask(rig.model().as_ref(), stream).await {
        (got, Some(err)) => (got, err),
        (got, None) => panic!("GenerateContent() = {got:?}, want an error"),
    }
}

/// The last response, failing the test when there is none.
fn last(got: &[LlmResponse]) -> &LlmResponse {
    match got.last() {
        Some(resp) => resp,
        None => panic!("no response yielded"),
    }
}

/// A "hi" request whose `http_options.timeout` is `timeout` (Go
/// `Config.HTTPOptions.Timeout`).
fn hi_with_timeout(timeout: Duration) -> LlmRequest {
    LlmRequest {
        contents: vec![Content::from_text("hi", "user")],
        config: Some(GenerateContentConfig::default()),
        http_options: Some(HttpOptions {
            timeout: Some(timeout),
            ..HttpOptions::default()
        }),
        ..LlmRequest::default()
    }
}

/// The error of a run of `req`, failing the test when there is none (Go's loop keeps the
/// last error; the model yields at most one).
async fn run_err(rig: &TestRig, req: LlmRequest, stream: bool) -> OpenAiError {
    match collect(rig.model().as_ref(), req, stream).await {
        (_, Some(err)) => err,
        (got, None) => panic!("GenerateContent() = {got:?}, want an error"),
    }
}

// parity: skipped TestModel_GenerateContent_NilRequest: gadk's `generate_content` takes an `LlmRequest` by value, so there is no nil request (Contract compile fix 7).

#[tokio::test]
async fn test_model_generate_content_text() {
    let rig = TestRig::new(|_| {
        serve_json(
            r#"{"id":"c1","model":"gpt-4o-mini",
		"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"sunny"}}],
		"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
        )
    });
    let got = ask_ok(&rig, false).await;
    assert_eq!(got.len(), 1, "responses");
    assert_eq!(response_text(&got[0]), "sunny", "text");
    assert_eq!(
        got[0].custom_metadata.get("openai_response_id"),
        Some(&json!("c1")),
        "response id metadata"
    );
    // Recorded as a plain string, which is what this key has always been documented to
    // carry.
    assert!(
        matches!(
            got[0].custom_metadata.get("openai_model"),
            Some(Value::String(_))
        ),
        "model metadata = {:?}, want string",
        got[0].custom_metadata.get("openai_model")
    );
}

// parity: adapted: served by a ScriptedClient under paused tokio time, because openai-go retries a 429 twice with backoff; errors.As(*openai.Error) is OpenAiError::status()
#[tokio::test(start_paused = true)]
async fn test_model_generate_content_server_error() {
    let fake = ScriptedClient::always(FakeResponse::json_status(
        429,
        r#"{"error":{"message":"slow down"}}"#,
    ));
    let llm = test_model(
        Api::ChatCompletions,
        super::support::TEST_MODEL,
        &TestClientConfig::for_scripted(&fake),
    );
    let (_, err) = ask(llm.as_ref(), false).await;
    let Some(err) = err else {
        panic!("err = nil, want the provider's 429 surfaced as an API error");
    };
    assert_eq!(
        err.status(),
        Some(429),
        "err = {err}, want the provider's 429 surfaced as an API error"
    );
    assert_eq!(
        err.to_string(),
        "openai: call failed: OpenAI API error: 429 Too Many Requests"
    );
    assert_eq!(fake.hits(), 3, "attempts (openai-go retries twice)");
}

#[tokio::test]
async fn test_model_generate_stream_text_deltas() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hail "}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"at -7 C"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    assert!(
        got.len() >= 2,
        "responses = {}, want partials plus a final",
        got.len()
    );
    let final_resp = last(&got);
    assert!(
        final_resp.turn_complete,
        "last response does not close the turn"
    );
    assert_eq!(response_text(final_resp), "hail at -7 C", "final text");
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Stop),
        "finish reason"
    );
    // Usage arrives only on the trailing chunk, and only because the request asked for it.
    assert_eq!(
        final_resp
            .usage_metadata
            .as_ref()
            .map(|u| u.total_token_count),
        Some(12),
        "usage = {:?}, want 12 total tokens",
        final_resp.usage_metadata
    );
    let requests = rig.requests();
    assert!(
        requests[0].contains(r#""include_usage":true"#),
        "request did not ask for streamed usage: {}",
        requests[0]
    );
}

/// Covers the shape this endpoint has no event for: arguments arrive in fragments and the
/// call is complete only once the stream ends.
#[tokio::test]
async fn test_model_generate_stream_tool_arguments_across_chunks() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Lisbon\"}"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    let final_resp = last(&got);
    let call = final_resp
        .content
        .as_ref()
        .and_then(|c| c.parts.iter().rev().find_map(|p| p.function_call.clone()));
    let Some(call) = call else {
        panic!(
            "no function call on the final response: {:?}",
            final_resp.content
        );
    };
    assert_eq!(
        (call.name.as_str(), call.id.as_str()),
        ("get_weather", "call_1"),
        "call = {call:?}"
    );
    assert_eq!(
        call.args.get("city"),
        Some(&json!("Lisbon")),
        "args = {:?}, want the fragments reassembled",
        call.args
    );
}

#[tokio::test]
async fn test_model_generate_stream_refusal_deltas() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","refusal":"I cannot "}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"refusal":"help"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    assert_eq!(
        response_text(last(&got)),
        "I cannot help",
        "final text, want the refusal as text"
    );
}

#[tokio::test]
async fn test_model_generate_stream_empty_stream() {
    let rig = TestRig::new(|_| serve_sse(&[]));
    let (_, err) = ask_err(&rig, true).await;
    assert!(
        err.is(ErrorKind::NoChoices),
        "err = {err}, want {}",
        ErrorKind::NoChoices
    );
}

/// Covers a provider that resends the running usage on every chunk. Summing those reports
/// would count the prompt once per chunk.
#[tokio::test]
async fn test_model_generate_stream_usage_is_the_latest_report() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hail "}}],"usage":{"prompt_tokens":8,"completion_tokens":1,"total_tokens":9}}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"at -7 C"}}],"usage":{"prompt_tokens":8,"completion_tokens":3,"total_tokens":11}}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    let usage = last(&got).usage_metadata.as_ref();
    assert_eq!(
        usage.map(|u| (
            u.prompt_token_count,
            u.candidates_token_count,
            u.total_token_count
        )),
        Some((8, 4, 12)),
        "usage = {usage:?}, want the last report: 8 prompt, 4 candidates, 12 total"
    );
}

/// Covers a provider that reports no usage, as one ignoring `stream_options.include_usage`
/// does. Zeros would report a turn that did real work as having cost nothing. The streamed
/// tool-call turn matters most: its final response is rebuilt from the snapshot, whose usage
/// is zero.
#[tokio::test]
async fn test_model_no_usage_reported_leaves_usage_unset() {
    for (name, frames) in [
        (
            "text",
            vec![
                r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"sunny"}}]}"#,
                r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            ],
        ),
        (
            "tool call",
            vec![
                r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{}"}}]}}]}"#,
                r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ],
        ),
    ] {
        let resp = serve_sse(&frames);
        let rig = TestRig::new(move |_| resp.clone());
        let got = ask_ok(&rig, true).await;
        let usage = &last(&got).usage_metadata;
        assert!(
            usage.is_none(),
            "{name}: usage = {usage:?}, want None when the provider reported none"
        );
    }
    // Blocking agrees, so one provider does not read differently by mode.
    let name = "blocking";
    let rig = TestRig::new(|_| {
        serve_json(
            r#"{"id":"c","model":"m","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"sunny"}}]}"#,
        )
    });
    let got = ask_ok(&rig, false).await;
    let usage = &got[0].usage_metadata;
    assert!(
        usage.is_none(),
        "{name}: usage = {usage:?}, want None when the provider reported none"
    );
}

/// Pins that a turn whose streamed text survived still fails when its tool call cannot be
/// read, because only the snapshot states the calls and blocking rejects the same body.
#[tokio::test]
async fn test_model_generate_stream_unparseable_call_fails_as_blocking() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"looking"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
    });
    let (_, err) = ask_err(&rig, true).await;
    assert!(
        err.is(ErrorKind::FunctionCallArgs),
        "err = {err}, want {}",
        ErrorKind::FunctionCallArgs
    );
}

/// Covers a delta the accumulator refuses, here for a choice index past its bound, while the
/// text it carried still reached the caller as a partial.
#[tokio::test]
async fn test_model_generate_stream_empty_snapshot_keeps_streamed_text() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":500,"delta":{"role":"assistant","content":"sunny"}}]}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    let final_resp = last(&got);
    assert_eq!(
        response_text(final_resp),
        "sunny",
        "final text, want the streamed text"
    );
    // Nothing states why the turn ended, which must not read as a clean stop.
    assert_eq!(
        final_resp.finish_reason,
        Some(FinishReason::Unspecified),
        "finish reason"
    );
}

/// Covers a provider that gives each chunk of one stream its own id. The accumulator refuses
/// a chunk whose id is not the first one's, which would lose the call's arguments, or the
/// call.
#[tokio::test]
async fn test_model_generate_stream_chunk_i_ds_differ() {
    struct Case {
        name: &'static str,
        frames: Vec<&'static str>,
        want_text: &'static str,
        want_id: &'static str,
    }
    for tt in [
        Case {
            name: "arguments under changing ids",
            frames: vec![
                r#"{"id":"a1","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
                r#"{"id":"a2","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
                r#"{"id":"a3","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}"#,
                r#"{"id":"a4","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ],
            want_text: "",
            want_id: "a1",
        },
        // Opens with a chunk that has no id, so the id to keep is the first non-empty one
        // rather than the first one.
        Case {
            name: "text then a call under a new id",
            frames: vec![
                r#"{"id":"","object":"chat.completion.chunk","model":"m","choices":[]}"#,
                r#"{"id":"a1","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"looking"}}]}"#,
                r#"{"id":"a2","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]}}]}"#,
                r#"{"id":"a3","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ],
            want_text: "looking",
            want_id: "a1",
        },
    ] {
        let name = tt.name;
        let resp = serve_sse(&tt.frames);
        let rig = TestRig::new(move |_| resp.clone());
        let got = ask_ok(&rig, true).await;
        let final_resp = last(&got);
        assert_eq!(
            response_text(final_resp),
            tt.want_text,
            "{name}: final text"
        );
        assert_eq!(
            final_resp.custom_metadata.get("openai_response_id"),
            Some(&json!(tt.want_id)),
            "{name}: response id, want {:?} from the first non-empty chunk id",
            tt.want_id
        );
        let call = only_call(final_resp);
        assert_eq!(call.id, "call_1", "{name}: call = {call:?}");
        assert_eq!(
            call.args.get("city"),
            Some(&json!("Paris")),
            "{name}: call = {call:?}, want call_1 with city Paris"
        );
        assert_eq!(
            final_resp.finish_reason,
            Some(FinishReason::Stop),
            "{name}: finish reason"
        );
    }
}

/// Pins that a tool-call delta the accumulator refuses fails the turn, since only the
/// snapshot states the calls and dropping one would pass the turn off as whole.
// parity: adapted: errToolCallChunkRejected is a plain error (no ErrorKind), so errors.Is is a comparison with its text
#[tokio::test]
async fn test_model_generate_stream_rejected_tool_call_chunk_fails() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"looking"}}]}"#,
            // A tool-call index this far ahead grows the choice past the bound the
            // accumulator allows in one step.
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":1000,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{}"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
    });
    let (got, err) = ask_err(&rig, true).await;
    assert_eq!(err.to_string(), TOOL_CALL_CHUNK_REJECTED);
    for resp in &got {
        assert!(
            !resp.turn_complete,
            "a response closed the turn without its call: {:?}",
            resp.content
        );
    }
}

/// Covers a snapshot whose text cannot replace the streamed text, because the refusal and
/// content deltas interleaved. The streamed text stands, but the calls exist only in the
/// snapshot, so they must still reach the final response.
#[tokio::test]
async fn test_model_generate_stream_unsuperseded_snapshot_keeps_calls() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","refusal":"no"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"x"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Lisbon\"}"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    let final_resp = last(&got);
    assert_eq!(
        response_text(final_resp),
        "nox",
        "final text, want the streamed text"
    );
    let call = only_call(final_resp);
    assert_eq!(call.id, "call_1", "call = {call:?}");
    assert_eq!(
        call.args.get("city"),
        Some(&json!("Lisbon")),
        "call = {call:?}, want call_1 with city Lisbon"
    );
}

/// Covers a stream whose first tool-call delta uses index 1. The accumulator pads index 0
/// with an empty entry, which must not reach the caller as a call with no name.
#[tokio::test]
async fn test_model_generate_stream_sparse_tool_index() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":1,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Lisbon\"}"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
    });
    let got = ask_ok(&rig, true).await;
    let final_resp = last(&got);
    let n = final_resp.content.as_ref().map_or(0, |c| c.parts.len());
    assert_eq!(n, 1, "parts, want only the get_weather call");
    let call = only_call(final_resp);
    assert_eq!(
        (call.id.as_str(), call.name.as_str()),
        ("call_1", "get_weather"),
        "call = {call:?}, want call_1 to get_weather"
    );
}

#[tokio::test]
async fn test_model_generate_stream_error_mid_stream() {
    let rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"hail "}}]}"#,
            r#"{"error":{"message":"overloaded","type":"server_error"}}"#,
        ])
    });
    let (got, err) = ask_err(&rig, true).await;
    assert_eq!(
        err.to_string(),
        r#"received error while streaming: {"message":"overloaded","type":"server_error"}"#
    );
    assert!(
        !got.is_empty() && response_text(&got[0]) == "hail ",
        "responses = {}, want the partial that streamed before the error",
        got.len()
    );
    for resp in &got {
        assert!(
            !resp.turn_complete,
            "a response closed the turn, but the stream failed"
        );
    }
}

/// Pins that a consumer leaving the stream releases the connection rather than leaving the
/// provider streaming into a reader that is gone.
// parity: adapted: Go's range break is dropping the stream (D9); the handler's wait on r.Context().Done() is a body that never ends, released when the server sees it dropped
#[tokio::test]
async fn test_model_generate_stream_early_break_closes_the_stream() {
    let rig = TestRig::new(|_| {
        FakeResponse::sse(
            "data: {\"id\":\"c\",\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hail \"}}]}\n\n",
        )
        .hang()
    });
    let llm = rig.model();
    let mut items = llm.generate_content(user_request("hi"), true);
    while let Some(item) = items.next().await {
        match item {
            Ok(resp) if resp.partial => break,
            Ok(_) => {}
            Err(err) => panic!("GenerateContent() err = {err}"),
        }
    }
    drop(items);
    let released = tokio::time::timeout(WAIT_TIMEOUT, rig.server.wait_for_drops(1)).await;
    assert!(
        released.is_ok(),
        "the connection was still open after the consumer stopped"
    );
}

// parity: adapted: Go's 1ns context deadline is http_options.timeout = 1ns (CallTimeout::Call, D7); errors.Is(context.DeadlineExceeded) is is_deadline_exceeded()
#[tokio::test]
async fn test_model_generate_stream_honours_timeout() {
    let rig = TestRig::new(|_| FakeResponse::never());
    let err = run_err(&rig, hi_with_timeout(Duration::from_nanos(1)), true).await;
    assert!(
        err.is_deadline_exceeded(),
        "err = {err}, want a deadline error"
    );
}

/// The guard that keeps the two paths from drifting: the same turn must read the same
/// whether it streamed.
#[tokio::test]
async fn test_model_generate_stream_matches_blocking() {
    let blocking_rig = TestRig::new(|_| {
        serve_json(
            r#"{"id":"c","model":"m",
		"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"looking",
		"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Lisbon\"}"}}]}}],
		"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#,
        )
    });
    let stream_rig = TestRig::new(|_| {
        serve_sse(&[
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"looking"}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Lisbon\"}"}}]}}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"c","model":"m","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}"#,
        ])
    });

    let blocking = ask_ok(&blocking_rig, false).await;
    let streamed = ask_ok(&stream_rig, true).await;

    let (want, got) = (&blocking[0], last(&streamed));
    assert_eq!(response_text(want), response_text(got), "text");
    assert_eq!(want.finish_reason, got.finish_reason, "finish reason");
    assert_eq!(
        want.usage_metadata.as_ref().map(|u| u.total_token_count),
        got.usage_metadata.as_ref().map(|u| u.total_token_count),
        "usage"
    );
    let (want_call, got_call) = (only_call(want), only_call(got));
    assert_eq!(
        (&want_call.name, &want_call.id),
        (&got_call.name, &got_call.id),
        "call"
    );
    assert_eq!(
        want_call.args.get("city"),
        got_call.args.get("city"),
        "args"
    );
}

// parity: adapted: Go's 1ns context deadline is http_options.timeout = 1ns (CallTimeout::Call, D7); errors.Is(context.DeadlineExceeded) is is_deadline_exceeded()
#[tokio::test]
async fn test_model_honours_timeout() {
    let rig = TestRig::new(|_| FakeResponse::never());
    let err = run_err(&rig, hi_with_timeout(Duration::from_nanos(1)), false).await;
    assert!(
        err.is_deadline_exceeded(),
        "err = {err}, want a deadline error"
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
    let llm = test_model(
        Api::ChatCompletions,
        super::support::TEST_MODEL,
        &TestClientConfig::for_scripted(&fake),
    );
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
        FakeResponse::sse(
            "data: {\"id\":\"c\",\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hail \"}}]}\n\n",
        )
        .hang(),
    );
    let llm = test_model(
        Api::ChatCompletions,
        super::support::TEST_MODEL,
        &TestClientConfig::for_scripted(&fake),
    );
    let (got, err) = collect(llm.as_ref(), hi_with_timeout(Duration::from_secs(5)), true).await;
    assert_eq!(got.len(), 1, "the partial before the deadline: {got:?}");
    assert!(got[0].partial);
    let Some(err) = err else {
        panic!("want the deadline as an error");
    };
    assert_eq!(err.to_string(), "context deadline exceeded");
    assert!(err.is_deadline_exceeded());
}
