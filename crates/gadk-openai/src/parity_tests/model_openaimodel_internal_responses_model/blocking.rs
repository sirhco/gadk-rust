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

//! `responses/model_test.go`: the blocking tests (Task 9; see `mod.rs`).

use gadk_genai::{Content, FunctionCall, Part, ROLE_MODEL};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;

use super::{
    BODY_COMPLETED, BODY_FAILED_EMPTY, BODY_FAILED_NO_STATUS, BODY_FAILED_PARTIAL, EV_COMPLETED,
    EV_CREATED, EV_CREATED_FAILED, EV_DELTA1, EV_FAILED,
};
use crate::error::ErrorKind;
use crate::parity_tests::support::{
    FakeResponse, TEST_MODEL, TestClientConfig, all_text, collect, new_loopback_server,
    new_test_model, run_blocking, run_stream, serve_events, serve_json, user_request,
};

/// A function call named `name` with call ID `id` and the arguments object `args` (Go
/// `&genai.FunctionCall{Name: name, ID: id, Args: args}`).
pub(super) fn call(name: &str, id: &str, args: serde_json::Value) -> FunctionCall {
    FunctionCall {
        name: name.to_string(),
        id: id.to_string(),
        args: match args {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        },
        ..FunctionCall::default()
    }
}

/// `{"type":"<kind>","response":<body>}`: a terminal event carrying `body`.
pub(super) fn terminal(kind: &str, body: &str) -> String {
    format!(r#"{{"type":"{kind}","response":{body}}}"#)
}

#[tokio::test]
async fn test_model_generate() {
    let server = new_loopback_server(|_| {
        serve_json(
            r#"{"id":"resp_123","model":"test-model","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}"#,
        )
    });
    let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));
    let (got, err) = collect(llm.as_ref(), user_request("World?"), false).await;
    if let Some(err) = err {
        panic!("GenerateContent() err = {err}");
    }
    let text: String = got.iter().map(|r| all_text(r.content.as_ref())).collect();
    assert_eq!(text, "hello", "response text mismatch");
    for req in server.requests() {
        assert_eq!(req.path, "/v1/responses", "unexpected path");
    }
}

/// Pins every way a failure can arrive to one outcome: an error naming what the server said,
/// and no response claiming the turn finished. Streamed, it need not come as
/// `response.failed`: an ordinary terminal event can carry it, after the deltas have gone out.
#[tokio::test]
async fn test_model_failed_status() {
    let completed_partial = terminal("response.completed", BODY_FAILED_PARTIAL);
    let completed_empty = terminal("response.completed", BODY_FAILED_EMPTY);
    let incomplete_partial = terminal("response.incomplete", BODY_FAILED_PARTIAL);
    let completed_no_status = terminal("response.completed", BODY_FAILED_NO_STATUS);
    // (name, blocking body, stream events, text delivered before the failure)
    let tests: Vec<(&str, &str, Vec<&str>, &str)> = vec![
        ("blocking with no output", BODY_FAILED_EMPTY, vec![], ""),
        (
            "blocking with partial output",
            BODY_FAILED_PARTIAL,
            vec![],
            "",
        ),
        (
            "streamed as response.failed",
            "",
            vec![EV_CREATED, EV_DELTA1, EV_FAILED],
            "hel",
        ),
        (
            "streamed in the terminal body after deltas",
            "",
            vec![EV_CREATED, EV_DELTA1, &completed_partial],
            "hel",
        ),
        // Nothing to deliver, so the failure is all there is, and it must still be reported.
        (
            "streamed in the terminal body with no deltas",
            "",
            vec![EV_CREATED, &completed_empty],
            "",
        ),
        (
            "streamed in an incomplete body",
            "",
            vec![EV_CREATED, EV_DELTA1, &incomplete_partial],
            "hel",
        ),
        // Announced on `response.created` and never taken back.
        (
            "announced at creation, no terminal event",
            "",
            vec![EV_CREATED_FAILED, EV_DELTA1],
            "hel",
        ),
        // No status anywhere, so the error object is the only thing saying this is not an
        // answer.
        (
            "error object with no status at all",
            BODY_FAILED_NO_STATUS,
            vec![],
            "",
        ),
        (
            "error object with no status at all, streamed",
            "",
            vec![EV_CREATED, EV_DELTA1, &completed_no_status],
            "hel",
        ),
    ];
    for (name, body, events, want_delivered) in tests {
        let streamed = !events.is_empty();
        let (got, err) = if streamed {
            run_stream(&events).await
        } else {
            run_blocking(body).await
        };
        let Some(err) = err else {
            panic!(
                "{name}: GenerateContent() err = nil, want errors.Is(err, shared.ErrResponseFailed)"
            );
        };
        assert!(
            err.is(ErrorKind::ResponseFailed),
            "{name}: GenerateContent() err = {err}, want errors.Is(err, shared.ErrResponseFailed)"
        );
        for want in ["upstream exploded", "resp_123", "server_error"] {
            assert!(
                err.to_string().contains(want),
                "{name}: GenerateContent() err = {:?}, want it to mention {want:?}",
                err.to_string()
            );
        }
        let mut delivered = String::new();
        for (i, resp) in got.iter().enumerate() {
            delivered.push_str(&all_text(resp.content.as_ref()));
            // Nothing handed over may say the turn ended well.
            assert!(
                !resp.turn_complete,
                "{name}: response {i} has turn_complete = true, want the failure to end the turn instead"
            );
        }
        assert_eq!(
            delivered, want_delivered,
            "{name}: text delivered before the failure"
        );
    }
}

