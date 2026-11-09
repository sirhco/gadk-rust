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

//! `responses/model_test.go`: the stream_content tests (see `mod.rs`).

use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL};
use gadk_model::LlmResponse;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::blocking::{call, terminal};
use super::{
    BODY_COMPLETED, EV_COMPLETED, EV_CREATED, EV_DELTA1, EV_DELTA2, EV_ERROR, EV_FAILED,
    EV_FILTERED, EV_TRUNCATED,
};
use crate::error::ErrorKind;
use crate::parity_tests::support::{
    all_text, assert_turn_shape, function_calls, part_kinds, run_blocking, run_stream,
};
use crate::responses::consts::{
    RESPONSE_REASONING_SUMMARY_TEXT_DELTA, RESPONSE_REASONING_TEXT_DELTA,
};

/// The responses a stream of `events` yields; panics (naming `name`) on an error.
async fn streamed_ok(name: &str, events: &[&str]) -> Vec<LlmResponse> {
    let (got, err) = run_stream(events).await;
    if let Some(err) = err {
        panic!("{name}: streaming err = {err}");
    }
    got
}

/// The last response the blocking path yields for `body`; panics (naming `name`) on an error
/// or an empty result.
async fn blocking_ok(name: &str, body: &str) -> LlmResponse {
    let (got, err) = run_blocking(body).await;
    if let Some(err) = err {
        panic!("{name}: blocking err = {err}");
    }
    got.into_iter()
        .last()
        .unwrap_or_else(|| panic!("{name}: blocking emitted no response"))
}

/// The one response the blocking path yields for `body` (Go's `len(blocking) != 1` check).
async fn blocking_one(name: &str, body: &str) -> LlmResponse {
    let (got, err) = run_blocking(body).await;
    if let Some(err) = err {
        panic!("{name}: blocking err = {err}");
    }
    assert_eq!(got.len(), 1, "{name}: blocking emitted responses");
    got.into_iter()
        .next()
        .unwrap_or_else(|| panic!("{name}: blocking emitted no response"))
}

/// `v` as string slices.
fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// A text part (Go `&genai.Part{Text: text, Thought: thought}`).
fn text_part(text: &str, thought: bool) -> Part {
    Part {
        text: text.to_string(),
        thought,
        ..Part::default()
    }
}

