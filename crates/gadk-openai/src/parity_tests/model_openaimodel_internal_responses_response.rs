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

//! Port of ADK-Go `model/openaimodel/internal/responses/response_test.go`.
//!
//! Go builds `oairesponses.Response` struct literals; these tests build the wire structs the
//! same way. A Go literal carries no JSON presence metadata, so an `Option` field Go reads
//! with `JSON.X.Valid()` (only `incomplete_details`, in `truncated`) is `None` unless the case
//! decodes a body; the error object is `Some` wherever Go sets one of its fields.

use gadk_genai::{
    FinishReason, FunctionCall, LogprobsResult, LogprobsResultCandidate,
    LogprobsResultTopCandidates, Part,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use serde_json::value::RawValue;

use crate::error::ErrorKind;
use crate::responses::response::{
    convert_function_call, convert_logprobs, convert_output_items, convert_response,
    finish_message, finish_reason, logprobs_for,
};
use crate::responses::wire::{
    Response, ResponseError, ResponseIncompleteDetails, ResponseOutputContent, ResponseOutputItem,
    ResponseOutputTextLogprob, ResponseOutputTextLogprobTopLogprob, ResponseReasoningItemSummary,
    ResponseUsage, status,
};

/// A `message` output item holding `content`.
fn message(content: Vec<ResponseOutputContent>) -> ResponseOutputItem {
    ResponseOutputItem {
        kind: "message".into(),
        content,
        ..ResponseOutputItem::default()
    }
}

/// An `output_text` content entry.
fn output_text(text: &str) -> ResponseOutputContent {
    ResponseOutputContent {
        kind: "output_text".into(),
        text: text.into(),
        ..ResponseOutputContent::default()
    }
}

/// A `refusal` content entry.
fn refusal(text: &str) -> ResponseOutputContent {
    ResponseOutputContent {
        kind: "refusal".into(),
        refusal: text.into(),
        ..ResponseOutputContent::default()
    }
}

/// Go `oairesponses.ResponseError{Code: code, Message: message}`.
fn response_error(code: &str, message: &str) -> Option<ResponseError> {
    Some(ResponseError {
        code: code.into(),
        message: message.into(),
        ..ResponseError::default()
    })
}

/// Go `oairesponses.ResponseOutputItemUnionArguments{OfString: s}`: arguments sent as a JSON
/// string.
fn string_arguments(s: &str) -> Option<Box<RawValue>> {
    Some(RawValue::from_string(serde_json::to_string(s).unwrap()).unwrap())
}

/// The fully specified logprobs Go's logprob tests share, and the result they convert to.
fn fully_specified_logprobs() -> (Vec<ResponseOutputTextLogprob>, LogprobsResult) {
    let top = |token: &str, logprob: f64| ResponseOutputTextLogprobTopLogprob {
        token: token.into(),
        logprob,
        ..ResponseOutputTextLogprobTopLogprob::default()
    };
    let logprobs = vec![
        ResponseOutputTextLogprob {
            token: "hel".into(),
            logprob: -0.1,
            top_logprobs: vec![top("hel", -0.1), top("hi", -2.3)],
            ..ResponseOutputTextLogprob::default()
        },
        ResponseOutputTextLogprob {
            token: "lo".into(),
            logprob: -0.2,
            ..ResponseOutputTextLogprob::default()
        },
    ];
    let cand = |token: &str, log_probability: f32| LogprobsResultCandidate {
        token: token.into(),
        log_probability,
        ..LogprobsResultCandidate::default()
    };
    let want = LogprobsResult {
        chosen_candidates: vec![cand("hel", -0.1), cand("lo", -0.2)],
        top_candidates: vec![
            LogprobsResultTopCandidates {
                candidates: vec![cand("hel", -0.1), cand("hi", -2.3)],
            },
            LogprobsResultTopCandidates { candidates: vec![] },
        ],
        ..LogprobsResult::default()
    };
    (logprobs, want)
}

#[test]
fn test_convert_response_text() {
    let resp = Response {
        id: Some("resp-1".into()),
        model: Some("gpt-test".into()),
        output: Some(vec![message(vec![output_text("hello")])]),
        usage: ResponseUsage {
            input_tokens: 5,
            output_tokens: 2,
            total_tokens: 7,
            ..ResponseUsage::default()
        },
        ..Response::default()
    };
    let got = convert_response(Some(&resp)).unwrap();
    assert_eq!(
        got.candidates[0].content.as_ref().unwrap().parts[0].text,
        "hello",
        "unexpected candidate contents: {:?}",
        got.candidates
    );
    assert_eq!(
        got.usage_metadata.as_ref().map(|u| u.prompt_token_count),
        Some(5),
        "usage metadata missing: {:?}",
        got.usage_metadata
    );
}

#[test]
fn test_convert_response_refusal() {
    let resp = Response {
        output: Some(vec![message(vec![refusal("nope")])]),
        ..Response::default()
    };
    let got = convert_response(Some(&resp)).unwrap();
    let part = &got.candidates[0].content.as_ref().unwrap().parts[0];
    assert_eq!(part.text, "nope", "refusal mismatch");
}

#[test]
fn test_convert_response_no_output() {
    assert!(
        convert_response(Some(&Response::default())).is_err(),
        "expected error for empty output"
    );
}

#[test]
fn test_convert_response_failed_status() {
    let output = vec![message(vec![output_text("half an answer")])];
    let failed =
        |id: &str, error: Option<ResponseError>, output: Option<Vec<ResponseOutputItem>>| {
            Response {
                id: (!id.is_empty()).then(|| id.to_string()),
                status: Some(status::FAILED.into()),
                error,
                output,
                ..Response::default()
            }
        };
    // The error text is compared in full: a partially rendered detail, such as the empty "()"
    // left by a missing code, only shows up in an exact match.
    let tests: Vec<(&str, Response, &str)> = vec![
        (
            "no output",
            failed(
                "resp_123",
                response_error("server_error", "the model failed to generate a response"),
                None,
            ),
            r#"openai: response failed (id "resp_123", code "server_error"): "the model failed to generate a response""#,
        ),
        (
            "partial output",
            failed(
                "resp_123",
                response_error("server_error", "the model failed to generate a response"),
                Some(output.clone()),
            ),
            r#"openai: response failed (id "resp_123", code "server_error"): "the model failed to generate a response""#,
        ),
        (
            "message only",
            failed("", response_error("", "upstream exploded"), None),
            r#"openai: response failed: "upstream exploded""#,
        ),
        (
            "code only",
            failed("", response_error("rate_limit_exceeded", ""), None),
            r#"openai: response failed (code "rate_limit_exceeded")"#,
        ),
        // Nothing but the status and the ID, still the handle to quote back to the provider.
        (
            "id only",
            failed("resp_123", None, None),
            r#"openai: response failed (id "resp_123")"#,
        ),
        // The bare sentinel, with nothing appended to it.
        (
            "no error object",
            failed("", None, None),
            "openai: response failed",
        ),
        // Nothing but spaces must not leave a dangling separator.
        (
            "blank message",
            failed("", response_error("server_error", "  \n "), None),
            r#"openai: response failed (code "server_error")"#,
        ),
        // Reported twice rather than elided: dropping a code because the message appears to
        // contain it loses it whenever the message merely embeds it in a longer token.
        (
            "message already names the code",
            failed(
                "",
                response_error("server_error", "server_error: upstream exploded"),
                None,
            ),
            r#"openai: response failed (code "server_error"): "server_error: upstream exploded""#,
        ),
        // The case an unanchored substring test would silently drop.
        (
            "code embedded in a longer token in the message",
            failed(
                "resp_1",
                response_error(
                    "server_error",
                    "Downstream returned server_error_5xx; retry later.",
                ),
                None,
            ),
            r#"openai: response failed (id "resp_1", code "server_error"): "Downstream returned server_error_5xx; retry later.""#,
        ),
        // Same rule for the ID, which had no de-duplication of its own.
        (
            "message already names the id",
            failed(
                "resp_123",
                response_error("", "resp_123 could not be completed"),
                None,
            ),
            r#"openai: response failed (id "resp_123"): "resp_123 could not be completed""#,
        ),
        // The whole reason the values are quoted: bare, this renders identically to an id of
        // "resp_123" beside a code of "invalid_prompt".
        (
            "id forging a second field",
            failed("resp_123, code invalid_prompt", None, None),
            r#"openai: response failed (id "resp_123, code invalid_prompt")"#,
        ),
        // Distinct from the whitespace-only row below: these trim to something, so a
        // one-sided trim would leave the padding in.
        (
            "id and code that merely have padding",
            failed("  resp_123  ", response_error("\tserver_error ", ""), None),
            r#"openai: response failed (id "resp_123", code "server_error")"#,
        ),
        // Both trims, which nothing else exercises: whitespace-only values must contribute no
        // label at all.
        (
            "blank id and code",
            failed("  ", response_error(" \t ", "upstream exploded"), None),
            r#"openai: response failed: "upstream exploded""#,
        ),
        // The two halves of the absent-status rule, one at a time. With both set, flipping its
        // || to && would go unnoticed.
        (
            "no status, message only",
            Response {
                error: response_error("", "upstream exploded"),
                ..Response::default()
            },
            r#"openai: response failed: "upstream exploded""#,
        ),
        (
            "no status, code only",
            Response {
                error: response_error("rate_limit_exceeded", ""),
                ..Response::default()
            },
            r#"openai: response failed (code "rate_limit_exceeded")"#,
        ),
    ];
    for (name, resp, want_err) in tests {
        let err = match convert_response(Some(&resp)) {
            Ok(got) => panic!("{name}: convertResponse() = {got:?}, want nil alongside the error"),
            Err(err) => err,
        };
        assert!(
            err.is(ErrorKind::ResponseFailed),
            "{name}: error = {err}, want errors.Is(err, shared.ErrResponseFailed)"
        );
        assert_eq!(err.to_string(), want_err, "{name}");
    }
}

/// Pins how narrow the failure check is: "failed" alone costs the caller its output, while a
/// truncation, a cancellation, a queued or in-progress body and an unknown status all still
/// convert. Widening the check to any of them breaks a row here.
#[test]
fn test_convert_response_status_other_than_failed() {
    let tests: Vec<(&str, &str, &str, FinishReason)> = vec![
        ("completed", status::COMPLETED, "", FinishReason::Stop),
        (
            "truncated",
            status::INCOMPLETE,
            "max_output_tokens",
            FinishReason::MaxTokens,
        ),
        (
            "content filtered",
            status::INCOMPLETE,
            "content_filter",
            FinishReason::Safety,
        ),
        (
            "incomplete with no reason",
            status::INCOMPLETE,
            "",
            FinishReason::Other,
        ),
        ("cancelled", status::CANCELLED, "", FinishReason::Other),
        ("queued", status::QUEUED, "", FinishReason::Other),
        ("in progress", status::IN_PROGRESS, "", FinishReason::Other),
        ("unknown to this SDK", "moderated", "", FinishReason::Other),
        ("absent", "", "", FinishReason::Stop),
    ];
    for (name, status, reason, want) in tests {
        let mut resp = Response {
            id: Some("resp_123".into()),
            status: (!status.is_empty()).then(|| status.to_string()),
            incomplete_details: (!reason.is_empty()).then(|| ResponseIncompleteDetails {
                reason: reason.into(),
            }),
            output: Some(vec![message(vec![output_text("half an answer")])]),
            ..Response::default()
        };
        // An error object rides along wherever a status is stated, to pin that it does not
        // override one. The absent-status row omits it, since there it would be a failure
        // rather than a turn.
        if !status.is_empty() {
            resp.error = response_error("server_error", "ignore me");
        }
        let got = convert_response(Some(&resp)).unwrap_or_else(|err| {
            panic!("{name}: convertResponse(status {status:?}) err = {err}, want the response converted")
        });
        let candidate = &got.candidates[0];
        assert_eq!(
            candidate.content.as_ref().unwrap().parts[0].text,
            "half an answer",
            "{name}: text, want the output the server did produce"
        );
        assert_eq!(candidate.finish_reason, Some(want), "{name}: FinishReason");
    }
}

#[test]
fn test_convert_response_logprobs() {
    let (logprobs, want) = fully_specified_logprobs();
    let tests: Vec<(&str, Vec<ResponseOutputTextLogprob>, Option<LogprobsResult>)> = vec![
        ("empty config", vec![], None),
        ("fully specified", logprobs, Some(want)),
    ];
    for (name, logprobs, want_result) in tests {
        let resp = Response {
            id: Some("resp-1".into()),
            model: Some("gpt-test".into()),
            output: Some(vec![message(vec![ResponseOutputContent {
                logprobs,
                ..output_text("hello")
            }])]),
            ..Response::default()
        };
        let got = convert_response(Some(&resp)).unwrap();
        assert!(!got.candidates.is_empty(), "{name}: expected candidates");
        assert_eq!(
            got.candidates[0].logprobs_result, want_result,
            "{name}: LogprobsResult mismatch"
        );
    }
}

#[test]
fn test_convert_response_incomplete_details() {
    let resp = Response {
        id: Some("resp-1".into()),
        model: Some("gpt-test".into()),
        output: Some(vec![message(vec![output_text("hello")])]),
        incomplete_details: Some(ResponseIncompleteDetails {
            reason: "max_output_tokens".into(),
        }),
        ..Response::default()
    };
    let got = convert_response(Some(&resp)).unwrap();
    assert_eq!(
        got.prompt_feedback, None,
        "expected PromptFeedback to be nil"
    );
    assert!(!got.candidates.is_empty(), "expected candidates");
    assert_eq!(
        got.candidates[0].finish_reason,
        Some(FinishReason::MaxTokens)
    );
}

#[test]
fn test_convert_function_call() {
    // (name, arguments, wantErr, wantID, wantName, wantArg)
    let tests: Vec<(&str, &str, bool, &str, &str, &str)> = vec![
        (
            "valid",
            r#"{"arg":"val"}"#,
            false,
            "call-1",
            "test_fn",
            "val",
        ),
        ("bad json", "{bad", true, "", "", ""),
    ];
    for (name, arguments, want_err, want_id, want_name, want_arg) in tests {
        let call = ResponseOutputItem {
            call_id: "call-1".into(),
            name: "test_fn".into(),
            arguments: string_arguments(arguments),
            ..ResponseOutputItem::default()
        };
        let got = convert_function_call(&call);
        assert_eq!(
            got.is_err(),
            want_err,
            "{name}: convertFunctionCall() = {got:?}"
        );
        let Ok(got) = got else { continue };
        let call = got
            .function_call
            .as_ref()
            .expect("expected result, got nil");
        assert_eq!(
            (call.id.as_str(), call.name.as_str()),
            (want_id, want_name),
            "{name}: unexpected fields"
        );
        assert_eq!(
            call.args.get("arg"),
            Some(&json!(want_arg)),
            "{name}: unexpected args"
        );
    }
}

/// Exercises the arguments field through real decoding: building the item by hand bypasses
/// it, so it cannot tell which form a given wire payload takes.
#[test]
fn test_convert_function_call_decoded_arguments() {
    let ok = |args: serde_json::Value| Some(args);
    let tests: Vec<(&str, &str, Option<serde_json::Value>)> = vec![
        (
            "string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":"{\"location\":\"Paris\"}"}"#,
            ok(json!({"location": "Paris"})),
        ),
        // OpenAI-compatible endpoints commonly send a bare JSON object here rather than a
        // string. These arguments must not be dropped.
        (
            "object arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":{"location":"Paris"}}"#,
            ok(json!({"location": "Paris"})),
        ),
        (
            "empty object arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":{}}"#,
            ok(json!({})),
        ),
        (
            "empty string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":""}"#,
            ok(json!({})),
        ),
        (
            "null arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":null}"#,
            ok(json!({})),
        ),
        (
            "absent arguments",
            r#"{"type":"function_call","name":"f","call_id":"c"}"#,
            ok(json!({})),
        ),
        // A JSON string holding the literal null means "no arguments", matching a null in the
        // bare form.
        (
            "null string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":"null"}"#,
            ok(json!({})),
        ),
        // Both forms follow encoding/json's last-one-wins rule for a duplicate key.
        (
            "duplicate keys in object arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":{"path":"/tmp/safe","path":"/etc/shadow"}}"#,
            ok(json!({"path": "/etc/shadow"})),
        ),
        (
            "duplicate keys in string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":"{\"path\":\"/tmp/safe\",\"path\":\"/etc/shadow\"}"}"#,
            ok(json!({"path": "/etc/shadow"})),
        ),
        // A non-object payload must surface an error rather than degrade into an empty
        // argument map.
        (
            "array arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":[1,2]}"#,
            None,
        ),
        (
            "number arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":42}"#,
            None,
        ),
        (
            "bool arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":true}"#,
            None,
        ),
        (
            "non-object string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":"[1,2]"}"#,
            None,
        ),
        // Out of range for float64: rejected from the original bytes.
        (
            "out of range number in object arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":{"x":1e400}}"#,
            None,
        ),
        (
            "malformed string arguments",
            r#"{"type":"function_call","name":"f","call_id":"c","arguments":"{not json"}"#,
            None,
        ),
    ];
    for (name, raw, want_args) in tests {
        let item: ResponseOutputItem = serde_json::from_str(raw)
            .unwrap_or_else(|err| panic!("{name}: json.Unmarshal({raw}) err = {err}"));
        let got = convert_function_call(&item);
        assert_eq!(
            got.is_err(),
            want_args.is_none(),
            "{name}: convertFunctionCall() = {got:?}"
        );
        match (got, want_args) {
            (Err(err), _) => assert!(
                err.is(ErrorKind::FunctionCallArgs),
                "{name}: error = {err}, want errors.Is(err, shared.ErrFunctionCallArgs)"
            ),
            (Ok(part), Some(want)) => assert_eq!(
                serde_json::Value::Object(part.function_call.unwrap().args),
                want,
                "{name}: Args mismatch"
            ),
            (Ok(_), None) => unreachable!(),
        }
    }
}

