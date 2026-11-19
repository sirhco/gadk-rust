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

//! Parity target for Go tool/toolconfirmation/tool_confirmation_test.go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_agent::{REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, original_call_from};
use gadk_genai::FunctionCall;
use serde_json::{Map, Value, json};

fn call(id: &str, args: Value) -> FunctionCall {
    FunctionCall {
        id: id.into(),
        args: args.as_object().cloned().unwrap_or_else(Map::new),
        ..Default::default()
    }
}

// parity: adapted: the parameter is &FunctionCall (decision D3), so Go's nil-pointer case is a call with no args, which takes the same nil check and message
#[test]
fn test_original_call_from() {
    assert_eq!(
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        "adk_request_confirmation"
    );
    let cases: Vec<(&str, FunctionCall, Option<&str>)> = vec![
        (
            "Success - Valid Structure",
            call(
                "call_123",
                json!({"originalFunctionCall": {"ID": "call_999", "Name": "weather_lookup"}}),
            ),
            None,
        ),
        (
            "Failure - Nil Input",
            FunctionCall::default(),
            Some("cannot be nil"),
        ),
        (
            "Failure - Missing Key",
            call("call_456", json!({"someOtherKey": "foo"})),
            Some("required argument \"originalFunctionCall\" is missing"),
        ),
        (
            "Failure - Invalid Type (String instead of Map)",
            call(
                "call_789",
                json!({"originalFunctionCall": "{\"Name\": \"bad_json\"}"}),
            ),
            Some("found a string"),
        ),
        (
            "Failure - Invalid Type (Nil value)",
            call("call_nil", json!({"originalFunctionCall": null})),
            Some("found null"),
        ),
        (
            "Failure - Converter Error (Bad Internal Structure id with wrong type)",
            call(
                "call_bad_struct",
                json!({"originalFunctionCall": {"ID": 11}}),
            ),
            Some("failed to decode"),
        ),
    ];
    for (name, input, want_err) in cases {
        match (original_call_from(&input), want_err) {
            (Ok(got), None) => assert_eq!(got.name, "weather_lookup", "{name}"),
            (Err(e), Some(sub)) => assert!(
                e.to_string().contains(sub),
                "{name}: error {e:?} lacks {sub:?}"
            ),
            (got, want) => panic!("{name}: got {got:?}, want error {want:?}"),
        }
    }
}