/// The terminal event's calls stand in for what streamed rather than adding to it.
#[tokio::test]
async fn test_model_generate_stream_terminal_tool_calls_are_authoritative() {
    const EV_ADDED_NAME_ONLY: &str = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","name":"get_weather"}}"#;
    const EV_ADDED_OTHER_ID: &str = r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","name":"get_weather","call_id":"call_A"}}"#;
    const EV_ARGS_DONE_SF: &str = r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"city\":\"SF\"}"}"#;
    const CALL_SF: &str = r#"{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}"#;
    let completed_with = |output: &str| {
        format!(
            r#"{{"type":"response.completed","response":{{"id":"resp_1","model":"stream-model","status":"completed","output":[{output}]}}}}"#
        )
    };
    let ev_one_call = completed_with(CALL_SF);
    let sf = || call("get_weather", "call_1", json!({"city": "SF"}));
    let ny = || call("get_weather", "call_2", json!({"city": "NY"}));
    let want_sf = vec![sf()];

    // The three shapes in which the streamed call cannot be paired with the terminal item,
    // each of which appended a second copy of the one call.
    let tests: Vec<(&str, Vec<&str>)> = vec![
        (
            "the added event carries no call_id",
            vec![
                EV_CREATED,
                EV_ADDED_NAME_ONLY,
                EV_ARGS_DONE_SF,
                &ev_one_call,
            ],
        ),
        (
            "no added event at all",
            vec![EV_CREATED, EV_ARGS_DONE_SF, &ev_one_call],
        ),
        (
            "the added event and the terminal item disagree",
            vec![EV_CREATED, EV_ADDED_OTHER_ID, EV_ARGS_DONE_SF, &ev_one_call],
        ),
    ];
    for (name, events) in tests {
        let got = streamed_ok(name, &events).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            want_sf,
            "{name}: streamed function calls"
        );
    }

    // Two calls stream and the terminal event names one.
    const EV_ADDED1: &str = r#"{"type":"response.output_item.added","item":{"id":"f1","type":"function_call","name":"get_weather","call_id":"call_1"}}"#;
    const EV_ADDED2: &str = r#"{"type":"response.output_item.added","item":{"id":"f2","type":"function_call","name":"get_weather","call_id":"call_2"}}"#;
    const EV_ARGS1: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f1","arguments":"{\"city\":\"SF\"}"}"#;
    const EV_ARGS2: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f2","arguments":"{\"city\":\"NY\"}"}"#;
    let two_streamed = [EV_CREATED, EV_ADDED1, EV_ARGS1, EV_ADDED2, EV_ARGS2];
    let with = |extra: &[&str]| -> Vec<String> {
        two_streamed
            .iter()
            .chain(extra)
            .map(|s| s.to_string())
            .collect()
    };

    {
        let name = "a completed response states the turn's whole output";
        let events = with(&[&ev_one_call]);
        let got = streamed_ok(name, &refs(&events)).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            want_sf,
            "{name}: streamed function calls"
        );
    }

    {
        let name = "an incomplete response may name fewer than streamed";
        let ev_cut_short = format!(
            r#"{{"type":"response.incomplete","response":{{"id":"resp_1","model":"stream-model","status":"incomplete","incomplete_details":{{"reason":"max_output_tokens"}},"output":[{CALL_SF}]}}}}"#
        );
        let events = with(&[&ev_cut_short]);
        let got = streamed_ok(name, &refs(&events)).await;
        // The shorter list is the truncation showing, not the provider's count.
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![sf(), ny()],
            "{name}: streamed function calls"
        );
    }

    {
        // Replacing must not reorder: blocking reports the call first for the same body.
        let name = "the event's calls keep the place the turn's calls held";
        let ev_call_then_text = completed_with(&format!(
            r#"{CALL_SF},{{"type":"message","content":[{{"type":"output_text","text":"hello"}}]}}"#
        ));
        let got = streamed_ok(
            name,
            &[
                EV_CREATED,
                EV_ADDED_NAME_ONLY,
                EV_ARGS_DONE_SF,
                EV_DELTA1,
                EV_DELTA2,
                &ev_call_then_text,
            ],
        )
        .await;
        assert_eq!(
            part_kinds(assert_turn_shape(&got)),
            vec!["call", "text"],
            "{name}: part order"
        );
    }

    {
        // Two calls straddling the text: each stated call takes its streamed one's place.
        let name = "a turn that interleaved calls with text stays interleaved";
        let body = format!(
            r#"{{"id":"resp_1","model":"stream-model","status":"completed","output":[{CALL_SF},{{"type":"message","content":[{{"type":"output_text","text":"hello"}}]}},{{"type":"function_call","name":"get_weather","call_id":"call_2","arguments":"{{\"city\":\"NY\"}}"}}]}}"#
        );
        let completed = terminal("response.completed", &body);
        let streamed = streamed_ok(
            name,
            &[
                EV_CREATED, EV_ADDED1, EV_ARGS1, EV_DELTA1, EV_DELTA2, EV_ADDED2, EV_ARGS2,
                &completed,
            ],
        )
        .await;
        let final_resp = assert_turn_shape(&streamed);
        let blocked = blocking_ok(name, &body).await;
        assert_eq!(
            part_kinds(final_resp),
            part_kinds(&blocked),
            "{name}: streaming and blocking disagree on part order"
        );
        // Still the event's own calls, in the event's order.
        assert_eq!(
            function_calls(final_resp),
            vec![sf(), ny()],
            "{name}: streamed function calls"
        );
    }

    {
        // Nothing survives aggregation, so the turn is rebuilt from the event as blocking
        // builds it and adoption then runs over that rebuild.
        let name = "a rebuilt turn keeps the shape blocking gives the same body";
        let body = format!(
            r#"{{"id":"resp_1","model":"stream-model","status":"completed","output":[{CALL_SF},{{"type":"message","content":[{{"type":"output_text","text":"hello"}}]}}]}}"#
        );
        let completed = terminal("response.completed", &body);
        let streamed = streamed_ok(name, &[EV_CREATED, &completed]).await;
        let blocked = blocking_ok(name, &body).await;
        assert_eq!(
            part_kinds(assert_turn_shape(&streamed)),
            part_kinds(&blocked),
            "{name}: streaming and blocking disagree on part order"
        );
    }

    {
        // The event lists the same two calls the other way round; the flow dispatches tools
        // in part order.
        let name = "the event's order is the turn's, not the order they streamed in";
        let ev_reversed = completed_with(
            r#"{"type":"function_call","name":"get_weather","call_id":"call_2","arguments":"{\"city\":\"NY\"}"},{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}"#,
        );
        let events = with(&[&ev_reversed]);
        let got = streamed_ok(name, &refs(&events)).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![ny(), sf()],
            "{name}: streamed function calls"
        );
    }

    {
        // A call with no name anywhere is dropped in aggregation; taking the event's list
        // whole recovers it ahead of the second.
        let name = "a call the aggregator dropped keeps the event's order";
        const EV_UNNAMED_ARGS: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f0","arguments":"{\"n\":1}"}"#;
        const BOTH_IN_ORDER: &str = r#"{"type":"function_call","name":"first","call_id":"call_0","arguments":"{\"n\":1}"},{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}"#;
        let ev_both_in_order = completed_with(BOTH_IN_ORDER);
        let got = streamed_ok(
            name,
            &[
                EV_CREATED,
                EV_UNNAMED_ARGS,
                EV_ADDED1,
                EV_ARGS1,
                &ev_both_in_order,
            ],
        )
        .await;
        let streamed = function_calls(assert_turn_shape(&got));
        assert_eq!(
            streamed,
            vec![call("first", "call_0", json!({"n": 1})), sf()],
            "{name}: streamed function calls"
        );
        // The same body blocking, since the event's list is what both report.
        let blocked = blocking_ok(
            name,
            &format!(
                r#"{{"id":"resp_1","model":"stream-model","status":"completed","output":[{BOTH_IN_ORDER}]}}"#
            ),
        )
        .await;
        assert_eq!(
            function_calls(&blocked),
            streamed,
            "{name}: streaming and blocking disagree on the turn's calls"
        );
    }

    {
        // Arguments no caller can read lose what unstated ones lose; blocking still rejects
        // these bytes because they are all it has.
        let name = "an unusable item keeps the call the deltas built";
        const BAD_ARGS: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{"}]}"#;
        let completed = terminal("response.completed", BAD_ARGS);
        let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            want_sf,
            "{name}: streamed function calls"
        );
        let (_, err) = run_blocking(BAD_ARGS).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.is(ErrorKind::FunctionCallArgs)),
            "{name}: blocking err = {err:?}, want ErrFunctionCallArgs"
        );
    }

    {
        // Nothing pairs it, so there is no readable copy to keep.
        let name = "an unusable item no delta restates fails the turn";
        const BAD_ARGS: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"},{"type":"function_call","name":"lookup","call_id":"call_9","arguments":"{"}]}"#;
        let completed = terminal("response.completed", BAD_ARGS);
        let (_, err) = run_stream(&[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.is(ErrorKind::FunctionCallArgs)),
            "{name}: streaming err = {err:?}, want ErrFunctionCallArgs"
        );
        let (_, err) = run_blocking(BAD_ARGS).await;
        assert!(
            err.as_ref()
                .is_some_and(|e| e.is(ErrorKind::FunctionCallArgs)),
            "{name}: blocking err = {err:?}, want ErrFunctionCallArgs"
        );
    }

    {
        // The event is authoritative for which calls a turn made, not for every field of
        // each.
        let group = "a field the item leaves unstated keeps what streamed";
        let with_args = vec![sf()];
        let no_args = vec![call("get_weather", "call_1", json!({}))];
        let tests: Vec<(&str, &str, &Vec<FunctionCall>)> = vec![
            (
                "arguments omitted",
                r#"{"type":"function_call","name":"get_weather","call_id":"call_1"}"#,
                &with_args,
            ),
            (
                "arguments null",
                r#"{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":null}"#,
                &with_args,
            ),
            (
                "name omitted, leaving a call nothing can dispatch",
                r#"{"type":"function_call","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}"#,
                &with_args,
            ),
            // A stated field is never second-guessed: "{}" is a call that takes none.
            (
                "empty arguments are stated, not unstated",
                r#"{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{}"}"#,
                &no_args,
            ),
            // The provider saying the same thing badly, but still saying it.
            (
                "an empty string is stated too",
                r#"{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":""}"#,
                &no_args,
            ),
        ];
        for (name, item, want) in tests {
            let completed = completed_with(item);
            let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
            assert_eq!(
                &function_calls(assert_turn_shape(&got)),
                want,
                "{group}/{name}: streamed function calls"
            );
        }
    }

    {
        // More calls than the turn holds are placed as a group where the turn's own call
        // sat.
        let name = "a call that streamed before the text is still reported before it";
        const BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"},{"type":"function_call","name":"get_weather","call_id":"call_2","arguments":"{\"city\":\"NY\"}"},{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}"#;
        let completed = terminal("response.completed", BODY);
        let got = streamed_ok(
            name,
            &[
                EV_CREATED, EV_ADDED1, EV_ARGS1, EV_DELTA1, EV_DELTA2, &completed,
            ],
        )
        .await;
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            part_kinds(final_resp),
            vec!["call", "call", "text"],
            "{name}: part order"
        );
        let blocked = blocking_ok(name, BODY).await;
        assert_eq!(
            part_kinds(final_resp),
            part_kinds(&blocked),
            "{name}: streaming and blocking disagree on part order"
        );
    }

    {
        // The counts differ, but the call IDs agree, so restoring still happens.
        let name = "an unstated field is restored when the event states more calls than streamed";
        let completed = completed_with(
            r#"{"type":"function_call","name":"get_weather","call_id":"call_1"},{"type":"function_call","name":"get_weather","call_id":"call_2","arguments":"{\"city\":\"NY\"}"}"#,
        );
        let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![sf(), ny()],
            "{name}: streamed function calls"
        );
    }

    {
        // Pairing by slot would hand each the arguments the model wrote for the other; the
        // call ID says which.
        let name = "each call is restored from the one it restates, not the one in its place";
        let completed = completed_with(
            r#"{"type":"function_call","name":"get_weather","call_id":"call_2"},{"type":"function_call","name":"get_weather","call_id":"call_1"}"#,
        );
        let events = with(&[&completed]);
        let got = streamed_ok(name, &refs(&events)).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![ny(), sf()],
            "{name}: streamed function calls"
        );
    }

    const EV_ADDED_ALPHA: &str = r#"{"type":"response.output_item.added","item":{"id":"f1","type":"function_call","name":"alpha","call_id":"call_1"}}"#;
    const EV_ARGS_ALPHA: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f1","arguments":"{\"x\":1}"}"#;
    const EV_ADDED_BETA: &str = r#"{"type":"response.output_item.added","item":{"id":"f2","type":"function_call","name":"beta","call_id":"call_2"}}"#;
    const EV_ARGS_BETA: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f2","arguments":"{\"y\":2}"}"#;

    {
        // The same reordering with no call ID on either item: position would swap the two
        // tools' arguments.
        let name = "the tool's name pairs reordered calls the event does not identify";
        let completed = completed_with(
            r#"{"type":"function_call","name":"beta"},{"type":"function_call","name":"alpha"}"#,
        );
        let got = streamed_ok(
            name,
            &[
                EV_CREATED,
                EV_ADDED_ALPHA,
                EV_ARGS_ALPHA,
                EV_ADDED_BETA,
                EV_ARGS_BETA,
                &completed,
            ],
        )
        .await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![
                call("beta", "call_2", json!({"y": 2})),
                call("alpha", "call_1", json!({"x": 1})),
            ],
            "{name}: streamed function calls"
        );
    }

    {
        // The event crosses the two calls' IDs; trusting the ID alone hands beta alpha's
        // arguments.
        let name = "a call id the names contradict does not pair";
        let completed = completed_with(
            r#"{"type":"function_call","name":"beta","call_id":"call_1"},{"type":"function_call","name":"alpha","call_id":"call_2"}"#,
        );
        let got = streamed_ok(
            name,
            &[
                EV_CREATED,
                EV_ADDED_ALPHA,
                EV_ARGS_ALPHA,
                EV_ADDED_BETA,
                EV_ARGS_BETA,
                &completed,
            ],
        )
        .await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![
                call("beta", "call_1", json!({"y": 2})),
                call("alpha", "call_2", json!({"x": 1})),
            ],
            "{name}: streamed function calls"
        );
    }

    {
        // The name narrows to both, so position is all that is left.
        let name = "two calls to one tool fall back to position";
        let completed = completed_with(
            r#"{"type":"function_call","name":"get_weather"},{"type":"function_call","name":"get_weather"}"#,
        );
        let events = with(&[&completed]);
        let got = streamed_ok(name, &refs(&events)).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![sf(), ny()],
            "{name}: streamed function calls"
        );
    }

    {
        // A provider that omits call_id on the item leaves the caller nothing to match the
        // tool's result back to; an item that states one still outranks what streamed.
        let group = "an item stating no call id keeps the one the turn reported";
        let tests = [
            (
                "omitted",
                r#"{"type":"function_call","name":"get_weather","arguments":"{\"city\":\"SF\"}"}"#,
                "call_1",
            ),
            (
                "stated, and it wins",
                r#"{"type":"function_call","name":"get_weather","call_id":"call_9","arguments":"{\"city\":\"SF\"}"}"#,
                "call_9",
            ),
        ];
        for (name, item, want_id) in tests {
            let completed = completed_with(item);
            let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
            assert_eq!(
                function_calls(assert_turn_shape(&got)),
                vec![call("get_weather", want_id, json!({"city": "SF"}))],
                "{group}/{name}: streamed function calls"
            );
        }
    }

    {
        // Both items resolve to the single call that streamed; restoring onto both would
        // report two calls under one ID. (Go also checks the two calls do not share one
        // arguments map; gadk's calls own their maps.)
        let name = "a streamed call restores onto one item only";
        let completed = completed_with(
            r#"{"type":"function_call","name":"get_weather"},{"type":"function_call","name":"get_weather"}"#,
        );
        let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![sf(), call("get_weather", "", json!({}))],
            "{name}: streamed function calls"
        );
    }

    {
        // The second item states call_1 for itself, so lending it to the first would report
        // two calls under it.
        let name = "an id another item claims is not lent away";
        let completed = completed_with(
            r#"{"type":"function_call","name":"get_weather"},{"type":"function_call","name":"lookup","call_id":"call_1","arguments":"{\"q\":\"x\"}"}"#,
        );
        let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![
                call("get_weather", "", json!({"city": "SF"})),
                call("lookup", "call_1", json!({"q": "x"})),
            ],
            "{name}: streamed function calls"
        );
    }

    {
        // Position alone would dispatch a different tool on the input the model wrote for
        // get_weather.
        let name = "position does not hand one tool another's arguments";
        let completed =
            completed_with(r#"{"type":"function_call","name":"delete_files","call_id":"call_1"}"#);
        let got = streamed_ok(name, &[EV_CREATED, EV_ADDED1, EV_ARGS1, &completed]).await;
        assert_eq!(
            function_calls(assert_turn_shape(&got)),
            vec![call("delete_files", "call_1", json!({}))],
            "{name}: streamed function calls"
        );
    }

    const EV_UNNAMED_SF: &str = r#"{"type":"response.function_call_arguments.done","item_id":"f0","arguments":"{\"city\":\"SF\"}"}"#;

    {
        // The turn holds no call, so the event's own order is the only one either path has.
        let name = "the event places a call no aggregated turn held";
        const BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"},{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}"#;
        let completed = terminal("response.completed", BODY);
        let got = streamed_ok(
            name,
            &[EV_CREATED, EV_UNNAMED_SF, EV_DELTA1, EV_DELTA2, &completed],
        )
        .await;
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            part_kinds(final_resp),
            vec!["call", "text"],
            "{name}: part order"
        );
        let blocked = blocking_ok(name, BODY).await;
        assert_eq!(
            part_kinds(final_resp),
            part_kinds(&blocked),
            "{name}: streaming and blocking disagree on part order"
        );
    }

    {
        // An event leading with a message leaves the calls at the end.
        let name = "a call the event states after its text stays after it";
        const BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]},{"type":"function_call","name":"get_weather","call_id":"call_1","arguments":"{\"city\":\"SF\"}"}]}"#;
        let completed = terminal("response.completed", BODY);
        let got = streamed_ok(
            name,
            &[EV_CREATED, EV_UNNAMED_SF, EV_DELTA1, EV_DELTA2, &completed],
        )
        .await;
        let final_resp = assert_turn_shape(&got);
        let blocked = blocking_ok(name, BODY).await;
        assert_eq!(
            part_kinds(final_resp),
            part_kinds(&blocked),
            "{name}: streaming and blocking disagree on part order"
        );
    }
}