/// Covers items built by hand rather than decoded from the wire, as callers do in their own
/// tests.
// parity: adapted: Go's "bare arm" is a hand-built `map[string]any` the code re-marshals; gadk keeps every non-string form as raw JSON, so the case builds that raw value
#[test]
fn test_convert_function_call_hand_built_arguments() {
    let tests: Vec<(&str, Option<Box<RawValue>>, serde_json::Value)> = vec![
        (
            "string arm",
            string_arguments(r#"{"location":"Paris"}"#),
            json!({"location": "Paris"}),
        ),
        (
            "bare arm",
            Some(RawValue::from_string(r#"{"location":"Paris"}"#.into()).unwrap()),
            json!({"location": "Paris"}),
        ),
        ("zero value", None, json!({})),
    ];
    for (name, arguments, want_args) in tests {
        let item = ResponseOutputItem {
            arguments,
            ..ResponseOutputItem::default()
        };
        let got = convert_function_call(&item)
            .unwrap_or_else(|err| panic!("{name}: convertFunctionCall() err = {err}"));
        assert_eq!(
            serde_json::Value::Object(got.function_call.unwrap().args),
            want_args,
            "{name}: Args mismatch"
        );
    }
}

/// Covers what the `convert_function_call` tests structurally cannot: one undecodable
/// arguments payload discards the entire response (the model's text with it), the sentinel
/// survives the wrapping, and the error names the offending call.
#[test]
fn test_convert_response_bad_function_call_args() {
    let raw = r#"{"id":"r","model":"m","output":[
		{"type":"message","content":[{"type":"output_text","text":"hello"}]},
		{"type":"function_call","name":"write_file","call_id":"call-1","arguments":[1,2]}]}"#;
    let resp: Response = serde_json::from_str(raw).unwrap();
    // The preceding text part does not survive.
    let err = match convert_response(Some(&resp)) {
        Ok(got) => panic!("convertResponse() = {got:?}, want error"),
        Err(err) => err,
    };
    assert!(
        err.is(ErrorKind::FunctionCallArgs),
        "error = {err}, want errors.Is(err, shared.ErrFunctionCallArgs)"
    );
    for want in ["write_file", "call-1"] {
        assert!(
            err.to_string().contains(want),
            "error = {err:?}, want it to mention {want:?}"
        );
    }
}

/// Go `decodeResponse`: a response built the way the wire does, so a case can turn on whether
/// a field was sent.
fn decode_response(body: &str) -> Response {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("decoding {body}: {err}"))
}

