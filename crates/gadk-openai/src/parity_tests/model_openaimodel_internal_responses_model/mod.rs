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

//! Port of ADK-Go `model/openaimodel/internal/responses/model_test.go`, a directory target
//! (Task 9), split by Go source order:
//! - `blocking`: Go lines 39–176 and 2302–end (`TestModel_Generate`, `TestModel_FailedStatus*`,
//!   `TestModel_GenerateContent_*`);
//! - `stream_terminal`: Go lines 178–1121;
//! - `stream_content`: Go lines 1122–2301.
//!
//! The synthetic stream events and bodies the Go file shares between its tests (its `const`
//! block and `incompleteEvent`) are here; the shared helpers (`runStream`, `runBlocking`,
//! `assertTurnShape`, …) are in [`super::support`].

mod blocking;
mod stream_content;
mod stream_terminal;

macro_rules! evcreated {
    () => {
        r#"{"type":"response.created","response":{"id":"resp_1","model":"stream-model","status":"in_progress"}}"#
    };
}
macro_rules! evdelta1 {
    () => {
        r#"{"type":"response.output_text.delta","delta":"hel"}"#
    };
}
macro_rules! evdelta2 {
    () => {
        r#"{"type":"response.output_text.delta","delta":"lo"}"#
    };
}
macro_rules! evcompleted {
    () => {
        concat!(
            r#"{"type":"response.completed","response":"#,
            bodycompleted!(),
            r#"}"#
        )
    };
}
macro_rules! evmaxtokens {
    () => {
        r#"{"type":"response.incomplete","response":{"id":"resp_1","model":"stream-model","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#
    };
}
macro_rules! evfiltered {
    () => {
        r#"{"type":"response.incomplete","response":{"id":"resp_1","model":"stream-model","status":"incomplete","incomplete_details":{"reason":"content_filter"}}}"#
    };
}
macro_rules! evfailed {
    () => {
        concat!(
            r#"{"type":"response.failed","response":"#,
            bodyfailedempty!(),
            r#"}"#
        )
    };
}
macro_rules! everror {
    () => {
        r#"{"type":"error","message":"stream blew up"}"#
    };
}
macro_rules! evreasoning {
    () => {
        r#"{"type":"response.reasoning_text.delta","delta":"thinking"}"#
    };
}
macro_rules! evcompletedreasoning {
    () => {
        concat!(
            r#"{"type":"response.completed","response":"#,
            bodycompletedreasoning!(),
            r#"}"#
        )
    };
}
macro_rules! evbarecompleted {
    () => {
        r#"{"type":"response.completed"}"#
    };
}
macro_rules! evtruncated {
    () => {
        r#"{"type":"response.output_text.delta","delta":"#
    };
}
macro_rules! evargsdoneunnamed {
    () => {
        r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"city\":\"SF\"}"}"#
    };
}
macro_rules! evcompletednostatus {
    () => {
        r#"{"type":"response.completed","response":{"id":"resp_1","model":"stream-model","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}}"#
    };
}
macro_rules! evcreatedfailed {
    () => {
        concat!(
            r#"{"type":"response.created","response":"#,
            bodyfailedempty!(),
            r#"}"#
        )
    };
}
macro_rules! bodycompleted {
    () => {
        r#"{"id":"resp_1","model":"stream-model","status":"completed","usage":{"input_tokens":3,"output_tokens":4,"total_tokens":7},"output":[{"type":"message","content":[{"type":"output_text","text":"hello","logprobs":[{"token":"hello","logprob":-0.5,"top_logprobs":[{"token":"hello","logprob":-0.5}]}]}]}]}"#
    };
}
macro_rules! bodycompletedreasoning {
    () => {
        r#"{"id":"resp_1","model":"stream-model","status":"completed","usage":{"input_tokens":3,"output_tokens":4,"total_tokens":7},"output":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"thinking"}]},{"type":"message","content":[{"type":"output_text","text":"hello","logprobs":[{"token":"hello","logprob":-0.5,"top_logprobs":[{"token":"hello","logprob":-0.5}]}]}]}]}"#
    };
}
macro_rules! bodyfiltered {
    () => {
        r#"{"id":"resp_1","model":"stream-model","status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}"#
    };
}
macro_rules! bodytoolcall {
    () => {
        r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}]}"#
    };
}
macro_rules! bodyfailedempty {
    () => {
        r#"{"id":"resp_123","model":"stream-model","status":"failed","error":{"code":"server_error","message":"upstream exploded"},"output":[]}"#
    };
}
macro_rules! bodyfailedpartial {
    () => {
        r#"{"id":"resp_123","model":"stream-model","status":"failed","error":{"code":"server_error","message":"upstream exploded"},"output":[{"type":"message","content":[{"type":"output_text","text":"hel"}]}]}"#
    };
}
macro_rules! bodyfailednostatus {
    () => {
        r#"{"id":"resp_123","model":"stream-model","error":{"code":"server_error","message":"upstream exploded"},"output":[{"type":"message","content":[{"type":"output_text","text":"hel"}]}]}"#
    };
}