/// A refusal counts towards the answer the logprobs describe.
#[tokio::test]
async fn test_model_generate_stream_refusal_logprobs_match_blocking() {
    const BODY_REFUSAL: &str = r#"{"id":"resp_1","model":"stream-model","output":[{"type":"message","content":[{"type":"output_text","text":"hello","logprobs":[{"token":"hello","logprob":-0.5}]},{"type":"refusal","refusal":"I cannot"}]}]}"#;
    let completed = terminal("response.completed", BODY_REFUSAL);
    let streamed = streamed_ok("refusal", &[EV_CREATED, &completed]).await;
    let blocking = blocking_one("refusal", BODY_REFUSAL).await;
    for (path, got) in [
        ("streamed", assert_turn_shape(&streamed)),
        ("blocking", &blocking),
    ] {
        assert_eq!(
            all_text(got.content.as_ref()),
            "helloI cannot",
            "{path} text"
        );
        assert!(
            got.logprobs_result.is_some(),
            "{path} LogprobsResult = nil, want the logprobs for the text it carries"
        );
    }
}

/// One model output reports the same terminal fields whether or not it streamed.
#[tokio::test]
async fn test_model_generate_stream_matches_blocking() {
    let streamed = streamed_ok("matches", &[EV_CREATED, EV_DELTA1, EV_DELTA2, EV_COMPLETED]).await;
    let want = blocking_one("matches", BODY_COMPLETED).await;
    let got = assert_turn_shape(&streamed);
    assert_eq!(got.finish_reason, want.finish_reason, "FinishReason");
    assert_eq!(got.logprobs_result, want.logprobs_result, "LogprobsResult");
    assert_eq!(got.usage_metadata, want.usage_metadata, "UsageMetadata");
    assert_eq!(got.model_version, want.model_version, "ModelVersion");
    assert_eq!(
        all_text(got.content.as_ref()),
        all_text(want.content.as_ref()),
        "text"
    );
}