#[test]
fn test_finish_reason() {
    // (name, body, incompleteEvent, want): body is the response as the provider sent it, so a
    // field's presence is part of the case; incompleteEvent is what only the streaming path
    // can know (the terminal event was a "response.incomplete").
    let tests: Vec<(&str, &str, bool, FinishReason)> = vec![
        (
            "max output tokens",
            r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#,
            false,
            FinishReason::MaxTokens,
        ),
        (
            "content filter",
            r#"{"incomplete_details":{"reason":"content_filter"}}"#,
            false,
            FinishReason::Safety,
        ),
        (
            "an unmapped reason",
            r#"{"incomplete_details":{"reason":"something_new"}}"#,
            false,
            FinishReason::Other,
        ),
        ("nothing at all", "{}", false, FinishReason::Stop),
        (
            "completed",
            r#"{"status":"completed"}"#,
            false,
            FinishReason::Stop,
        ),
        // A finished turn carries incomplete_details as null.
        (
            "completed, incomplete_details null",
            r#"{"status":"completed","incomplete_details":null}"#,
            false,
            FinishReason::Stop,
        ),
        // Cut short, but not saying why. Reading that silence as a clean stop would have a
        // caller accept a truncated answer as final.
        (
            "incomplete without a reason",
            r#"{"status":"incomplete"}"#,
            false,
            FinishReason::Other,
        ),
        (
            "incomplete with a reason",
            r#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}"#,
            false,
            FinishReason::MaxTokens,
        ),
        (
            "failed",
            r#"{"status":"failed"}"#,
            false,
            FinishReason::Other,
        ),
        (
            "cancelled",
            r#"{"status":"cancelled"}"#,
            false,
            FinishReason::Other,
        ),
        // openai-go documents six statuses and marks none required; an unfinished one is no
        // more a clean stop than an empty reason is.
        (
            "queued",
            r#"{"status":"queued"}"#,
            false,
            FinishReason::Other,
        ),
        (
            "in progress",
            r#"{"status":"in_progress"}"#,
            false,
            FinishReason::Other,
        ),
        // No status, but an incomplete_details object the provider did send: a schema-legal
        // way to report a truncated turn, and all the blocking path gets.
        (
            "empty incomplete_details, no status",
            r#"{"incomplete_details":{}}"#,
            false,
            FinishReason::Other,
        ),
        (
            "empty reason, no status",
            r#"{"incomplete_details":{"reason":""}}"#,
            false,
            FinishReason::Other,
        ),
        // The payload says nothing; only the event does.
        (
            "silent payload behind an incomplete event",
            "{}",
            true,
            FinishReason::Other,
        ),
        (
            "an incomplete event never downgrades a real reason",
            r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#,
            true,
            FinishReason::MaxTokens,
        ),
    ];
    for (name, body, incomplete_event, want) in tests {
        let got = finish_reason(Some(&decode_response(body)), incomplete_event);
        assert_eq!(
            got, want,
            "{name}: finishReason({body}, incompleteEvent={incomplete_event})"
        );
    }

    // nil response
    assert_eq!(
        finish_reason(None, false),
        FinishReason::Unspecified,
        "nil response: finishReason(nil), want Unspecified"
    );
}

