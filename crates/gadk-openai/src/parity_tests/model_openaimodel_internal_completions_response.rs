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

//! Port of ADK-Go `model/openaimodel/internal/completions/response_test.go` (Task 6).

use gadk_genai::{FinishReason, Part, ROLE_MODEL};
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::completions::response::{
    convert_completion, convert_logprobs, convert_usage, finish_reason,
};
use crate::completions::wire::{ChatCompletion, ChatCompletionChoiceLogprobs};
use crate::error::{ErrorKind, OpenAiError};

/// Go `decodeCompletion`: a `ChatCompletion` from the JSON a provider would send, so the tests
/// exercise the wire types' own decoding rather than hand-built structs.
fn decode_completion(body: &str) -> ChatCompletion {
    match serde_json::from_str(body) {
        Ok(resp) => resp,
        Err(err) => panic!("unmarshal completion: {err}"),
    }
}

fn convert_ok(resp: &ChatCompletion) -> gadk_genai::GenerateContentResponse {
    match convert_completion(Some(resp)) {
        Ok(got) => got,
        Err(err) => panic!("convert_completion() err = {err}"),
    }
}

fn convert_err(resp: Option<&ChatCompletion>) -> OpenAiError {
    match convert_completion(resp) {
        Err(err) => err,
        Ok(got) => panic!("convert_completion() = {got:?}, want an error"),
    }
}

fn parts(resp: &gadk_genai::GenerateContentResponse) -> &[Part] {
    match &resp.candidates[0].content {
        Some(content) => &content.parts,
        None => panic!("candidate has no content"),
    }
}

#[test]
fn test_convert_completion_text() {
    let resp = decode_completion(
        r#"{
		"id":"chatcmpl-1","model":"gpt-4o-mini","object":"chat.completion",
		"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hail at -7 C"}}],
		"usage":{"prompt_tokens":11,"completion_tokens":5,"total_tokens":16}
	}"#,
    );
    let got = convert_ok(&resp);
    assert_eq!(
        (got.response_id.as_str(), got.model_version.as_str()),
        ("chatcmpl-1", "gpt-4o-mini")
    );
    let cand = &got.candidates[0];
    let parts = parts(&got);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].text, "hail at -7 C");
    assert_eq!(
        cand.content.as_ref().map(|c| c.role.as_str()),
        Some(ROLE_MODEL)
    );
    assert_eq!(cand.finish_reason, Some(FinishReason::Stop));
    let usage = match &got.usage_metadata {
        Some(usage) => usage,
        None => panic!("usage = None"),
    };
    assert_eq!(
        (usage.prompt_token_count, usage.candidates_token_count),
        (11, 5)
    );
}

#[test]
fn test_convert_completion_refusal() {
    let resp = decode_completion(
        r#"{
		"id":"c","model":"m","choices":[{"index":0,"finish_reason":"stop",
		"message":{"role":"assistant","content":"","refusal":"I cannot help with that"}}]}"#,
    );
    let got = convert_ok(&resp);
    // Flattened to text, matching what the Responses path does with a refusal content block,
    // so one refusal reads the same on either endpoint.
    let parts = parts(&got);
    assert_eq!(parts.len(), 1, "want the refusal as text");
    assert_eq!(parts[0].text, "I cannot help with that");
}

#[test]
fn test_convert_completion_tool_calls() {
    let resp = decode_completion(
        r#"{
		"id":"c","model":"m","choices":[{"index":0,"finish_reason":"tool_calls","message":{
			"role":"assistant","content":"",
			"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Lisbon\"}"}}]
		}}]}"#,
    );
    let got = convert_ok(&resp);
    let parts = parts(&got);
    assert_eq!(parts.len(), 1, "want one function call");
    let call = match &parts[0].function_call {
        Some(call) => call,
        None => panic!("parts = {parts:?}, want one function call"),
    };
    assert_eq!(call.name, "get_weather");
    assert_eq!(call.id, "call_1");
    assert_eq!(call.args.get("city"), Some(&json!("Lisbon")));
    // A turn that ends by calling a tool stopped cleanly.
    assert_eq!(got.candidates[0].finish_reason, Some(FinishReason::Stop));
}