/// Non-empty streamed refusals reach callers as non-thought text and the final content
/// matches blocking output.
#[tokio::test]
async fn test_model_generate_stream_refusal() {
    const REFUSAL_ONLY_DELTA1: &str = r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"I cannot ","sequence_number":1}"#;
    const REFUSAL_ONLY_DELTA2: &str = r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"help with that.","sequence_number":2}"#;
    const REFUSAL_ONLY_DONE: &str = r#"{"type":"response.refusal.done","item_id":"msg_1","output_index":0,"content_index":0,"refusal":"I cannot help with that.","sequence_number":3}"#;
    const REFUSAL_ONLY_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"I cannot help with that."}]}]}"#;
    const EMPTY_REFUSAL_DELTA: &str = r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"","sequence_number":1}"#;
    const REASONING_DELTA: &str = r#"{"type":"response.reasoning_text.delta","item_id":"rs_1","output_index":0,"content_index":0,"delta":"Checking the request","sequence_number":1}"#;
    const MIXED_DELTA1: &str = r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":1,"content_index":0,"delta":"I cannot ","sequence_number":2}"#;
    const MIXED_DELTA2: &str = r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":1,"content_index":0,"delta":"help with that.","sequence_number":3}"#;
    const MIXED_DONE: &str = r#"{"type":"response.refusal.done","item_id":"msg_1","output_index":1,"content_index":0,"refusal":"I cannot help with that.","sequence_number":4}"#;
    const MIXED_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"content":[{"type":"reasoning_text","text":"Checking the request"}]},{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"I cannot help with that."}]}]}"#;
    let refusal_only_final = format!(
        r#"{{"type":"response.completed","sequence_number":4,"response":{REFUSAL_ONLY_BODY}}}"#
    );
    let empty_refusal_final = format!(
        r#"{{"type":"response.completed","sequence_number":2,"response":{REFUSAL_ONLY_BODY}}}"#
    );
    let mixed_final =
        format!(r#"{{"type":"response.completed","sequence_number":5,"response":{MIXED_BODY}}}"#);

    // (name, events, blocking body, streamed refusal)
    let tests: Vec<(&str, Vec<&str>, &str, &str)> = vec![
        (
            "refusal only",
            vec![
                EV_CREATED,
                REFUSAL_ONLY_DELTA1,
                REFUSAL_ONLY_DELTA2,
                REFUSAL_ONLY_DONE,
                &refusal_only_final,
            ],
            REFUSAL_ONLY_BODY,
            "I cannot help with that.",
        ),
        (
            "partial refusal completed without done",
            vec![EV_CREATED, REFUSAL_ONLY_DELTA1, &refusal_only_final],
            REFUSAL_ONLY_BODY,
            "I cannot ",
        ),
        (
            "partial refusal completed after done",
            vec![
                EV_CREATED,
                REFUSAL_ONLY_DELTA1,
                REFUSAL_ONLY_DONE,
                &refusal_only_final,
            ],
            REFUSAL_ONLY_BODY,
            "I cannot ",
        ),
        (
            "empty delta falls back to terminal response",
            vec![EV_CREATED, EMPTY_REFUSAL_DELTA, &empty_refusal_final],
            REFUSAL_ONLY_BODY,
            "",
        ),
        (
            "refusal after reasoning",
            vec![
                EV_CREATED,
                REASONING_DELTA,
                MIXED_DELTA1,
                MIXED_DELTA2,
                MIXED_DONE,
                &mixed_final,
            ],
            MIXED_BODY,
            "I cannot help with that.",
        ),
        (
            "partial refusal after reasoning",
            vec![EV_CREATED, REASONING_DELTA, MIXED_DELTA1, &mixed_final],
            MIXED_BODY,
            "I cannot ",
        ),
    ];
    for (name, events, blocking_body, want_streamed_refusal) in tests {
        let got = streamed_ok(name, &events).await;
        let final_resp = assert_turn_shape(&got);
        let blocking = blocking_one(name, blocking_body).await;
        let streamed_refusal: String = got[..got.len() - 1]
            .iter()
            .filter_map(|resp| resp.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter(|part| !part.thought)
            .map(|part| part.text.as_str())
            .collect();
        assert_eq!(
            streamed_refusal, want_streamed_refusal,
            "{name}: streamed refusal"
        );
        assert_eq!(
            final_resp.content, blocking.content,
            "{name}: content mismatch (blocking vs streamed)"
        );
    }
}

/// Reasoning deltas must not hide an answer carried only by the completed response.
#[tokio::test]
async fn test_model_generate_stream_terminal_only_message_content() {
    const REASONING_DELTA: &str = r#"{"type":"response.reasoning_text.delta","item_id":"rs_1","output_index":0,"content_index":0,"delta":"Checking the request.","sequence_number":1}"#;
    const BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"content":[{"type":"reasoning_text","text":"Checking the request."}]},{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"The final answer."}]}]}"#;
    let completed =
        format!(r#"{{"type":"response.completed","sequence_number":2,"response":{BODY}}}"#);
    let streamed = streamed_ok("terminal only", &[EV_CREATED, REASONING_DELTA, &completed]).await;
    let final_resp = assert_turn_shape(&streamed);
    let blocking = blocking_one("terminal only", BODY).await;
    assert_eq!(
        final_resp.content, blocking.content,
        "content mismatch (blocking vs streamed)"
    );
}

