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

//! Port of ADK-Go `model/openaimodel/internal/responses/stream_test.go` (Task 7).

use gadk_model::StreamingResponseAggregator;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::error::ErrorKind;
use crate::responses::response::convert_response;
use crate::responses::stream::StreamTranslator;
use crate::responses::wire::ResponseStreamEvent;

/// Go `decodeEvent`: a stream event decoded from its JSON.
pub(super) fn decode_event(body: &str) -> ResponseStreamEvent {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("decodeEvent: {err}"))
}

#[test]
fn test_stream_translator_text_delta() {
    let mut tr = StreamTranslator::new();
    let event = decode_event(r#"{"type":"response.output_text.delta","delta":"chunk"}"#);
    let resp = tr.process(&event).unwrap();
    let text = resp.as_ref().map(|r| {
        r.candidates[0].content.as_ref().unwrap().parts[0]
            .text
            .as_str()
    });
    assert_eq!(text, Some("chunk"), "unexpected translation: {resp:?}");
}

#[test]
fn test_stream_translator_refusal_delta() {
    let mut tr = StreamTranslator::new();
    let event = decode_event(
        r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"I cannot help with that.","sequence_number":1}"#,
    );
    let resp = tr.process(&event).unwrap();
    let Some(resp) = resp else {
        panic!("unexpected translation: None")
    };
    assert_eq!(resp.candidates.len(), 1, "unexpected translation: {resp:?}");
    let parts = &resp.candidates[0].content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 1, "unexpected translation: {resp:?}");
    assert_eq!(parts[0].text, "I cannot help with that.", "part.Text");
    assert!(!parts[0].thought, "part.Thought = true, want false");
}

#[test]
fn test_stream_translator_empty_refusal_delta() {
    let mut tr = StreamTranslator::new();
    let event = decode_event(
        r#"{"type":"response.refusal.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"","sequence_number":1}"#,
    );
    let resp = tr.process(&event).unwrap();
    assert_eq!(resp, None, "process() response, want nil");
}

#[test]
fn test_stream_translator_function_call() {
    let mut tr = StreamTranslator::new();
    let added = decode_event(
        r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","call_id":"call_real","name":"lookup"}}"#,
    );
    tr.process(&added).unwrap();
    let delta = decode_event(
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"city\":\""}"#,
    );
    tr.process(&delta).unwrap();
    let delta = decode_event(
        r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"Paris\"}"}"#,
    );
    tr.process(&delta).unwrap();
    let done = decode_event(
        r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":""}"#,
    );
    let resp = tr.process(&done).unwrap().unwrap();
    let part = &resp.candidates[0].content.as_ref().unwrap().parts[0];
    let call = part
        .function_call
        .as_ref()
        .unwrap_or_else(|| panic!("function call not translated: {part:?}"));
    assert_eq!(call.name, "lookup", "function call not translated");
    assert_eq!(
        call.args.get("city"),
        Some(&json!("Paris")),
        "args mismatch"
    );
    assert_eq!(call.id, "call_real", "call ID mismatch");
}

/// Pins where the name may come from. The OpenAI API does not put one on the done event, so
/// `response.output_item.added` is the only source; a provider that sends the name only on
/// the done event loses it.
#[test]
fn test_stream_translator_function_call_name_only_on_done_event() {
    let mut tr = StreamTranslator::new();
    let done = decode_event(
        r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","name":"lookup","arguments":"{}"}"#,
    );
    let resp = tr.process(&done).unwrap().unwrap();
    let part = &resp.candidates[0].content.as_ref().unwrap().parts[0];
    let call = part
        .function_call
        .as_ref()
        .unwrap_or_else(|| panic!("no function call: {part:?}"));
    assert_eq!(
        call.name, "",
        "FunctionCall.Name, want empty: the done event is not a name source"
    );
}

#[test]
fn test_stream_translator_with_aggregator() {
    let mut tr = StreamTranslator::new();
    let mut aggregator = StreamingResponseAggregator::new();
    let events = [
        decode_event(r#"{"type":"response.output_text.delta","delta":"hel"}"#),
        decode_event(r#"{"type":"response.output_text.delta","delta":"lo"}"#),
    ];
    let mut final_text = String::new();
    for event in &events {
        let resp = match tr.process(event) {
            Ok(Some(resp)) => resp,
            other => panic!("unexpected translator result: {other:?}"),
        };
        for llm_resp in aggregator.process_response(resp) {
            let llm_resp = llm_resp.unwrap_or_else(|err| panic!("aggregator err = {err}"));
            if !llm_resp.partial
                && let Some(content) = &llm_resp.content
                && let Some(part) = content.parts.first()
            {
                final_text.push_str(&part.text);
            }
        }
    }
    if let Some(last) = aggregator.close()
        && let Some(content) = &last.content
        && let Some(part) = content.parts.first()
    {
        final_text.push_str(&part.text);
    }
    assert_eq!(final_text, "hello", "aggregated text mismatch");
}

#[test]
fn test_stream_translator_response_failed() {
    let mut tr = StreamTranslator::new();
    let event = decode_event(
        r#"{"type":"response.failed","response":{"id":"resp_123","status":"failed","error":{"code":"server_error","message":"the model failed to generate a response"}}}"#,
    );
    let err = match tr.process(&event) {
        Ok(resp) => panic!("process() = {resp:?}, want nil alongside the error"),
        Err(err) => err,
    };
    assert!(
        err.is(ErrorKind::ResponseFailed),
        "process() err = {err}, want errors.Is(err, shared.ErrResponseFailed)"
    );
    assert_eq!(
        err.to_string(),
        r#"openai: response failed (id "resp_123", code "server_error"): "the model failed to generate a response""#
    );
}

/// Pins that one server failure reads the same on the blocking path and as a streamed
/// "response.failed". Comparing the two texts to each other, not to literals, is what stops
/// them drifting apart again.
#[test]
fn test_stream_translator_response_failed_paths_agree() {
    let tests = [
        (
            "message and code",
            r#","error":{"code":"server_error","message":"upstream exploded"}"#,
        ),
        (
            "message only",
            r#","error":{"message":"upstream exploded"}"#,
        ),
        ("code only", r#","error":{"code":"rate_limit_exceeded"}"#),
        ("no error object", ""),
    ];
    for (name, err_json) in tests {
        let event = decode_event(&format!(
            r#"{{"type":"response.failed","response":{{"status":"failed"{err_json}}}}}"#
        ));
        let stream_err = StreamTranslator::new().process(&event).unwrap_err();
        let block_err = convert_response(Some(&event.response)).unwrap_err();
        for (path, err) in [("stream", &stream_err), ("blocking", &block_err)] {
            assert!(
                err.is(ErrorKind::ResponseFailed),
                "{name}: {path} path err = {err}, want errors.Is(err, shared.ErrResponseFailed)"
            );
        }
        assert_eq!(
            stream_err.to_string(),
            block_err.to_string(),
            "{name}: paths disagree"
        );
    }
}