/// Go `evCreated`.
pub(super) const EV_CREATED: &str = evcreated!();
/// Go `evDelta1`.
pub(super) const EV_DELTA1: &str = evdelta1!();
/// Go `evDelta2`.
pub(super) const EV_DELTA2: &str = evdelta2!();
/// Go `evCompleted`.
pub(super) const EV_COMPLETED: &str = evcompleted!();
/// Go `evMaxTokens`.
pub(super) const EV_MAX_TOKENS: &str = evmaxtokens!();
/// Go `evFiltered`.
pub(super) const EV_FILTERED: &str = evfiltered!();
/// Go `evFailed`.
pub(super) const EV_FAILED: &str = evfailed!();
/// Go `evError`.
pub(super) const EV_ERROR: &str = everror!();
/// Go `evReasoning`.
pub(super) const EV_REASONING: &str = evreasoning!();
/// Go `evCompletedReasoning`.
pub(super) const EV_COMPLETED_REASONING: &str = evcompletedreasoning!();
/// Go `evBareCompleted`.
pub(super) const EV_BARE_COMPLETED: &str = evbarecompleted!();
/// Go `evTruncated`.
pub(super) const EV_TRUNCATED: &str = evtruncated!();
/// Go `evArgsDoneUnnamed`.
pub(super) const EV_ARGS_DONE_UNNAMED: &str = evargsdoneunnamed!();
/// Go `evCompletedNoStatus`.
pub(super) const EV_COMPLETED_NO_STATUS: &str = evcompletednostatus!();
/// Go `evCreatedFailed`.
pub(super) const EV_CREATED_FAILED: &str = evcreatedfailed!();
/// Go `bodyCompleted`.
pub(super) const BODY_COMPLETED: &str = bodycompleted!();
/// Go `bodyCompletedReasoning`.
#[allow(dead_code)] // Go's const block; like Go, only `EV_COMPLETED_REASONING` uses the body (via its macro).
pub(super) const BODY_COMPLETED_REASONING: &str = bodycompletedreasoning!();
/// Go `bodyFiltered`.
pub(super) const BODY_FILTERED: &str = bodyfiltered!();
/// Go `bodyToolCall`.
pub(super) const BODY_TOOL_CALL: &str = bodytoolcall!();
/// Go `bodyFailedEmpty`.
pub(super) const BODY_FAILED_EMPTY: &str = bodyfailedempty!();
/// Go `bodyFailedPartial`.
pub(super) const BODY_FAILED_PARTIAL: &str = bodyfailedpartial!();
/// Go `bodyFailedNoStatus`.
pub(super) const BODY_FAILED_NO_STATUS: &str = bodyfailednostatus!();

/// Go `incompleteEvent(fields)`: a `response.incomplete` whose response carries `fields`
/// (each ending in `,`) ahead of the model's partial output.
pub(super) fn incomplete_event(fields: &str) -> String {
    format!(
        r#"{{"type":"response.incomplete","response":{{"id":"resp_1","model":"stream-model",{fields}"output":[{{"type":"message","content":[{{"type":"output_text","text":"hello"}}]}}]}}}}"#
    )
}

#[test]
fn shared_events_are_json() {
    for (name, raw) in [
        ("EV_CREATED", EV_CREATED),
        ("EV_COMPLETED", EV_COMPLETED),
        ("EV_FAILED", EV_FAILED),
        ("EV_COMPLETED_REASONING", EV_COMPLETED_REASONING),
        ("EV_CREATED_FAILED", EV_CREATED_FAILED),
        ("EV_ARGS_DONE_UNNAMED", EV_ARGS_DONE_UNNAMED),
        ("BODY_TOOL_CALL", BODY_TOOL_CALL),
        ("BODY_FAILED_NO_STATUS", BODY_FAILED_NO_STATUS),
    ] {
        assert!(
            serde_json::from_str::<serde_json::Value>(raw).is_ok(),
            "{name}: {raw}"
        );
    }
    assert!(serde_json::from_str::<serde_json::Value>(EV_TRUNCATED).is_err());
    let ev: serde_json::Value =
        serde_json::from_str(&incomplete_event(r#""status":"incomplete","#)).unwrap_or_default();
    assert_eq!(ev["response"]["status"], "incomplete");
    assert_eq!(ev["response"]["output"][0]["content"][0]["text"], "hello");
}