#[test]
fn test_convert_completion_parallel_tool_calls() {
    let resp = decode_completion(
        r#"{
		"id":"c","model":"m","choices":[{"index":0,"finish_reason":"tool_calls","message":{
			"role":"assistant","content":"working on it",
			"tool_calls":[
				{"id":"a","type":"function","function":{"name":"f","arguments":"{}"}},
				{"id":"b","type":"function","function":{"name":"g","arguments":"{\"n\":3}"}}
			]}}]}"#,
    );
    let got = convert_ok(&resp);
    let parts = parts(&got);
    assert_eq!(parts.len(), 3, "want text plus two calls");
    assert_eq!(parts[0].text, "working on it");
    let ids: Vec<&str> = parts[1..]
        .iter()
        .map(|p| p.function_call.as_ref().map_or("", |c| c.id.as_str()))
        .collect();
    assert_eq!(ids, ["a", "b"]);
    // An empty argument payload is a call that takes none (gadk's args map is never nil).
    assert_eq!(
        parts[1].function_call.as_ref().map(|c| c.args.is_empty()),
        Some(true)
    );
}

/// The two ways a provider says a call takes no arguments besides `{}`: leaving them out,
/// and JSON null. Either must reach the tool as an empty map.
#[test]
fn test_convert_completion_empty_tool_arguments() {
    for args in ["", "null"] {
        let name = format!("{args:?}");
        let raw = json!(args).to_string();
        let resp = decode_completion(&format!(
            r#"{{"id":"c","model":"m","choices":[{{"index":0,"finish_reason":"tool_calls","message":{{
				"role":"assistant","tool_calls":[{{"id":"a","type":"function","function":{{"name":"f","arguments":{raw}}}}}]}}}}]}}"#
        ));
        let got = convert_ok(&resp);
        let call = parts(&got)[0].function_call.as_ref();
        assert_eq!(
            call.map(|c| c.args.is_empty()),
            Some(true),
            "{name}: call = {call:?}, want an empty argument map"
        );
    }
}

/// No call reaches the caller without a function name: stored in the session, a nameless call
/// fails every later request with `ErrorKind::FunctionCallMissingName`.
#[test]
fn test_convert_completion_unusable_tool_calls() {
    for (name, call, want) in [
        (
            "custom tool call",
            r#"{"id":"call_9","type":"custom","custom":{"name":"grep","input":"x"}}"#,
            ErrorKind::UnsupportedOutputItemType,
        ),
        (
            "function call without a name",
            r#"{"id":"call_9","type":"function","function":{"name":"","arguments":"{}"}}"#,
            ErrorKind::FunctionCallMissingName,
        ),
        // Either an id or arguments shows a call was made, so neither alone is read as a
        // padded index.
        (
            "arguments without a name or id",
            r#"{"id":"","type":"function","function":{"name":"","arguments":"{\"city\":\"Lisbon\"}"}}"#,
            ErrorKind::FunctionCallMissingName,
        ),
        (
            "id without a name or arguments",
            r#"{"id":"call_9","type":"function","function":{"name":"","arguments":""}}"#,
            ErrorKind::FunctionCallMissingName,
        ),
    ] {
        let resp = decode_completion(&format!(
            r#"{{"id":"c","model":"m","choices":[{{"index":0,"finish_reason":"tool_calls",
				"message":{{"role":"assistant","tool_calls":[{call}]}}}}]}}"#
        ));
        let err = convert_err(Some(&resp));
        assert!(err.is(want), "{name}: err = {err}, want {want}");
    }
}

/// The empty entry the accumulator leaves at a tool-call index a stream skipped: no call was
/// made there, so it is dropped rather than returned as a nameless call.
#[test]
fn test_convert_completion_skips_padded_tool_call() {
    let resp = decode_completion(
        r#"{"id":"c","model":"m","choices":[{"index":0,"finish_reason":"tool_calls",
		"message":{"role":"assistant","tool_calls":[
			{"id":"","type":"","function":{"name":"","arguments":""}},
			{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{}"}}
		]}}]}"#,
    );
    let got = convert_ok(&resp);
    let parts = parts(&got);
    assert_eq!(parts.len(), 1, "want only the call_1 call");
    assert_eq!(
        parts[0].function_call.as_ref().map(|c| c.id.as_str()),
        Some("call_1")
    );
}