/// Pins which of the provider's own words are reported, since the finish reason flattens a
/// failure and an unmapped truncation both to OTHER.
#[test]
fn test_finish_message() {
    let with_status = |status: &str| Response {
        status: Some(status.into()),
        ..Response::default()
    };
    let tests: Vec<(&str, Option<Response>, bool, &str)> = vec![
        ("nil response", None, false, ""),
        (
            "the server's error message outranks the rest",
            Some(Response {
                status: Some(status::FAILED.into()),
                error: response_error("", "upstream exploded"),
                incomplete_details: Some(ResponseIncompleteDetails {
                    reason: "content_filter".into(),
                }),
                ..Response::default()
            }),
            false,
            "upstream exploded",
        ),
        (
            "then the incomplete reason",
            Some(Response {
                status: Some(status::INCOMPLETE.into()),
                incomplete_details: Some(ResponseIncompleteDetails {
                    reason: "something_new".into(),
                }),
                ..Response::default()
            }),
            false,
            "something_new",
        ),
        (
            "and the status when that is all there is",
            Some(with_status(status::INCOMPLETE)),
            false,
            "incomplete",
        ),
        // A silent payload behind an event whose name is the only account.
        (
            "the event's own name when the payload is silent",
            Some(Response::default()),
            true,
            "incomplete",
        ),
        // The precedence truncated() applies, applied here too: a turn the event says did not
        // finish cannot report "completed" as why.
        (
            "the event's name outranks a payload calling the turn completed",
            Some(with_status(status::COMPLETED)),
            true,
            "incomplete",
        ),
        // Only that contradiction is overridden. A status agreeing the turn did not finish is
        // still the provider's own wording for why.
        (
            "a status the event does not contradict is reported",
            Some(with_status(status::FAILED)),
            true,
            "failed",
        ),
        (
            "a completed payload behind no such event stands",
            Some(with_status(status::COMPLETED)),
            false,
            "completed",
        ),
        ("nothing to say", Some(Response::default()), false, ""),
    ];
    for (name, resp, incomplete_event, want) in tests {
        assert_eq!(
            finish_message(resp.as_ref(), incomplete_event),
            want,
            "{name}: finishMessage()"
        );
    }
}

