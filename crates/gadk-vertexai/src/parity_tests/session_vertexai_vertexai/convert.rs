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

//! Ports of the conversion tests in ADK-Go `session/vertexai/vertexai_test.go`. Go tests that
//! build `aiplatformpb` messages take the same messages as protojson values here.

use std::collections::BTreeMap;

use gadk_genai::{Content, FunctionCall, FunctionResponse, Language, Outcome, Part, Transcription};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, NodeInfo, RequestInput};
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::session::convert::{
    actions_from_wire, actions_to_wire, content_from_wire, content_to_wire, event_from_raw_event,
    event_needs_raw_event, event_to_raw_event, metadata_to_wire, to_struct,
};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().expect("an object").clone()
}

#[test]
fn test_aiplatform_to_genai_content_function_call_mapping() {
    struct Case {
        name: &'static str,
        input: Value,
        want_id: &'static str,
        want_name: &'static str,
        want_key: &'static str,
        want_val: &'static str,
        is_response: bool,
    }
    let cases = [
        Case {
            name: "FunctionCall preserves ID, Name, and Args",
            input: json!({"content": {"role": "model", "parts": [{"functionCall": {
                "id": "call-id-abc", "name": "my_tool", "args": {"param": "value"}
            }}]}}),
            want_id: "call-id-abc",
            want_name: "my_tool",
            want_key: "param",
            want_val: "value",
            is_response: false,
        },
        Case {
            name: "FunctionCall with empty ID is preserved as empty",
            input: json!({"content": {"role": "model", "parts": [{"functionCall": {
                "name": "tool_no_id", "args": {"x": "y"}
            }}]}}),
            want_id: "",
            want_name: "tool_no_id",
            want_key: "x",
            want_val: "y",
            is_response: false,
        },
        Case {
            name: "FunctionResponse preserves ID, Name, and Response",
            input: json!({"content": {"role": "user", "parts": [{"functionResponse": {
                "id": "call-id-abc", "name": "my_tool", "response": {"result": "ok"}
            }}]}}),
            want_id: "call-id-abc",
            want_name: "my_tool",
            want_key: "result",
            want_val: "ok",
            is_response: true,
        },
    ];

    for tc in cases {
        let got = content_from_wire(&tc.input["content"]);
        assert!(
            !got.parts.is_empty(),
            "{}: expected at least one part, got none",
            tc.name
        );
        let (id, name, map) = if tc.is_response {
            let fr = got.parts[0]
                .function_response
                .as_ref()
                .unwrap_or_else(|| panic!("{}: expected FunctionResponse part", tc.name));
            (&fr.id, &fr.name, &fr.response)
        } else {
            let fc = got.parts[0]
                .function_call
                .as_ref()
                .unwrap_or_else(|| panic!("{}: expected FunctionCall part", tc.name));
            (&fc.id, &fc.name, &fc.args)
        };
        assert_eq!(id, tc.want_id, "{}: ID", tc.name);
        assert_eq!(name, tc.want_name, "{}: Name", tc.name);
        assert_eq!(
            map.get(tc.want_key),
            Some(&json!(tc.want_val)),
            "{}: [{}]",
            tc.name,
            tc.want_key
        );
    }
}

