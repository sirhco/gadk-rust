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

//! Port of ADK-Go `server/adka2a/v2/parts_test.go`.

#![allow(clippy::unwrap_used)]

use gadk_a2a::a2a;
use gadk_a2a::convert::{to_a2a_meta_key, to_a2a_parts, to_genai_parts};
use gadk_genai::{
    Blob, CodeExecutionResult, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    FunctionResponseScheduling, Language, Outcome, Part,
};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// Go `p.SetMeta(k, v)`.
fn with_meta(mut part: a2a::Part, key: &str, value: Value) -> a2a::Part {
    part.metadata
        .get_or_insert_with(Default::default)
        .insert(key.to_owned(), value);
    part
}

fn data_part(kind: &str, data: Value) -> a2a::Part {
    with_meta(a2a::Part::data(data), &to_a2a_meta_key("type"), json!(kind))
}

#[test]
fn test_parts_two_way_conversion() {
    let long_running_key = to_a2a_meta_key("is_long_running");
    let cases: Vec<(&str, a2a::Part, Part, Vec<String>)> = vec![
        (
            "text",
            a2a::Part::text("Hello"),
            Part {
                text: "Hello".into(),
                ..Default::default()
            },
            vec![],
        ),
        (
            "thought",
            with_meta(
                a2a::Part::text("Hello"),
                &to_a2a_meta_key("thought"),
                json!(true),
            ),
            Part {
                text: "Hello".into(),
                thought: true,
                ..Default::default()
            },
            vec![],
        ),
        (
            "file uri",
            a2a::Part::url("ftp://cat.com")
                .with_media_type("image/jpeg")
                .with_filename("cat.jpeg"),
            Part {
                file_data: Some(FileData {
                    file_uri: "ftp://cat.com".into(),
                    mime_type: "image/jpeg".into(),
                    display_name: "cat.jpeg".into(),
                }),
                ..Default::default()
            },
            vec![],
        ),
        (
            "file bytes",
            a2a::Part::raw(vec![0xff])
                .with_media_type("image/jpeg")
                .with_filename("cat.jpeg"),
            Part {
                inline_data: Some(Blob {
                    data: vec![0xff].into(),
                    mime_type: "image/jpeg".into(),
                    display_name: "cat.jpeg".into(),
                }),
                ..Default::default()
            },
            vec![],
        ),
        (
            "function call",
            with_meta(
                data_part(
                    "function_call",
                    json!({"id": "get_weather", "args": {"city": "Warsaw"}, "name": "GetWeather"}),
                ),
                &long_running_key,
                json!(false),
            ),
            Part {
                function_call: Some(FunctionCall {
                    id: "get_weather".into(),
                    args: obj(json!({"city": "Warsaw"})),
                    name: "GetWeather".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        ),
        (
            "long running function call",
            with_meta(
                data_part(
                    "function_call",
                    json!({"id": "get_weather", "args": {"city": "Warsaw"}, "name": "GetWeather"}),
                ),
                &long_running_key,
                json!(true),
            ),
            Part {
                function_call: Some(FunctionCall {
                    id: "get_weather".into(),
                    args: obj(json!({"city": "Warsaw"})),
                    name: "GetWeather".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec!["get_weather".into()],
        ),
        (
            "function response",
            data_part(
                "function_response",
                json!({
                    "id": "get_weather",
                    "scheduling": "INTERRUPT",
                    "response": {"temperature": "7C"},
                    "name": "GetWeather",
                }),
            ),
            Part {
                function_response: Some(FunctionResponse {
                    id: "get_weather".into(),
                    scheduling: Some(FunctionResponseScheduling::Interrupt),
                    response: obj(json!({"temperature": "7C"})),
                    name: "GetWeather".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        ),
        (
            "code execution result",
            data_part(
                "code_execution_result",
                json!({"outcome": "OUTCOME_OK", "output": "4"}),
            ),
            Part {
                code_execution_result: Some(CodeExecutionResult {
                    outcome: Some(Outcome::Ok),
                    output: "4".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        ),
        (
            // Go names this case "code execution result" too.
            "executable code",
            data_part(
                "executable_code",
                json!({"code": "print(2+2)", "language": "PYTHON"}),
            ),
            Part {
                executable_code: Some(ExecutableCode {
                    code: "print(2+2)".into(),
                    language: Some(Language::Python),
                    ..Default::default()
                }),
                ..Default::default()
            },
            vec![],
        ),
    ];

    for (name, a2a_part, genai_part, long_running_ids) in cases {
        let to_a2a = to_a2a_parts(std::slice::from_ref(&genai_part), &long_running_ids).unwrap();
        assert_eq!(to_a2a, vec![a2a_part.clone()], "{name}: to_a2a_parts");

        let to_genai = to_genai_parts(std::slice::from_ref(&a2a_part)).unwrap();
        assert_eq!(to_genai, vec![genai_part], "{name}: to_genai_parts");
    }
}

#[test]
fn test_parts_data_part_conversion_round_trip() {
    let a2a_part = a2a::Part::data(json!({"arbitrary": "data"}));
    let want_genai = Part {
        inline_data: Some(Blob {
            data: b"<a2a_datapart_json>{\"arbitrary\":\"data\"}</a2a_datapart_json>"
                .to_vec()
                .into(),
            mime_type: "text/plain".into(),
            ..Default::default()
        }),
        ..Default::default()
    };

    let got_genai = to_genai_parts(std::slice::from_ref(&a2a_part)).unwrap();
    assert_eq!(got_genai, vec![want_genai]);

    let got_back_a2a = to_a2a_parts(&got_genai, &[]).unwrap();
    assert_eq!(got_back_a2a, vec![a2a_part]);
}