/// Streamed reasoning still reaches callers even when the completed response omits it from
/// the final content, matching the blocking representation.
#[tokio::test]
async fn test_model_generate_stream_completed_message_omits_streamed_reasoning() {
    for event_type in [
        RESPONSE_REASONING_TEXT_DELTA,
        RESPONSE_REASONING_SUMMARY_TEXT_DELTA,
    ] {
        let reasoning =
            format!(r#"{{"type":"{event_type}","item_id":"rs_1","delta":"Checking the request"}}"#);
        let got = streamed_ok(
            event_type,
            &[EV_CREATED, &reasoning, EV_DELTA1, EV_DELTA2, EV_COMPLETED],
        )
        .await;
        let final_resp = assert_turn_shape(&got);
        let streamed_thought: String = got[..got.len() - 1]
            .iter()
            .filter_map(|resp| resp.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter(|part| part.thought)
            .map(|part| part.text.as_str())
            .collect();
        assert_eq!(
            streamed_thought, "Checking the request",
            "{event_type}: streamed reasoning"
        );
        let blocking = blocking_one(event_type, BODY_COMPLETED).await;
        assert_eq!(
            final_resp.content,
            Some(Content::from_text("hello", ROLE_MODEL)),
            "{event_type}: final content"
        );
        assert_eq!(
            final_resp.content, blocking.content,
            "{event_type}: content mismatch (blocking vs streamed)"
        );
    }
}

#[tokio::test]
async fn test_model_generate_stream_completed_content_order() {
    const COMPLETED_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"Checking the request"}],"summary":[{"type":"summary_text","text":"Request checked"}]},{"type":"message","content":[{"type":"output_text","text":"hello"},{"type":"output_text","text":" again"}]},{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"SF\"}"},{"type":"message","content":[{"type":"refusal","refusal":"I cannot help with that."}]}]}"#;
    let completed = terminal("response.completed", COMPLETED_BODY);
    let got = streamed_ok(
        "content order",
        &[
            EV_CREATED,
            r#"{"type":"response.reasoning_text.delta","delta":"Checking"}"#,
            EV_DELTA1,
            &completed,
        ],
    )
    .await;
    let final_resp = assert_turn_shape(&got);
    let blocking = blocking_one("content order", COMPLETED_BODY).await;
    assert_eq!(
        final_resp.content, blocking.content,
        "content mismatch (blocking vs streamed)"
    );
}