#[test]
fn test_raw_event_round_trip() {
    let cases = [
        (
            "workflow fields",
            Event {
                invocation_id: "inv-1".into(),
                author: "agent-x".into(),
                branch: "a.b".into(),
                isolation_scope: "scope-1".into(),
                routes: vec!["approve".into()],
                output: Some(json!("the-output")),
                node_info: Some(NodeInfo {
                    path: "wf/child@1".into(),
                    message_as_output: true,
                    output_for: vec!["wf/child@1".into(), "wf".into()],
                }),
                ..Default::default()
            },
        ),
        (
            "content with text and function call",
            Event {
                author: "agent-x".into(),
                response: LlmResponse {
                    content: Some(Content {
                        role: "model".into(),
                        parts: vec![
                            Part::from_text("hello"),
                            Part {
                                function_call: Some(FunctionCall {
                                    id: "call-1".into(),
                                    name: "get_weather".into(),
                                    args: obj(json!({"city": "Stockholm"})),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        ],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        (
            // Go: map[string]any{"score": float64(42), …}; a float64 42 is the JSON number 42.
            "structured output",
            Event {
                author: "agent-x".into(),
                output: Some(json!({"score": 42, "label": "ok"})),
                ..Default::default()
            },
        ),
        (
            // A typed i64 artifact delta keeps its integer type (unlike the any-typed output
            // and state delta; see test_raw_event_numeric_contract).
            "typed int64 artifact delta preserved",
            Event {
                author: "agent-x".into(),
                output: Some(json!("x")),
                actions: EventActions {
                    artifact_delta: Some(BTreeMap::from([("file.png".into(), 7)])),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    ];

    for (name, event) in cases {
        let raw = event_to_raw_event(&event)
            .unwrap_or_else(|e| panic!("{name}: event_to_raw_event() error = {e}"));
        let got = event_from_raw_event(&raw)
            .unwrap_or_else(|e| panic!("{name}: event_from_raw_event() error = {e}"));
        assert_eq!(got, event, "{name}: round-trip mismatch");
    }
}

// parity: adapted: Go checks for float64; serde_json keeps one number type, so the port pins the double's lost precision (2^53 + 1 reads back as 2^53) and the value 3
#[test]
fn test_raw_event_numeric_contract() {
    let event = Event {
        author: "agent-x".into(),
        output: Some(json!(9_007_199_254_740_993_i64)), // 2^53 + 1
        actions: EventActions {
            state_delta: Some(obj(json!({"count": 3}))),
            ..Default::default()
        },
        ..Default::default()
    };
    let raw = event_to_raw_event(&event).unwrap();
    let got = event_from_raw_event(&raw).unwrap();
    assert_eq!(
        got.output,
        Some(json!(9_007_199_254_740_992_i64)),
        "Output went through a float64"
    );
    assert_eq!(
        got.actions.state_delta.unwrap()["count"].as_f64(),
        Some(3.0),
        "StateDelta[count]"
    );
}

#[test]
fn test_event_needs_raw_event() {
    let cases = [
        (
            "plain event",
            Event {
                author: "user".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "with content only",
            Event {
                response: LlmResponse {
                    content: Some(Content::from_text("hi", "user")),
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
        ),
        (
            "with output",
            Event {
                output: Some(json!("x")),
                ..Default::default()
            },
            true,
        ),
        (
            "with node info",
            Event {
                node_info: Some(NodeInfo {
                    path: "wf".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            true,
        ),
        (
            "with isolation scope",
            Event {
                isolation_scope: "s".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "with routes",
            Event {
                routes: vec!["approve".into()],
                ..Default::default()
            },
            true,
        ),
        (
            "with requested input",
            Event {
                requested_input: Some(RequestInput {
                    interrupt_id: "i".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            true,
        ),
        (
            "with input transcription",
            Event {
                response: LlmResponse {
                    input_transcription: Some(Transcription {
                        text: "hi".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
        ),
        (
            "with output transcription",
            Event {
                response: LlmResponse {
                    output_transcription: Some(Transcription {
                        text: "hi".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
        ),
    ];
    for (name, event, want) in cases {
        assert_eq!(
            event_needs_raw_event(&event),
            want,
            "{name}: event_needs_raw_event()"
        );
    }
}

#[test]
fn test_aiplatform_to_genai_content_preserves_function_i_ds() {
    let content = content_from_wire(&json!({
        "role": "model",
        "parts": [
            {"functionCall": {"id": "call-123", "name": "get_weather", "args": {"city": "Stockholm"}}},
            {"functionResponse": {"id": "call-123", "name": "get_weather", "response": {"temperature": 21}}}
        ]
    }));

    assert_eq!(content.parts.len(), 2, "len(content.parts)");
    let function_call = content.parts[0]
        .function_call
        .as_ref()
        .expect("content.parts[0].function_call is None");
    assert_eq!(function_call.id, "call-123", "FunctionCall.ID");
    let function_response = content.parts[1]
        .function_response
        .as_ref()
        .expect("content.parts[1].function_response is None");
    assert_eq!(function_response.id, "call-123", "FunctionResponse.ID");
}

// parity: adapted: Go converts arbitrary Go values through encoding/json; the port converts their serde_json values (Go nil maps and nil are null)
#[test]
fn test_to_struct_pb() {
    #[derive(Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct PlainFields {
        string_value: String,
        bool_value: bool,
        int_value: i32,
        array_value: Vec<String>,
    }
    #[derive(Serialize)]
    struct TaggedFields {
        string_value: String,
        bool_value: bool,
        int_value: i32,
        array_value: Vec<String>,
        #[serde(skip_serializing_if = "String::is_empty")]
        empty_string_value: String,
    }
    type Validate = fn(&Value);
    let cases: [(&str, Value, bool, Option<Validate>); 6] = [
        (
            "simple map representing function call args or function response",
            json!({"city": "Stockholm"}),
            false,
            Some(|s| assert_eq!(s["city"], "Stockholm", "city")),
        ),
        ("invalid input", json!("hello"), true, None),
        (
            "nil map converts to an empty struct",
            Value::Null,
            false,
            Some(|s| assert_eq!(s.as_object().map(Map::len), Some(0), "len(fields)")),
        ),
        (
            "untyped nil converts to an empty struct",
            Value::Null,
            false,
            Some(|s| assert_eq!(s.as_object().map(Map::len), Some(0), "len(fields)")),
        ),
        (
            "custom struct representing possible state delta",
            serde_json::to_value(PlainFields {
                string_value: "value".into(),
                bool_value: false,
                int_value: 123,
                array_value: vec!["value".into()],
            })
            .unwrap(),
            false,
            Some(|s| {
                for key in ["StringValue", "BoolValue", "IntValue", "ArrayValue"] {
                    assert!(s.get(key).is_some(), "expected '{key}' field to exist");
                }
            }),
        ),
        (
            "custom struct representing possible state delta respects json tags and omitempty",
            serde_json::to_value(TaggedFields {
                string_value: "value".into(),
                bool_value: false,
                int_value: 123,
                array_value: vec!["value".into()],
                empty_string_value: String::new(),
            })
            .unwrap(),
            false,
            Some(|s| {
                for key in ["string_value", "bool_value", "int_value", "array_value"] {
                    assert!(s.get(key).is_some(), "expected '{key}' field to exist");
                }
                assert!(
                    s.get("empty_string_value").is_none(),
                    "unexpected 'empty_string_value' field"
                );
            }),
        ),
    ];

    for (name, input, expect_error, validate) in cases {
        let result = to_struct(&input);
        assert_eq!(
            result.is_err(),
            expect_error,
            "{name}: to_struct() = {result:?}"
        );
        if let (Ok(s), Some(validate)) = (&result, validate) {
            validate(s);
        }
    }
}

#[test]
fn test_create_aiplatformpb_content() {
    let cases = [
        (
            "simple function call args",
            Content::from_parts(
                vec![Part::from_function_call(
                    "tool",
                    obj(json!({"city": "Stockholm"})),
                )],
                "user",
            ),
        ),
        (
            // Regression: a tool declaring no parameters yields a function call with no args.
            "function call with nil args",
            Content::from_parts(
                vec![Part {
                    function_call: Some(FunctionCall {
                        id: "call-1".into(),
                        name: "get_time".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                "model",
            ),
        ),
        (
            // Regression: a tool returning no payload yields a function response with no response.
            "function response with nil response",
            Content::from_parts(
                vec![Part {
                    function_response: Some(FunctionResponse {
                        id: "call-1".into(),
                        name: "get_time".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                "user",
            ),
        ),
        (
            "simple function response",
            Content::from_parts(
                vec![Part::from_function_response(
                    "tool",
                    obj(json!({"city": "Stockholm"})),
                )],
                "user",
            ),
        ),
        (
            "executable code",
            Content::from_parts(
                vec![Part::from_executable_code(
                    "print('hello')",
                    Language::Python,
                )],
                "model",
            ),
        ),
        (
            "code execution result",
            Content::from_parts(
                vec![Part::from_code_execution_result(Outcome::Ok, "hello")],
                "user",
            ),
        ),
        (
            "file data",
            Content::from_parts(
                vec![Part::from_uri("gs://bucket/file.txt", "text/plain")],
                "user",
            ),
        ),
    ];
    for (name, content) in cases {
        let result = content_to_wire(&content);
        assert!(
            result.is_ok(),
            "{name}: content_to_wire() error = {:?}",
            result.err()
        );
    }
}

#[test]
fn test_create_aiplatformpb_metadata() {
    let event = Event {
        response: LlmResponse {
            custom_metadata: obj(json!({"key": "value"})),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = metadata_to_wire(&event);
    assert!(
        result.is_ok(),
        "simple custom metadata: metadata_to_wire() error = {:?}",
        result.err()
    );
}

#[test]
fn test_event_actions_converters() {
    type Run = fn(&str);
    let cases: [(&str, Run); 4] = [
        ("session to aiplatform includes artifact delta", |name| {
            let want_state_delta = obj(json!({"user:theme": "dark"}));
            let event = Event {
                actions: EventActions {
                    state_delta: Some(want_state_delta.clone()),
                    artifact_delta: Some(BTreeMap::from([
                        ("chart.html".into(), 3),
                        ("table.csv".into(), 7),
                    ])),
                    ..Default::default()
                },
                ..Default::default()
            };
            let actions = actions_to_wire(&event)
                .unwrap_or_else(|e| panic!("{name}: actions_to_wire() failed: {e}"))
                .unwrap_or_else(|| panic!("{name}: actions_to_wire() returned None"));
            assert_eq!(
                actions["stateDelta"],
                Value::Object(want_state_delta),
                "{name}: actions.stateDelta"
            );
            assert_eq!(
                actions["artifactDelta"],
                json!({"chart.html": 3, "table.csv": 7}),
                "{name}: actions.artifactDelta"
            );
        }),
        (
            "session to aiplatform rejects artifact version outside int32",
            |name| {
                let event = Event {
                    actions: EventActions {
                        artifact_delta: Some(BTreeMap::from([(
                            "chart.html".into(),
                            i64::from(i32::MAX) + 1,
                        )])),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let Err(err) = actions_to_wire(&event) else {
                    panic!("{name}: actions_to_wire() succeeded, want error");
                };
                assert_eq!(
                    err.to_string(),
                    r#"artifact "chart.html" version 2147483648 does not fit in int32"#,
                    "{name}"
                );
            },
        ),
        ("aiplatform to session includes artifact delta", |name| {
            let want = EventActions {
                state_delta: Some(obj(json!({"user:theme": "dark"}))),
                artifact_delta: Some(BTreeMap::from([
                    ("chart.html".into(), 3),
                    ("table.csv".into(), 7),
                ])),
                ..Default::default()
            };
            let actions = actions_from_wire(Some(&json!({
                "stateDelta": {"user:theme": "dark"},
                "artifactDelta": {"chart.html": 3, "table.csv": 7}
            })))
            .unwrap_or_else(|e| panic!("{name}: actions_from_wire() failed: {e}"));
            assert_eq!(actions, want, "{name}: actions mismatch");
        }),
        ("aiplatform to session nil actions", |name| {
            let actions = actions_from_wire(None)
                .unwrap_or_else(|e| panic!("{name}: actions_from_wire() failed: {e}"));
            assert_eq!(actions, EventActions::default(), "{name}: actions mismatch");
        }),
    ];
    for (name, run) in cases {
        run(name);
    }
}

#[test]
fn test_aiplatform_to_genai_content_other_parts() {
    let input = json!({"content": {"role": "model", "parts": [
        {"executableCode": {"code": "print('hello')", "language": 1}},
        {"codeExecutionResult": {"outcome": 1, "output": "hello"}},
        {"fileData": {"mimeType": "text/plain", "fileUri": "gs://bucket/file.txt"}}
    ]}});

    let got = content_from_wire(&input["content"]);
    let want = Content {
        role: "model".into(),
        parts: vec![
            Part::from_executable_code("print('hello')", Language::Python),
            Part::from_code_execution_result(Outcome::Ok, "hello"),
            Part::from_uri("gs://bucket/file.txt", "text/plain"),
        ],
    };
    assert_eq!(got, want, "content_from_wire() mismatch");
}
