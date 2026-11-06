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

//! Port of Go internal/utils/utils_test.go.

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_platform::Platform;

use crate::tool::utils;

// parity: adapted: Go installs the provider on a context.Context; Rust passes an explicit Platform.
#[test]
fn test_generate_function_call_id_uses_provider() {
    let platform = Platform::default().with_uuid(|| "fixed".to_string());
    let got = utils::generate_function_call_id(&platform);
    assert!(
        got.starts_with("adk-"),
        "GenerateFunctionCallID() = {got:?}, want \"adk-\" prefix"
    );
    assert!(
        got.ends_with("fixed"),
        "GenerateFunctionCallID() = {got:?}, want provider value"
    );
}

#[test]
fn test_generate_function_call_id_default_is_unique() {
    let platform = Platform::default();
    let first = utils::generate_function_call_id(&platform);
    let second = utils::generate_function_call_id(&platform);
    assert_ne!(first, second);
}

// parity: adapted: explicit Platform instead of context provider
#[test]
fn test_populate_client_function_call_id_uses_provider() {
    let mut content = Content {
        parts: vec![
            Part {
                function_call: Some(FunctionCall {
                    name: "needs_id".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Part {
                function_call: Some(FunctionCall {
                    id: "keep".to_string(),
                    name: "has_id".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let platform = Platform::default().with_uuid(|| "generated".to_string());
    utils::populate_client_function_call_id(&platform, Some(&mut content));
    let ids: Vec<&str> = content
        .parts
        .iter()
        .map(|p| p.function_call.as_ref().unwrap().id.as_str())
        .collect();
    assert_eq!(ids[0], "adk-generated", "empty function call ID");
    assert_eq!(
        ids[1], "keep",
        "preset function call ID must be left untouched"
    );
}

// parity: adapted: Rust Vec<Part> has no nil elements; Part::default() (no payload) stands in for nil
#[test]
fn test_part_accessors_skip_nil_parts() {
    let call = Part {
        function_call: Some(FunctionCall {
            name: "call".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let resp = Part {
        function_response: Some(FunctionResponse {
            name: "resp".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let text = Part::from_text("hi");

    /// Builds the Go test's part layout around `keep`.
    type Layout = fn(&Part) -> Vec<Part>;
    let layouts: [(&str, Layout, usize); 4] = [
        ("leading", |keep| vec![Part::default(), keep.clone()], 1),
        (
            "middle",
            |keep| vec![keep.clone(), Part::default(), keep.clone()],
            2,
        ),
        ("trailing", |keep| vec![keep.clone(), Part::default()], 1),
        ("only", |_keep| vec![Part::default()], 0),
    ];
    let content = |parts: Vec<Part>| Content {
        parts,
        ..Default::default()
    };
    for (name, parts, want) in layouts {
        assert_eq!(
            utils::function_calls(Some(&content(parts(&call)))).len(),
            want,
            "{name}: FunctionCalls()"
        );
        assert_eq!(
            utils::function_responses(Some(&content(parts(&resp)))).len(),
            want,
            "{name}: FunctionResponses()"
        );
        assert_eq!(
            utils::text_parts(Some(&content(parts(&text)))).len(),
            want,
            "{name}: TextParts()"
        );
    }
    assert!(utils::function_calls(None).is_empty());
    assert!(utils::function_responses(None).is_empty());
    assert!(utils::text_parts(None).is_empty());
}