#[tokio::test]
async fn test_model_generate_stream_narrower_completed_content_preserves_aggregate() {
    const REASONING_SUMMARY_DELTA: &str = r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_1","delta":"Weighing options"}"#;
    const REASONING_ONLY_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"Weighing options"}],"content":[]},{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":""}]}]}"#;
    const SHORTER_TEXT_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hel"}]}]}"#;
    const EV_ITEM_ADDED: &str = r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_1","name":"get_weather"}}"#;
    const EV_ARGS_DONE: &str = r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"city\":\"SF\"}"}"#;
    let reasoning_only = terminal("response.completed", REASONING_ONLY_BODY);
    let shorter_text = terminal("response.completed", SHORTER_TEXT_BODY);
    let reasoning_with_answer = r#"{"type":"response.completed","response":{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"content":[{"type":"reasoning_text","text":"hello is the answer"}]}]}}"#;

    // (name, events, final content)
    let tests: Vec<(&str, Vec<&str>, Content)> = vec![
        (
            "reasoning-only snapshot does not replace an answer",
            vec![
                EV_CREATED,
                REASONING_SUMMARY_DELTA,
                EV_DELTA1,
                EV_DELTA2,
                &reasoning_only,
            ],
            Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![
                    text_part("Weighing options", true),
                    text_part("hello", false),
                ],
            },
        ),
        (
            "reasoning-only snapshot containing the answer does not replace it",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, reasoning_with_answer],
            Content::from_text("hello", ROLE_MODEL),
        ),
        (
            "shorter text snapshot does not truncate an answer",
            vec![EV_CREATED, EV_DELTA1, EV_DELTA2, &shorter_text],
            Content::from_text("hello", ROLE_MODEL),
        ),
        (
            "message snapshot does not discard a streamed function call",
            vec![EV_CREATED, EV_ITEM_ADDED, EV_ARGS_DONE, EV_COMPLETED],
            Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![Part {
                    function_call: Some(call("get_weather", "call_1", json!({"city": "SF"}))),
                    ..Part::default()
                }],
            },
        ),
    ];
    for (name, events, want) in tests {
        let got = streamed_ok(name, &events).await;
        let final_resp = assert_turn_shape(&got);
        assert_eq!(final_resp.content, Some(want), "{name}: final content");
    }
}