/// One failed body reads the same whole as it does at the end of a stream.
#[tokio::test]
async fn test_model_failed_status_paths_agree() {
    let (_, blocking) = run_blocking(BODY_FAILED_PARTIAL).await;
    let completed = terminal("response.completed", BODY_FAILED_PARTIAL);
    let (_, streamed) = run_stream(&[EV_CREATED, EV_DELTA1, &completed]).await;
    let (Some(blocking), Some(streamed)) = (blocking, streamed) else {
        panic!("both paths must fail");
    };
    assert_eq!(blocking.to_string(), streamed.to_string(), "paths disagree");
}

/// Asserts the drop on the bytes that leave the process, on both code paths.
#[tokio::test]
async fn test_model_generate_content_does_not_send_replayed_reasoning() {
    let contents = vec![
        Content::from_text("what is 2+2?", "user"),
        Content {
            role: ROLE_MODEL.to_string(),
            parts: vec![
                Part {
                    text: "the user must not see this scratchpad".to_string(),
                    thought: true,
                    ..Part::default()
                },
                Part::from_text("4"),
            ],
        },
        Content::from_text("and 3+3?", "user"),
    ];
    for (name, stream) in [("blocking", false), ("streaming", true)] {
        let server = new_loopback_server(move |_| {
            if stream {
                serve_events(&[EV_CREATED, EV_COMPLETED])
            } else {
                serve_json(BODY_COMPLETED)
            }
        });
        let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));
        let req = LlmRequest {
            contents: contents.clone(),
            ..LlmRequest::default()
        };
        let (_, err) = collect(llm.as_ref(), req, stream).await;
        if let Some(err) = err {
            panic!("{name}: GenerateContent() err = {err}");
        }
        let body = server.last_request().body_text();
        assert!(
            !body.contains("scratchpad"),
            "{name}: request body replayed the model's reasoning:\n{body}"
        );
        // The answer survived, so the test cannot pass by sending nothing at all.
        for want in ["what is 2+2?", r#""4""#, "and 3+3?"] {
            assert!(
                body.contains(want),
                "{name}: request body is missing {want:?}:\n{body}"
            );
        }
    }
}

/// When the drop empties the request there is nothing to send, and the caller learns that
/// instead of the model being called with nothing.
#[tokio::test]
async fn test_model_generate_content_thought_only_request_fails_before_sending() {
    for (name, stream) in [("blocking", false), ("streaming", true)] {
        let server = new_loopback_server(|_| FakeResponse::new(200));
        let llm = new_test_model(TEST_MODEL, &TestClientConfig::for_server(&server));
        let req = LlmRequest {
            contents: vec![Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![Part {
                    text: "still thinking".to_string(),
                    thought: true,
                    ..Part::default()
                }],
            }],
            ..LlmRequest::default()
        };
        let (_, err) = collect(llm.as_ref(), req, stream).await;
        assert!(
            err.as_ref().is_some_and(|e| e.is(ErrorKind::NoContents)),
            "{name}: GenerateContent() err = {err:?}, want it to wrap ErrNoContents"
        );
        assert_eq!(
            server.requests().len(),
            0,
            "{name}: model was called with an emptied request"
        );
    }
}