/// Pins that logprobs are handed out only against the text they describe, so a streamed turn
/// cannot report the terminal event's probabilities over what the deltas built.
#[test]
fn test_logprobs_for() {
    let hello_logprobs = || {
        vec![ResponseOutputTextLogprob {
            token: "hello".into(),
            logprob: -0.5,
            ..ResponseOutputTextLogprob::default()
        }]
    };
    let resp = Response {
        output: Some(vec![message(vec![ResponseOutputContent {
            logprobs: hello_logprobs(),
            ..output_text("hello")
        }])]),
        ..Response::default()
    };
    assert!(
        logprobs_for(Some(&resp), "hello").is_some(),
        "logprobsFor(matching text) = nil, want the logprobs"
    );
    assert_eq!(
        logprobs_for(Some(&resp), "hel"),
        None,
        "logprobsFor(different text), want nil"
    );
    assert_eq!(logprobs_for(None, ""), None, "logprobsFor(nil), want nil");

    // a refusal is part of the answer: a refusal becomes a text part like any other (see
    // convert_output_items), so it counts towards the answer the logprobs describe.
    let refused = Response {
        output: Some(vec![message(vec![
            ResponseOutputContent {
                logprobs: hello_logprobs(),
                ..output_text("hello")
            },
            refusal("I cannot"),
        ])]),
        ..Response::default()
    };
    assert!(
        logprobs_for(Some(&refused), "helloI cannot").is_some(),
        "a refusal is part of the answer: logprobsFor(text including the refusal) = nil, want the logprobs"
    );
    assert_eq!(
        logprobs_for(Some(&refused), "hello"),
        None,
        "a refusal is part of the answer: logprobsFor(text missing the refusal), want nil"
    );
}