#[tokio::test]
async fn test_model_generate_stream_completed_function_call_replaces_thought_only_aggregate() {
    const COMPLETED_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"item_1","type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"SF\"}"}]}"#;
    let completed = terminal("response.completed", COMPLETED_BODY);
    let got = streamed_ok(
        "thought only",
        &[
            EV_CREATED,
            r#"{"type":"response.reasoning_text.delta","item_id":"rs_1","delta":"Checking"}"#,
            &completed,
        ],
    )
    .await;
    let final_resp = assert_turn_shape(&got);
    let blocking = blocking_one("thought only", COMPLETED_BODY).await;
    assert_eq!(
        final_resp.content, blocking.content,
        "content mismatch (blocking vs streamed)"
    );
}

/// Both encodings describe a call with no arguments; their decoded map shape must not prevent
/// recovery of text carried only by the completed response.
#[tokio::test]
async fn test_model_generate_stream_completed_text_after_empty_function_args() {
    // Go's `%q` of each.
    for (name, quoted) in [("{}", r#""{}""#), ("null", r#""null""#)] {
        let body = format!(
            r#"{{"id":"resp_1","model":"stream-model","status":"completed","output":[{{"id":"item_1","type":"function_call","call_id":"call_1","name":"ping","arguments":{quoted}}},{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"pong"}}]}}]}}"#
        );
        let args_done = format!(
            r#"{{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":{quoted}}}"#
        );
        let completed = terminal("response.completed", &body);
        let got = streamed_ok(
            name,
            &[
                EV_CREATED,
                r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_1","name":"ping"}}"#,
                &args_done,
                &completed,
            ],
        )
        .await;
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            all_text(final_resp.content.as_ref()),
            "pong",
            "{name}: final text"
        );
        let blocking = blocking_one(name, &body).await;
        assert_eq!(
            final_resp.content, blocking.content,
            "{name}: content mismatch (blocking vs streamed)"
        );
    }
}