#[test]
fn test_convert_completion_unparseable_tool_arguments() {
    let resp = decode_completion(
        r#"{
		"id":"c","model":"m","choices":[{"index":0,"finish_reason":"tool_calls","message":{
			"role":"assistant","tool_calls":[{"id":"call_9","type":"function",
			"function":{"name":"get_weather","arguments":"{not json"}}]}}]}"#,
    );
    let err = convert_err(Some(&resp));
    assert!(
        err.is(ErrorKind::FunctionCallArgs),
        "err = {err}, want {}",
        ErrorKind::FunctionCallArgs
    );
    // Conversion aborts on the first bad call, so the error has to identify it.
    let text = err.to_string();
    assert!(
        text.contains("get_weather") && text.contains("call_9"),
        "err = {text}, want it to name the call"
    );
}

#[test]
fn test_convert_completion_empty() {
    let no_choices = decode_completion(r#"{"id":"c","model":"m","choices":[]}"#);
    let nothing = decode_completion(
        r#"{"id":"c","model":"m","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":""}}]}"#,
    );
    for (name, resp, want) in [
        ("nil", None, ErrorKind::EmptyResponse),
        ("no choices", Some(&no_choices), ErrorKind::NoChoices),
        (
            "choice with nothing to read",
            Some(&nothing),
            ErrorKind::NoTextOrToolContent,
        ),
    ] {
        let err = convert_err(resp);
        assert!(err.is(want), "{name}: err = {err}, want {want}");
    }
}

#[test]
fn test_finish_reason() {
    for (reason, want) in [
        ("stop", FinishReason::Stop),
        ("tool_calls", FinishReason::Stop),
        ("function_call", FinishReason::Stop),
        ("length", FinishReason::MaxTokens),
        ("content_filter", FinishReason::Safety),
        // Silence is not a clean stop: a caller that retries on anything but STOP must not
        // accept a partial answer as final.
        ("", FinishReason::Unspecified),
        ("something_new", FinishReason::Other),
    ] {
        assert_eq!(finish_reason(reason), want, "finish_reason({reason:?})");
    }
}

#[test]
fn test_convert_usage() {
    let resp = decode_completion(
        r#"{"id":"c","model":"m",
		"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"x"}}],
		"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,
			"prompt_tokens_details":{"cached_tokens":40},
			"completion_tokens_details":{"reasoning_tokens":7}}}"#,
    );
    let usage = match &resp.usage {
        Some(usage) => usage,
        None => panic!("usage = None"),
    };
    let got = convert_usage(usage);
    // The field names differ from the Responses API's on every count, so this pins the
    // mapping rather than the arithmetic.
    assert_eq!(
        (
            got.prompt_token_count,
            got.candidates_token_count,
            got.total_token_count
        ),
        (100, 20, 120),
        "totals = {got:?}"
    );
    assert_eq!(got.cached_content_token_count, 40);
    assert_eq!(got.thoughts_token_count, 7);
}

#[test]
fn test_convert_logprobs() {
    let resp = decode_completion(
        r#"{"id":"c","model":"m","choices":[{"index":0,"finish_reason":"stop",
		"message":{"role":"assistant","content":"hi"},
		"logprobs":{"content":[{"token":"hi","logprob":-0.25,
			"top_logprobs":[{"token":"hi","logprob":-0.25},{"token":"yo","logprob":-2.5}]}]}}]}"#,
    );
    let got = match convert_logprobs(&resp.choices[0].logprobs) {
        Some(got) => got,
        None => panic!("logprobs = None"),
    };
    assert_eq!(got.chosen_candidates.len(), 1, "logprobs = {got:?}");
    assert_eq!(got.chosen_candidates[0].token, "hi");
    assert_eq!(got.top_candidates.len(), 1, "top candidates = {got:?}");
    assert_eq!(got.top_candidates[0].candidates.len(), 2);
}

#[test]
fn test_convert_logprobs_absent() {
    let got = convert_logprobs(&ChatCompletionChoiceLogprobs::default());
    assert_eq!(got, None, "want None when the provider sent none");
}