#[test]
fn test_convert_output_items() {
    let text = |t: &str, thought: bool| Part {
        text: t.into(),
        thought,
        ..Part::default()
    };
    // (name, items, want, wantErr)
    type Case = (
        &'static str,
        Vec<ResponseOutputItem>,
        Vec<Part>,
        Option<ErrorKind>,
    );
    let tests: Vec<Case> = vec![
        (
            "valid items",
            vec![
                message(vec![output_text("text1"), refusal("nope")]),
                ResponseOutputItem {
                    kind: "function_call".into(),
                    call_id: "call-1".into(),
                    name: "fn".into(),
                    arguments: string_arguments("{}"),
                    ..ResponseOutputItem::default()
                },
                ResponseOutputItem {
                    kind: "reasoning".into(),
                    content: vec![ResponseOutputContent {
                        text: "thought1".into(),
                        ..ResponseOutputContent::default()
                    }],
                    summary: vec![ResponseReasoningItemSummary {
                        text: "summary1".into(),
                        ..ResponseReasoningItemSummary::default()
                    }],
                    ..ResponseOutputItem::default()
                },
            ],
            vec![
                text("text1", false),
                text("nope", false),
                Part {
                    function_call: Some(FunctionCall {
                        name: "fn".into(),
                        id: "call-1".into(),
                        ..FunctionCall::default()
                    }),
                    ..Part::default()
                },
                text("thought1", true),
                text("summary1", true),
            ],
            None,
        ),
        (
            "empty items",
            vec![],
            vec![],
            Some(ErrorKind::NoOutputItems),
        ),
        (
            "invalid type",
            vec![ResponseOutputItem {
                kind: "invalid".into(),
                ..ResponseOutputItem::default()
            }],
            vec![],
            Some(ErrorKind::UnsupportedOutputItemType),
        ),
        (
            "invalid message content type",
            vec![message(vec![ResponseOutputContent {
                kind: "invalid".into(),
                ..ResponseOutputContent::default()
            }])],
            vec![],
            Some(ErrorKind::UnsupportedMessageContentType),
        ),
        // Empty text is skipped.
        (
            "empty message content",
            vec![message(vec![output_text("")])],
            vec![],
            Some(ErrorKind::NoTextOrToolContent),
        ),
    ];
    for (name, items, want, want_err) in tests {
        let (parts, err) = match convert_output_items(&items) {
            Ok(parts) => (parts, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        match (want_err, &err) {
            (None, None) => {}
            (Some(kind), Some(err)) if err.is(kind) => {}
            _ => panic!("{name}: convertOutputItems() error = {err:?}, wantErr {want_err:?}"),
        }
        assert_eq!(parts.len(), want.len(), "{name}: part count");
        assert_eq!(parts, want, "{name}: convertOutputItems() mismatch");
    }
}

#[test]
fn test_convert_logprobs() {
    let (logprobs, want) = fully_specified_logprobs();
    let tests: Vec<(&str, Vec<ResponseOutputItem>, Option<LogprobsResult>)> = vec![
        ("empty items", vec![], None),
        (
            "fully specified",
            vec![message(vec![ResponseOutputContent {
                kind: "output_text".into(),
                logprobs,
                ..ResponseOutputContent::default()
            }])],
            Some(want),
        ),
    ];
    for (name, items, want) in tests {
        assert_eq!(
            convert_logprobs(&items),
            want,
            "{name}: convertLogprobs() mismatch"
        );
    }
}