#[tokio::test]
async fn test_model_generate_stream_unusable_completed_content() {
    let tests = [
        ("no output items", "[]"),
        (
            "empty text",
            r#"[{"type":"message","content":[{"type":"output_text","text":""}]}]"#,
        ),
        (
            "empty refusal",
            r#"[{"type":"message","content":[{"type":"refusal","refusal":""}]}]"#,
        ),
        ("unsupported output item", r#"[{"type":"unsupported"}]"#),
        (
            "unsupported message content",
            r#"[{"type":"message","content":[{"type":"unsupported"}]}]"#,
        ),
    ];
    for (name, output) in tests {
        let completed = format!(
            r#"{{"type":"response.completed","response":{{"id":"resp_1","model":"stream-model","output":{output}}}}}"#
        );
        let got = streamed_ok(name, &[EV_CREATED, EV_DELTA1, &completed]).await;
        let final_resp = assert_turn_shape(&got);
        assert_eq!(
            final_resp.content,
            Some(Content::from_text("hel", ROLE_MODEL)),
            "{name}: final content"
        );
        assert_eq!(
            final_resp.finish_reason,
            Some(gadk_genai::FinishReason::Stop),
            "{name}: final FinishReason"
        );
    }
}

/// A stream cut off before its terminal event reports no usage rather than zero usage.
#[tokio::test]
async fn test_model_generate_stream_truncated_leaves_usage_unset() {
    let got = streamed_ok("truncated", &[EV_CREATED, EV_DELTA1, EV_DELTA2]).await;
    assert_eq!(
        assert_turn_shape(&got).usage_metadata,
        None,
        "final UsageMetadata"
    );
}

/// A terminal response carrying nothing usable fails the call, as blocking does.
#[tokio::test]
async fn test_model_generate_stream_no_output_items() {
    let (got, err) = run_stream(&[EV_CREATED, EV_FILTERED]).await;
    assert!(
        err.as_ref().is_some_and(|e| e.is(ErrorKind::NoOutputItems)),
        "streaming err = {err:?}, want ErrNoOutputItems"
    );
    assert_eq!(got.len(), 0, "stream emitted responses, want none");
    const FILTERED_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"incomplete","incomplete_details":{"reason":"content_filter"}}"#;
    let (_, err) = run_blocking(FILTERED_BODY).await;
    assert!(
        err.as_ref().is_some_and(|e| e.is(ErrorKind::NoOutputItems)),
        "blocking err = {err:?}, want ErrNoOutputItems"
    );
}

/// Every way a stream can go wrong ends the turn with that error rather than a response
/// claiming the turn completed.
#[tokio::test]
async fn test_model_generate_stream_errors_end_the_turn() {
    let tests: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "response.failed",
            vec![EV_CREATED, EV_DELTA1, EV_FAILED],
            "upstream exploded",
        ),
        (
            "error event",
            vec![EV_CREATED, EV_DELTA1, EV_ERROR],
            "stream blew up",
        ),
        (
            "translator rejects the event",
            vec![
                EV_CREATED,
                r#"{"type":"response.function_call_arguments.done","item_id":"i1","arguments":"{"}"#,
            ],
            "parse streamed function args",
        ),
        (
            "the stream itself fails",
            vec![EV_CREATED, EV_DELTA1, EV_TRUNCATED],
            "unexpected end of JSON input",
        ),
    ];
    for (name, events, want_err) in tests {
        let (got, err) = run_stream(&events).await;
        let Some(err) = err else {
            panic!("{name}: streaming err = nil, want one naming {want_err:?}");
        };
        assert!(
            err.to_string().contains(want_err),
            "{name}: streaming err = {err}, want it to name {want_err:?}"
        );
        for (i, resp) in got.iter().enumerate() {
            assert!(
                !resp.turn_complete,
                "{name}: response {i} has turn_complete = true, want the error to end the turn"
            );
        }
    }
}

/// A streamed tool call closes its turn the way a streamed message does.
#[tokio::test]
async fn test_model_generate_stream_function_call() {
    const EV_ITEM_ADDED: &str = r#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_1","name":"get_weather"}}"#;
    const EV_ARGS_DONE: &str = r#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"city\":\"SF\"}"}"#;
    const COMPLETED_BODY: &str = r#"{"id":"resp_1","model":"stream-model","status":"completed","output":[{"id":"item_1","type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"SF\"}"}]}"#;
    let completed = terminal("response.completed", COMPLETED_BODY);
    let got = streamed_ok(
        "function call",
        &[EV_CREATED, EV_ITEM_ADDED, EV_ARGS_DONE, &completed],
    )
    .await;
    let final_resp = assert_turn_shape(&got);
    assert_eq!(
        final_resp.finish_reason,
        Some(gadk_genai::FinishReason::Stop),
        "final FinishReason"
    );
    let want = vec![call("get_weather", "call_1", json!({"city": "SF"}))];
    assert_eq!(function_calls(final_resp), want, "final function calls");
    let streamed_calls: Vec<FunctionCall> = got[..got.len() - 1]
        .iter()
        .flat_map(function_calls)
        .collect();
    assert_eq!(streamed_calls, want, "streamed function calls");
    let blocking = blocking_one("function call", COMPLETED_BODY).await;
    assert_eq!(
        final_resp.content, blocking.content,
        "content mismatch (blocking vs streamed)"
    );
}
