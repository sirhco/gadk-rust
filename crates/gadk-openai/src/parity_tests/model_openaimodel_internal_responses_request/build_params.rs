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

//! `responses/request_test.go`: the build_params tests (see `mod.rs`).
//!
//! Go asserts on openai-go param structs; these assert on the encoded request (what
//! [`crate::client::encode_body`] sends), byte for byte where Go's probe of the same request
//! gave the bytes. Go's `describeInput` renders the encoded input items, so a field that goes
//! missing on the wire fails the case.

use bytes::Bytes;
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    GenerateContentConfig, MediaProcessing, Part, PartMediaResolution, PartMediaResolutionLevel,
    Schema, SpeechMetadata, Tool, ToolCall, ToolResponse, Transcription, Type, VideoMetadata,
};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::error::{ErrorKind, OpenAiError};
use crate::parity_tests::model_openaimodel_internal_responses_tools::{wire, wire_value};
use crate::responses::request::{build_params, new_function_response, normalize_role};
use crate::responses::wire::{EasyInputMessageRole, ResponseInput, ResponseNewParams};
use crate::shared::calls::CallTracker;
use crate::shared::contents::{PartField, replayed_reasoning, unsupported_payload};

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// genai `NewContentFromText`.
fn text_content(text: &str, role: &str) -> Content {
    Content::from_text(text, role)
}

fn request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest {
        contents,
        ..LlmRequest::default()
    }
}

fn text(t: &str) -> Part {
    Part {
        text: t.to_string(),
        ..Part::default()
    }
}

fn thought(t: &str) -> Part {
    Part {
        text: t.to_string(),
        thought: true,
        ..Part::default()
    }
}

fn turn(role: &str, parts: Vec<Part>) -> Content {
    Content {
        parts,
        role: role.to_string(),
    }
}

/// Go `userContent` / `userTurn`.
fn user_turn(parts: Vec<Part>) -> Content {
    turn("user", parts)
}

/// Go `modelContent` / `modelTurn`.
fn model_turn(parts: Vec<Part>) -> Content {
    turn("model", parts)
}

fn call(name: &str, id: &str) -> FunctionCall {
    FunctionCall {
        name: name.to_string(),
        id: id.to_string(),
        ..FunctionCall::default()
    }
}

fn response(name: &str, id: &str, payload: Value) -> FunctionResponse {
    FunctionResponse {
        name: name.to_string(),
        id: id.to_string(),
        response: object(payload),
        ..FunctionResponse::default()
    }
}

fn blob(mime_type: &str, data: &'static [u8]) -> Blob {
    Blob {
        mime_type: mime_type.to_string(),
        data: Bytes::from_static(data),
        ..Blob::default()
    }
}

fn png() -> Blob {
    blob("image/png", &[1])
}

fn file_data(mime_type: &str, file_uri: &str) -> FileData {
    FileData {
        mime_type: mime_type.to_string(),
        file_uri: file_uri.to_string(),
        ..FileData::default()
    }
}

fn sig() -> Bytes {
    Bytes::from_static(b"sig")
}

/// Go `err == shared.ErrNoContents`: the bare sentinel, not a wrap of it.
fn is_bare(err: &OpenAiError, kind: ErrorKind) -> bool {
    err.kinds() == [kind] && err.to_string() == kind.sentinel_text()
}

/// The encoded `input` items of `params`.
fn input_items(params: &ResponseNewParams) -> Vec<Value> {
    match wire_value(params).get("input") {
        Some(Value::Array(items)) => items.clone(),
        other => panic!("input is not an item list: {other:?}"),
    }
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// What Go's `item.OfX != nil` tells apart, read off an encoded item: an output message is the
/// `message` carrying a `status`, which an easy input message never has.
fn item_kind(item: &Value) -> &str {
    match str_of(item, "type") {
        "message" if item.get("status").is_some() => "output_message",
        kind => kind,
    }
}

/// Go `describeInput`: `in/<role>:<a>|<b>` for an input message, `out/assistant:<a>|<b>` for an
/// output message, `call:<name>/<id>`, `output:<id>`.
fn describe_input(params: &ResponseNewParams) -> Vec<String> {
    input_items(params)
        .iter()
        .map(|item| match item_kind(item) {
            "message" => {
                let texts: Vec<String> = content_of(item)
                    .iter()
                    .map(|c| match str_of(c, "type") {
                        "input_text" => str_of(c, "text").to_string(),
                        "input_image" => describe_image(c),
                        "input_file" => describe_file(c),
                        _ => "<non-text>".to_string(),
                    })
                    .collect();
                format!("in/{}:{}", str_of(item, "role"), texts.join("|"))
            }
            "output_message" => {
                let texts: Vec<String> = content_of(item)
                    .iter()
                    .map(|c| match str_of(c, "type") {
                        "output_text" => str_of(c, "text").to_string(),
                        _ => "<non-text>".to_string(),
                    })
                    .collect();
                format!("out/assistant:{}", texts.join("|"))
            }
            "function_call" => format!("call:{}/{}", str_of(item, "name"), str_of(item, "call_id")),
            "function_call_output" => format!("output:{}", str_of(item, "call_id")),
            _ => "<unrecognized item>".to_string(),
        })
        .collect()
}

fn content_of(item: &Value) -> Vec<Value> {
    match item.get("content") {
        Some(Value::Array(content)) => content.clone(),
        _ => Vec::new(),
    }
}

/// Go `describeImage`: `img/<detail>:<field>=<value>`; presence is the key being on the wire.
fn describe_image(image: &Value) -> String {
    let reference = if image.get("file_id").is_some() {
        format!("file_id={}", str_of(image, "file_id"))
    } else if image.get("image_url").is_some() {
        format!("image_url={}", str_of(image, "image_url"))
    } else {
        "<no reference>".to_string()
    };
    format!("img/{}:{reference}", str_of(image, "detail"))
}

/// Go `describeFile`: `file:<field>=<value>`, with `#<name>` when a filename rode along.
fn describe_file(file: &Value) -> String {
    let mut reference = if file.get("file_id").is_some() {
        format!("file_id={}", str_of(file, "file_id"))
    } else if file.get("file_data").is_some() {
        format!("file_data={}", str_of(file, "file_data"))
    } else if file.get("file_url").is_some() {
        format!("file_url={}", str_of(file, "file_url"))
    } else {
        "<no reference>".to_string()
    };
    if file.get("filename").is_some() {
        reference.push('#');
        reference.push_str(str_of(file, "filename"));
    }
    format!("file:{reference}")
}

/// Sets one `genai.Part` field to a non-zero value.
type PartSetter = fn(&mut Part);

/// Go's reflection walk over `genai.Part`: every field's Go name, in declaration order, and a
/// setter making it non-zero as Go's `nonZero` does (a pointer to a zero struct, a one-element
/// slice, `"x"`, `true`). Destructured exhaustively, so a field genai adds fails to compile
/// here until the walk covers it.
fn part_fields() -> Vec<(&'static str, PartSetter)> {
    let Part {
        media_resolution: _,
        code_execution_result: _,
        executable_code: _,
        file_data: _,
        function_call: _,
        function_response: _,
        inline_data: _,
        text: _,
        thought: _,
        thought_signature: _,
        video_metadata: _,
        tool_call: _,
        tool_response: _,
        part_metadata: _,
        audio_transcription: _,
        media_processing: _,
        speech_metadata: _,
    } = Part::default();
    vec![
        ("MediaResolution", |p| {
            p.media_resolution = Some(PartMediaResolution::default());
        }),
        ("CodeExecutionResult", |p| {
            p.code_execution_result = Some(CodeExecutionResult::default());
        }),
        ("ExecutableCode", |p| {
            p.executable_code = Some(ExecutableCode::default());
        }),
        ("FileData", |p| p.file_data = Some(FileData::default())),
        ("FunctionCall", |p| {
            p.function_call = Some(FunctionCall::default());
        }),
        ("FunctionResponse", |p| {
            p.function_response = Some(FunctionResponse::default());
        }),
        ("InlineData", |p| p.inline_data = Some(Blob::default())),
        ("Text", |p| p.text = "x".to_string()),
        ("Thought", |p| p.thought = true),
        ("ThoughtSignature", |p| {
            p.thought_signature = Bytes::from_static(&[0]);
        }),
        ("VideoMetadata", |p| {
            p.video_metadata = Some(VideoMetadata::default());
        }),
        ("ToolCall", |p| p.tool_call = Some(ToolCall::default())),
        ("ToolResponse", |p| {
            p.tool_response = Some(ToolResponse::default());
        }),
        // Go's `reflect.MakeMap` is an empty non-nil map, which Go counts as set; a gadk map is
        // set when it holds an entry.
        ("PartMetadata", |p| {
            p.part_metadata = object(json!({"x": "x"}));
        }),
        ("AudioTranscription", |p| {
            p.audio_transcription = Some(Transcription::default());
        }),
        ("MediaProcessing", |p| {
            p.media_processing = Some("x".parse::<MediaProcessing>().unwrap());
        }),
        ("SpeechMetadata", |p| {
            p.speech_metadata = Some(SpeechMetadata::default());
        }),
    ]
}

#[test]
fn test_build_params_text() {
    let req = LlmRequest {
        model: "gpt-4o-mini".to_string(),
        contents: vec![text_content("ping", "user")],
        ..LlmRequest::default()
    };
    let params = build_params("fallback", &req).unwrap();
    assert_eq!(params.model, "gpt-4o-mini");
    assert_eq!(describe_input(&params), ["in/user:ping"]);
    // The whole body, as openai-go encodes the same params.
    assert_eq!(
        wire(&params),
        r#"{"input":[{"content":[{"text":"ping","type":"input_text"}],"role":"user","type":"message"}],"model":"gpt-4o-mini"}"#
    );
}

// TestBuildParams_MultiTurnAssistantUsesOutputText guards that a replayed assistant turn is
// serialized as an output message with content type "output_text": "input_text" on the
// assistant role makes the Responses API reject every multi-turn request.
#[test]
fn test_build_params_multi_turn_assistant_uses_output_text() {
    let req = LlmRequest {
        model: "gpt-4o-mini".to_string(),
        contents: vec![
            text_content("hi", "user"),
            text_content("hello there", "model"),
            text_content("can you code", "user"),
        ],
        ..LlmRequest::default()
    };
    let params = build_params("fallback", &req).unwrap();
    let items = input_items(&params);
    assert_eq!(
        items.len(),
        3,
        "got {} input items, want 3: {items:?}",
        items.len()
    );

    // User turns remain easy input messages using input_text.
    assert_eq!(item_kind(&items[0]), "message");
    assert_eq!(item_kind(&items[2]), "message");
    assert_eq!(items[0]["content"][0]["type"], "input_text");

    // The assistant turn is an output message using output_text, with no id (OpenAI mints
    // message ids on output) and status completed.
    let raw = match &params.input {
        Some(ResponseInput::InputItemList(list)) => wire(&list[1]),
        other => panic!("input is not an item list: {other:?}"),
    };
    assert!(
        raw.contains(r#""output_text""#),
        "assistant item JSON missing output_text: {raw}"
    );
    assert!(
        !raw.contains(r#""input_text""#),
        "assistant item JSON must not contain input_text: {raw}"
    );
    assert!(
        items[1].get("id").is_none(),
        "assistant item must not carry an id: {raw}"
    );
    assert_eq!(items[1]["status"], "completed", "{raw}");
    assert_eq!(items[1]["role"], "assistant", "{raw}");
    assert_eq!(
        raw,
        r#"{"content":[{"text":"hello there","type":"output_text"}],"status":"completed","role":"assistant","type":"message"}"#
    );
}

// TestBuildParams_ItemOrdering pins the order in which a model turn's parts become input
// items: buffered text is flushed immediately before a call is appended.
#[test]
fn test_build_params_item_ordering() {
    let call_part = || Part {
        function_call: Some(call("lookup", "call_1")),
        ..Part::default()
    };
    let tests: Vec<(&str, Vec<Part>, Vec<&str>)> = vec![
        (
            "text before call is flushed first",
            vec![text("Let me check."), call_part()],
            vec!["output_message", "function_call"],
        ),
        (
            "text after call is flushed last",
            vec![call_part(), text("Checking now.")],
            vec!["function_call", "output_message"],
        ),
    ];
    for (name, parts, want) in tests {
        let params = build_params("fallback", &request(vec![model_turn(parts)])).unwrap();
        let got: Vec<String> = input_items(&params)
            .iter()
            .map(|item| item_kind(item).to_string())
            .collect();
        assert_eq!(got, want, "{name}: item order");
    }
}

#[test]
fn test_build_params_function_call() {
    let req = request(vec![model_turn(vec![
        Part {
            function_call: Some(FunctionCall {
                name: "lookup".to_string(),
                args: object(json!({"city": "Paris"})),
                ..FunctionCall::default()
            }),
            ..Part::default()
        },
        Part {
            function_response: Some(response("lookup", "", json!({"temp": 72}))),
            ..Part::default()
        },
    ])]);
    let params = build_params("fallback", &req).unwrap();
    let items = input_items(&params);
    let find = |kind: &str| {
        items
            .iter()
            .find(|item| item_kind(item) == kind)
            .unwrap_or_else(|| panic!("missing {kind} in {items:?}"))
            .clone()
    };
    let (call_item, response_item) = (find("function_call"), find("function_call_output"));
    let call_id = str_of(&call_item, "call_id");
    assert!(
        !call_id.is_empty() && response_item.get("call_id").is_some(),
        "call IDs must be populated: call={call_item} response={response_item}"
    );
    assert_eq!(
        call_id,
        str_of(&response_item, "call_id"),
        "call IDs mismatch"
    );
    assert_eq!(
        wire(&params),
        r#"{"input":[{"arguments":"{\"city\":\"Paris\"}","call_id":"adk-openai-call-0","name":"lookup","type":"function_call"},{"output":"{\"temp\":72}","call_id":"adk-openai-call-0","type":"function_call_output"}],"model":"fallback"}"#
    );
}

fn object_schema(properties: Vec<(&str, Schema)>) -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: properties
            .into_iter()
            .map(|(name, schema)| (name.to_string(), schema))
            .collect(),
        ..Schema::default()
    }
}

fn typed(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Schema::default()
    }
}

#[test]
fn test_build_params_json_schema() {
    let req = LlmRequest {
        contents: vec![text_content("respond JSON", "user")],
        config: Some(GenerateContentConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(object_schema(vec![("answer", typed(Type::String))])),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    let params = build_params("fallback", &req).unwrap();
    let format = &wire_value(&params)["text"]["format"];
    assert_eq!(
        format["type"], "json_schema",
        "expected json schema format, got: {format}"
    );
    assert_eq!(format["schema"]["type"], "object", "schema mismatch");
}

// TestBuildParams_JSONSchemaPropertylessObjectOnTheWire checks the serialized body, because
// omission rules decide what the API receives: a property-less object reaching OpenAI without
// all three keys is rejected with a 400.
#[test]
fn test_build_params_json_schema_propertyless_object_on_the_wire() {
    let req = LlmRequest {
        contents: vec![text_content("respond JSON", "user")],
        config: Some(GenerateContentConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(object_schema(vec![
                ("city", typed(Type::String)),
                ("audit", typed(Type::Object)),
            ])),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    let params = build_params("fallback", &req).unwrap();
    // Failures report the schema alone, never the request, which carries the prompt.
    let sent = wire_value(&params)["text"]["format"]["schema"].clone();
    let audit = match sent.pointer("/properties/audit") {
        Some(audit @ Value::Object(_)) => audit.clone(),
        _ => panic!("audit missing from the request schema: {sent}"),
    };
    assert_eq!(audit["type"], "object", "audit.type");
    // Presence matters: the API rejects the request when a key is missing.
    assert_eq!(
        audit.get("properties"),
        Some(&json!({})),
        "audit.properties, want an empty object: {sent}"
    );
    assert_eq!(
        audit.get("additionalProperties"),
        Some(&json!(false)),
        "audit.additionalProperties: {sent}"
    );
    assert_eq!(
        audit.get("required"),
        Some(&json!([])),
        "audit.required, want an empty array: {sent}"
    );
}

// TestBuildParams_ToolsPinStrictOff checks the strict flag on the request body: the
// declaration is already strict-compatible, the case the API would normalize into strict mode.
#[test]
fn test_build_params_tools_pin_strict_off() {
    let req = LlmRequest {
        contents: vec![text_content("weather?", "user")],
        config: Some(GenerateContentConfig {
            tools: vec![Tool {
                function_declarations: vec![gadk_genai::FunctionDeclaration {
                    name: "get_weather".to_string(),
                    parameters_json_schema: Some(json!({
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"],
                        "additionalProperties": false,
                    })),
                    ..gadk_genai::FunctionDeclaration::default()
                }],
                ..Tool::default()
            }],
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    };
    let params = build_params("fallback", &req).unwrap();
    let data = wire(&params);
    let payload = wire_value(&params);
    let tools = payload["tools"].as_array().cloned().unwrap_or_default();
    assert_eq!(tools.len(), 1, "tools = {}, want 1: {data}", tools.len());
    assert_eq!(
        tools[0].get("strict"),
        Some(&json!(false)),
        "strict missing or true in the request body: {data}"
    );
}

#[test]
fn test_build_params_unsupported_part() {
    // The leading turn makes this bite: alone, the unsupported part leaves the request empty,
    // so a build that skipped it would still fail with ErrNoContents.
    let req = request(vec![
        text_content("q", "user"),
        user_turn(vec![Part {
            executable_code: Some(ExecutableCode {
                code: "print(1)".to_string(),
                ..ExecutableCode::default()
            }),
            ..Part::default()
        }]),
    ]);
    let err =
        build_params("fallback", &req).expect_err("expected error for an executable code part");
    assert!(
        !err.is(ErrorKind::NoContents) && err.to_string().contains("unsupported content part"),
        "buildParams() err = {err}, want an unsupported-content-part error"
    );
}

struct Expect {
    want: Vec<&'static str>,
    want_err: Option<ErrorKind>,
    want_not_err: Option<ErrorKind>,
    want_err_text: &'static str,
}

fn items(want: Vec<&'static str>) -> Expect {
    Expect {
        want,
        want_err: None,
        want_not_err: None,
        want_err_text: "",
    }
}

fn fails(want_err: Option<ErrorKind>, want_err_text: &'static str) -> Expect {
    Expect {
        want: Vec::new(),
        want_err,
        want_not_err: None,
        want_err_text,
    }
}

/// The shared assertions of Go's table loops over `buildParams`.
fn check(name: &str, result: Result<ResponseNewParams, OpenAiError>, expect: &Expect) {
    if expect.want_err.is_some() || !expect.want_err_text.is_empty() {
        let err = match result {
            Ok(_) => panic!("{name}: buildParams() err = nil, want an error"),
            Err(err) => err,
        };
        if let Some(kind) = expect.want_err {
            assert!(
                err.is(kind),
                "{name}: buildParams() err = {err}, want {kind}"
            );
        }
        if let Some(kind) = expect.want_not_err {
            assert!(
                !err.is(kind),
                "{name}: buildParams() err = {err}, want it not to match {kind}"
            );
        }
        assert!(
            err.to_string().contains(expect.want_err_text),
            "{name}: buildParams() err = {err:?}, want it to mention {:?}",
            expect.want_err_text
        );
        assert!(
            !err.to_string().contains("SECRET"),
            "{name}: buildParams() err = {err} leaks the file uri"
        );
        return;
    }
    let params = match result {
        Ok(params) => params,
        Err(err) => panic!("{name}: buildParams() err = {err}"),
    };
    assert_eq!(describe_input(&params), expect.want, "{name}: input items");
}

#[test]
fn test_build_params_drops_replayed_thoughts() {
    let ok_response = || response("lookup", "c1", json!({"ok": true}));
    let tests: Vec<(&str, Vec<Content>, Expect)> = vec![
        (
            "thought_before_answer",
            vec![
                text_content("what is 2+2?", "user"),
                model_turn(vec![thought("do not reveal the scratchpad"), text("4")]),
                text_content("and 3+3?", "user"),
            ],
            items(vec![
                "in/user:what is 2+2?",
                "out/assistant:4",
                "in/user:and 3+3?",
            ]),
        ),
        // A turn that produced only reasoning contributes nothing.
        (
            "thought_only_turn",
            vec![
                text_content("q", "user"),
                model_turn(vec![thought("still thinking")]),
                text_content("q2", "user"),
            ],
            items(vec!["in/user:q", "in/user:q2"]),
        ),
        // The blank-skip is per text item, not per message.
        (
            "blank_text_is_skipped_beside_real_text",
            vec![model_turn(vec![text("   "), text("real")])],
            items(vec!["out/assistant:real"]),
        ),
        // The same on the user path (newMessage rather than newOutputMessage).
        (
            "blank_text_is_skipped_beside_real_text_in_user_turn",
            vec![user_turn(vec![text("   "), text("real")])],
            items(vec!["in/user:real"]),
        ),
        (
            "thought_between_answers",
            vec![model_turn(vec![text("A"), thought("T"), text("B")])],
            items(vec!["out/assistant:A|B"]),
        ),
        // Sub-agents and A2A peers can hand back a thought on a user turn.
        (
            "thought_in_user_turn",
            vec![user_turn(vec![thought("leaked"), text("real")])],
            items(vec!["in/user:real"]),
        ),
        // A thought carrying only a signature has no text to leak.
        (
            "signature_only_thought",
            vec![
                text_content("q", "user"),
                model_turn(vec![Part {
                    thought: true,
                    thought_signature: sig(),
                    ..Part::default()
                }]),
            ],
            items(vec!["in/user:q"]),
        ),
        // Dropping a thought-marked call would strand its response.
        (
            "thought_marked_call_and_response_survive",
            vec![
                model_turn(vec![Part {
                    thought: true,
                    function_call: Some(call("lookup", "c1")),
                    ..Part::default()
                }]),
                user_turn(vec![Part {
                    thought: true,
                    function_response: Some(ok_response()),
                    ..Part::default()
                }]),
            ],
            items(vec!["call:lookup/c1", "output:c1"]),
        ),
        // One part carrying both: the reasoning must not reach the wire, the call must.
        (
            "thought_text_riding_on_a_call",
            vec![
                text_content("q", "user"),
                model_turn(vec![Part {
                    function_call: Some(call("lookup", "c1")),
                    ..thought("scratchpad")
                }]),
                user_turn(vec![Part {
                    function_response: Some(ok_response()),
                    ..Part::default()
                }]),
            ],
            items(vec!["in/user:q", "call:lookup/c1", "output:c1"]),
        ),
        // Same shape on the response side.
        (
            "thought_text_riding_on_a_response",
            vec![
                model_turn(vec![Part {
                    function_call: Some(call("lookup", "c1")),
                    ..Part::default()
                }]),
                user_turn(vec![Part {
                    function_response: Some(ok_response()),
                    ..thought("scratchpad")
                }]),
            ],
            items(vec!["call:lookup/c1", "output:c1"]),
        ),
        // Ordinary text riding on a call is not reasoning: it keeps its place ahead of it.
        (
            "plain_text_riding_on_a_call_keeps_both",
            vec![model_turn(vec![Part {
                function_call: Some(call("lookup", "c1")),
                ..text("on it")
            }])],
            items(vec!["out/assistant:on it", "call:lookup/c1"]),
        ),
        // A bare thought signature has nowhere to go, but does not fail the conversation.
        (
            "signature_without_thought_marker_is_dropped",
            vec![
                text_content("q", "user"),
                model_turn(vec![Part {
                    thought_signature: sig(),
                    ..Part::default()
                }]),
            ],
            items(vec!["in/user:q"]),
        ),
        // Marking media as a thought must not make the media vanish with the reasoning.
        (
            "thought_marked_media_is_still_sent",
            vec![
                text_content("q", "user"),
                user_turn(vec![Part {
                    thought: true,
                    inline_data: Some(png()),
                    ..Part::default()
                }]),
            ],
            items(vec![
                "in/user:q",
                "in/user:img/auto:image_url=data:image/png;base64,AQ==",
            ]),
        ),
        // Reasoning text riding on media: the text is dropped and the image is not.
        (
            "thought_text_riding_on_media_drops_only_the_text",
            vec![
                text_content("q", "user"),
                user_turn(vec![Part {
                    inline_data: Some(png()),
                    ..thought("scratch")
                }]),
            ],
            items(vec![
                "in/user:q",
                "in/user:img/auto:image_url=data:image/png;base64,AQ==",
            ]),
        ),
        (
            "thought_text_riding_on_code_is_still_rejected",
            vec![
                text_content("q", "user"),
                model_turn(vec![Part {
                    executable_code: Some(ExecutableCode {
                        code: "print(1)".to_string(),
                        ..ExecutableCode::default()
                    }),
                    ..thought("scratch")
                }]),
            ],
            fails(None, "unsupported content part"),
        ),
        // Dropping the reasoning must not swallow the role error the same turn would have
        // raised had its text been an answer.
        (
            "unsupported_role_still_reported_on_a_thought_only_turn",
            vec![
                text_content("q", "user"),
                turn("assistant", vec![thought("scratch")]),
            ],
            fails(None, r#"unsupported role "assistant""#),
        ),
        // The same turn with the marker off.
        (
            "unsupported_role_reported_on_an_ordinary_turn",
            vec![
                text_content("q", "user"),
                turn("assistant", vec![text("answer")]),
            ],
            fails(None, r#"unsupported role "assistant""#),
        ),
        // And on a thought with no text to drop.
        (
            "unsupported_role_still_reported_on_a_textless_thought",
            vec![
                text_content("q", "user"),
                turn(
                    "assistant",
                    vec![Part {
                        thought: true,
                        ..Part::default()
                    }],
                ),
            ],
            fails(None, r#"unsupported role "assistant""#),
        ),
        // Same for a bare signature.
        (
            "unsupported_role_still_reported_on_a_bare_signature",
            vec![
                text_content("q", "user"),
                turn(
                    "assistant",
                    vec![Part {
                        thought_signature: sig(),
                        ..Part::default()
                    }],
                ),
            ],
            fails(None, r#"unsupported role "assistant""#),
        ),
        // An image riding on ordinary text keeps its place behind it.
        (
            "media_riding_on_plain_text_keeps_its_place",
            vec![user_turn(vec![Part {
                inline_data: Some(png()),
                ..text("describe this")
            }])],
            items(vec![
                "in/user:describe this|img/auto:image_url=data:image/png;base64,AQ==",
            ]),
        ),
        // A model turn replays as an output message, which has no field for an image.
        (
            "media_on_a_replayed_assistant_turn_is_refused",
            vec![model_turn(vec![Part {
                inline_data: Some(png()),
                ..text("here it is")
            }])],
            fails(Some(ErrorKind::MediaOnAssistantTurn), "image/png"),
        ),
        // A part that carries only bookkeeping is not reasoning, so it is reported.
        (
            "metadata_only_part_is_reported",
            vec![
                text_content("q", "user"),
                user_turn(vec![Part {
                    part_metadata: object(json!({"src": "a"})),
                    ..Part::default()
                }]),
            ],
            fails(None, "unsupported content part: carries nothing to send"),
        ),
        // The contract for a caller reaching convertContents directly: an empty part is
        // reported, not quietly skipped.
        (
            "empty_part_is_reported",
            vec![text_content("q", "user"), user_turn(vec![Part::default()])],
            fails(None, "unsupported content part: carries nothing to send"),
        ),
        // Text riding on a response that pairs with nothing: the pairing is enforced wherever
        // the response appears.
        (
            "orphan_response_riding_on_text_is_reported",
            vec![user_turn(vec![Part {
                function_response: Some(FunctionResponse {
                    name: "lookup".to_string(),
                    ..FunctionResponse::default()
                }),
                ..text("here you go")
            }])],
            fails(None, "missing call id"),
        ),
        // Same shape on the call arm: a call with no name is unsendable.
        (
            "nameless_call_riding_on_text_is_reported",
            vec![model_turn(vec![Part {
                function_call: Some(FunctionCall::default()),
                ..text("on it")
            }])],
            fails(Some(ErrorKind::FunctionCallMissingName), ""),
        ),
        // A request left empty by the drop says the drop emptied it.
        (
            "only_thoughts",
            vec![model_turn(vec![thought("scratch")])],
            fails(
                Some(ErrorKind::NoContents),
                "every part was dropped as replayed reasoning",
            ),
        ),
        // The wrap is keyed on a part having been dropped, so a turn mixing the two still
        // reports the drop.
        (
            "thought_and_blank_answer",
            vec![model_turn(vec![thought("scratch"), text("   ")])],
            fails(
                Some(ErrorKind::NoContents),
                "every part was dropped as replayed reasoning",
            ),
        ),
    ];
    for (name, contents, expect) in tests {
        check(name, build_params("fallback", &request(contents)), &expect);
    }
}

// TestBuildParams_Media drives every kind-and-source pair the classifier can produce through
// the content field this endpoint has for it.
#[test]
fn test_build_params_media() {
    const SECRET_URL: &str = "https://example.com/report.pdf?X-Amz-Signature=SECRET";
    let inline = |part_blob: Blob| Part {
        inline_data: Some(part_blob),
        ..Part::default()
    };
    let file = |fd: FileData| Part {
        file_data: Some(fd),
        ..Part::default()
    };
    let tests: Vec<(&str, &str, Part, Expect)> = vec![
        (
            "inline_image_becomes_a_data_url",
            "",
            inline(png()),
            items(vec![
                "in/user:img/auto:image_url=data:image/png;base64,AQ==",
            ]),
        ),
        // An uploaded image stays a file id instead of being handed over as a location.
        (
            "uploaded_image_stays_a_file_id",
            "",
            file(file_data("image/png", "file-abc123")),
            items(vec!["in/user:img/auto:file_id=file-abc123"]),
        ),
        (
            "remote_image_becomes_an_image_url",
            "",
            file(file_data("image/jpeg", "https://example.test/cat.jpg")),
            items(vec![
                "in/user:img/auto:image_url=https://example.test/cat.jpg",
            ]),
        ),
        (
            "inline_file_carries_its_display_name",
            "",
            inline(Blob {
                display_name: "report.pdf".to_string(),
                ..blob("application/pdf", &[1])
            }),
            items(vec![
                "in/user:file:file_data=data:application/pdf;base64,AQ==#report.pdf",
            ]),
        ),
        (
            "inline_file_without_a_display_name_falls_back_to_inline_data",
            "",
            inline(blob("application/pdf", &[1])),
            items(vec![
                "in/user:file:file_data=data:application/pdf;base64,AQ==#inline_data",
            ]),
        ),
        (
            "uploaded_file_stays_a_file_id",
            "",
            file(file_data("application/pdf", "file-xyz")),
            items(vec!["in/user:file:file_id=file-xyz"]),
        ),
        // The filename rides only with inline data: a file id already names the file.
        (
            "a_display_name_is_not_sent_with_a_file_id",
            "",
            file(FileData {
                display_name: "report.pdf".to_string(),
                ..file_data("application/pdf", "file-xyz")
            }),
            items(vec!["in/user:file:file_id=file-xyz"]),
        ),
        // input_file has a URL of its own; input_image reuses image_url.
        (
            "remote_file_becomes_a_file_url",
            "",
            file(file_data("application/pdf", "https://example.test/doc.pdf")),
            items(vec!["in/user:file:file_url=https://example.test/doc.pdf"]),
        ),
        // An unknown MIME type is a file: "file" is the endpoint's catch-all field.
        (
            "an_unrecognized_type_travels_as_a_file",
            "",
            inline(blob("application/x-tar", &[1])),
            items(vec![
                "in/user:file:file_data=data:application/x-tar;base64,AQ==#inline_data",
            ]),
        ),
        // Classified by the shared code, refused here, naming the declared type.
        (
            "audio_is_refused_by_its_declared_type",
            "",
            inline(blob("audio/mpeg", &[1])),
            fails(Some(ErrorKind::UnsupportedMimeType), "audio/mpeg"),
        ),
        (
            "video_is_refused_by_its_declared_type",
            "",
            file(file_data("video/mp4", "file-v1")),
            fails(Some(ErrorKind::UnsupportedMimeType), "video/mp4"),
        ),
        // Unlabeled bytes are refused rather than called application/octet-stream.
        (
            "inline_media_without_a_mime_type_is_refused",
            "",
            inline(blob("", &[1])),
            fails(Some(ErrorKind::MediaMimeTypeRequired), ""),
        ),
        (
            "file_data_without_a_mime_type_is_refused",
            "",
            file(file_data("", "file-abc")),
            fails(Some(ErrorKind::MediaMimeTypeRequired), ""),
        ),
        (
            "inline_media_without_bytes_is_refused",
            "",
            inline(blob("image/png", &[])),
            fails(Some(ErrorKind::MediaDataRequired), ""),
        ),
        (
            "file_data_without_a_uri_is_refused",
            "",
            file(file_data("image/png", "")),
            fails(Some(ErrorKind::MediaUriRequired), ""),
        ),
        // Both fields on one part are both sent, inline first (genai's order).
        (
            "a_part_carrying_both_fields_sends_both_in_order",
            "",
            Part {
                inline_data: Some(png()),
                file_data: Some(file_data("application/pdf", "file-p1")),
                ..Part::default()
            },
            items(vec![
                "in/user:img/auto:image_url=data:image/png;base64,AQ==|file:file_id=file-p1",
            ]),
        ),
        // System and developer messages take input_text only.
        (
            "image_in_a_system_message_is_refused",
            "system",
            inline(png()),
            fails(
                None,
                "media in a system message is not supported: image/png",
            ),
        ),
        (
            "image_in_a_developer_message_is_refused",
            "developer",
            inline(png()),
            fails(
                None,
                "media in a developer message is not supported: image/png",
            ),
        ),
        (
            "file_in_a_developer_message_is_refused",
            "developer",
            file(file_data("application/pdf", SECRET_URL)),
            fails(
                None,
                "media in a developer message is not supported: application/pdf",
            ),
        ),
        // The role is checked before the media type, so audio gets the role error.
        (
            "audio_in_a_system_message_gets_the_role_error",
            "system",
            inline(blob("audio/mpeg", &[1])),
            Expect {
                want_not_err: Some(ErrorKind::UnsupportedMimeType),
                ..fails(
                    None,
                    "media in a system message is not supported: audio/mpeg",
                )
            },
        ),
        (
            "text_in_a_system_message_is_kept",
            "system",
            text("be terse"),
            items(vec!["in/system:be terse"]),
        ),
        (
            "text_in_a_developer_message_is_kept",
            "developer",
            text("be terse"),
            items(vec!["in/developer:be terse"]),
        ),
    ];
    for (name, role, part, expect) in tests {
        let role = if role.is_empty() { "user" } else { role };
        let result = build_params("fallback", &request(vec![turn(role, vec![part])]));
        check(name, result, &expect);
    }
}

// TestBuildParams_MediaKeepsItsPlaceAroundACall: an image buffered in a turn leaves ahead of
// the call that follows it.
#[test]
fn test_build_params_media_keeps_its_place_around_a_call() {
    let params = build_params(
        "fallback",
        &request(vec![
            user_turn(vec![
                text("look at this"),
                Part {
                    inline_data: Some(png()),
                    ..Part::default()
                },
            ]),
            model_turn(vec![Part {
                function_call: Some(call("lookup", "c1")),
                ..Part::default()
            }]),
        ]),
    )
    .unwrap();
    assert_eq!(
        describe_input(&params),
        [
            "in/user:look at this|img/auto:image_url=data:image/png;base64,AQ==",
            "call:lookup/c1"
        ]
    );
}

// TestBuildParams_MediaInOnePartKeepsItsPlace: a single part carrying text, media and a call
// or a response is the text, then the media, in one message, then the call or its result.
#[test]
fn test_build_params_media_in_one_part_keeps_its_place() {
    let tests: Vec<(&str, Vec<Content>, Part, Vec<&str>)> = vec![
        (
            "call",
            Vec::new(),
            Part {
                inline_data: Some(png()),
                function_call: Some(call("lookup", "c1")),
                ..text("look at this")
            },
            vec![
                "in/user:look at this|img/auto:image_url=data:image/png;base64,AQ==",
                "call:lookup/c1",
            ],
        ),
        (
            "response",
            vec![model_turn(vec![Part {
                function_call: Some(call("lookup", "c1")),
                ..Part::default()
            }])],
            Part {
                inline_data: Some(png()),
                function_response: Some(response("lookup", "c1", json!({"ok": true}))),
                ..text("look at this")
            },
            vec![
                "call:lookup/c1",
                "in/user:look at this|img/auto:image_url=data:image/png;base64,AQ==",
                "output:c1",
            ],
        ),
    ];
    for (name, mut contents, part, want) in tests {
        contents.push(user_turn(vec![part]));
        let params = build_params("fallback", &request(contents))
            .unwrap_or_else(|err| panic!("{name}: buildParams() err = {err}"));
        assert_eq!(describe_input(&params), want, "{name}: input items");
    }
}

// TestBuildParams_ImageDetailReachesTheWire reads the detail off the encoded request: openai-go
// drops an unset detail, so only the bytes show it was sent.
#[test]
fn test_build_params_image_detail_reaches_the_wire() {
    let params = build_params(
        "fallback",
        &request(vec![user_turn(vec![Part {
            inline_data: Some(png()),
            ..Part::default()
        }])]),
    )
    .unwrap();
    let body = wire(&params);
    for want in [r#""detail":"auto""#, r#""type":"input_image""#] {
        assert!(body.contains(want), "encoded request is missing {want}");
    }
    assert_eq!(
        body,
        r#"{"input":[{"content":[{"detail":"auto","image_url":"data:image/png;base64,AQ==","type":"input_image"}],"role":"user","type":"message"}],"model":"fallback"}"#
    );
}

// TestBuildParams_NoContentsSentinelIdentity pins which requests get the bare ErrNoContents and
// which get it wrapped: only a drop that suppressed text the model would otherwise have seen
// earns the wrap.
// parity: adapted: a gadk `Content` holds `Part`s, not pointers, so "content_with_only_nil_parts" (Go: one nil part) is a content holding one empty part, which convert_contents reports (`carries nothing to send`) instead of skipping; that case asserts the report
#[test]
fn test_build_params_no_contents_sentinel_identity() {
    let model = |parts: Vec<Part>| vec![model_turn(parts)];
    let tests: Vec<(&str, Vec<Content>, bool)> = vec![
        ("nil_contents", Vec::new(), true),
        ("empty_contents", Vec::new(), true),
        ("content_with_no_parts", vec![user_turn(Vec::new())], true),
        // Nothing was dropped as reasoning: the text was accepted, then skipped as blank.
        ("only_blank_text", model(vec![text("   ")]), true),
        // The part is reasoning, but its text was blank, so the drop did not empty the request.
        ("only_blank_reasoning", model(vec![thought("   ")]), true),
        // A thought with nothing to suppress.
        (
            "only_bare_thought",
            model(vec![Part {
                thought: true,
                ..Part::default()
            }]),
            true,
        ),
        (
            "only_signature",
            model(vec![Part {
                thought_signature: sig(),
                ..Part::default()
            }]),
            true,
        ),
        ("only_reasoning", model(vec![thought("scratch")]), false),
        // The drop still earns the wrap when it suppressed real text.
        (
            "reasoning_and_blank_answer",
            model(vec![thought("scratch"), text("   ")]),
            false,
        ),
        // The flag accumulates rather than tracking the last part.
        (
            "real_reasoning_then_blank_reasoning",
            model(vec![thought("scratch"), thought("   ")]),
            false,
        ),
        // A flag reset per content block would show up here.
        (
            "real_reasoning_turn_then_blank_one",
            vec![
                model_turn(vec![thought("scratch")]),
                model_turn(vec![thought("   ")]),
            ],
            false,
        ),
        // The user path builds an input message and skips blank text elsewhere.
        (
            "only_blank_text_user_turn",
            vec![user_turn(vec![text("   ")])],
            true,
        ),
    ];
    for (name, contents, want_bare) in tests {
        let err = build_params("fallback", &request(contents)).unwrap_err();
        assert!(
            err.is(ErrorKind::NoContents),
            "{name}: buildParams() err = {err}, want it to wrap ErrNoContents"
        );
        assert_eq!(
            is_bare(&err, ErrorKind::NoContents),
            want_bare,
            "{name}: err == ErrNoContents (err = {err:?})"
        );
    }

    let err =
        build_params("fallback", &request(vec![user_turn(vec![Part::default()])])).unwrap_err();
    assert_eq!(
        err.to_string(),
        "openai: unsupported content part: carries nothing to send",
        "content_with_only_nil_parts"
    );
}

// parity: adapted: replayed_reasoning takes a `&Part`, so Go's "nil_part" case asks about an empty part, which Go answers the same way (false)
#[test]
fn test_replayed_reasoning() {
    let thought_with = |part: Part| Part {
        thought: true,
        ..part
    };
    let tests: Vec<(&str, Part, bool)> = vec![
        ("nil_part", Part::default(), false),
        // Nothing marks these as reasoning: convertContents reports them instead.
        ("empty_part", Part::default(), false),
        (
            "metadata_only",
            Part {
                part_metadata: object(json!({"src": "a"})),
                ..Part::default()
            },
            false,
        ),
        ("thought_text", thought("scratch"), true),
        (
            "thought_signature_only",
            thought_with(Part {
                thought_signature: sig(),
                ..Part::default()
            }),
            true,
        ),
        ("bare_thought", thought_with(Part::default()), true),
        (
            "signature_without_thought_marker",
            Part {
                thought_signature: sig(),
                ..Part::default()
            },
            true,
        ),
        ("plain_text", text("answer"), false),
        (
            "signature_on_answer",
            Part {
                thought_signature: sig(),
                ..text("answer")
            },
            false,
        ),
        // Marking content as a thought must not make it vanish.
        (
            "thought_marked_inline_data",
            thought_with(Part {
                inline_data: Some(png()),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_file_data",
            thought_with(Part {
                file_data: Some(FileData {
                    file_uri: "gs://b/o".to_string(),
                    ..FileData::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_executable_code",
            thought_with(Part {
                executable_code: Some(ExecutableCode {
                    code: "print(1)".to_string(),
                    ..ExecutableCode::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_code_result",
            thought_with(Part {
                code_execution_result: Some(CodeExecutionResult {
                    output: "1".to_string(),
                    ..CodeExecutionResult::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_call",
            thought_with(Part {
                function_call: Some(call("f", "")),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_response",
            thought_with(Part {
                function_response: Some(FunctionResponse {
                    name: "f".to_string(),
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        // Server-side tool traffic and transcription are payload too.
        (
            "thought_marked_tool_call",
            thought_with(Part {
                tool_call: Some(ToolCall {
                    id: "t1".to_string(),
                    ..ToolCall::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_tool_response",
            thought_with(Part {
                tool_response: Some(ToolResponse {
                    id: "t1".to_string(),
                    ..ToolResponse::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        (
            "thought_marked_audio_transcription",
            thought_with(Part {
                audio_transcription: Some(Transcription {
                    text: "hello".to_string(),
                    ..Transcription::default()
                }),
                ..Part::default()
            }),
            false,
        ),
        // These three only qualify media carried in another field, so on a thought they hold
        // nothing back.
        (
            "thought_with_video_metadata",
            Part {
                video_metadata: Some(VideoMetadata {
                    fps: Some(2.0),
                    ..VideoMetadata::default()
                }),
                ..thought("scratch")
            },
            true,
        ),
        (
            "thought_with_media_resolution",
            Part {
                media_resolution: Some(PartMediaResolution {
                    level: Some(PartMediaResolutionLevel::MediaResolutionLow),
                    ..PartMediaResolution::default()
                }),
                ..thought("scratch")
            },
            true,
        ),
        (
            "thought_with_part_metadata",
            Part {
                part_metadata: object(json!({"src": "a"})),
                ..thought("scratch")
            },
            true,
        ),
    ];
    for (name, part, want) in tests {
        assert_eq!(
            replayed_reasoning(&part),
            want,
            "{name}: ReplayedReasoning()"
        );
    }
}

/// Go `accountedForFields`: the `genai.Part` fields this endpoint's request converter knows
/// what to do with, and why. Restated rather than derived from the production code, so a
/// field genai adds is absent from it by construction.
const ACCOUNTED_FOR_FIELDS: &[(&str, &str)] = &[
    ("Text", "sent, or dropped when it is reasoning"),
    ("Thought", "the marker deciding which"),
    (
        "ThoughtSignature",
        "no request field on either endpoint carries one",
    ),
    ("FunctionCall", "sent as a call"),
    ("FunctionResponse", "sent as the call's result"),
    ("VideoMetadata", "qualifies media carried in another field"),
    (
        "MediaResolution",
        "qualifies media carried in another field",
    ),
    ("PartMetadata", "caller bookkeeping, never content"),
    (
        "InlineData",
        "sent as an input_image or input_file carrying the bytes",
    ),
    (
        "FileData",
        "sent as an input_image or input_file naming an upload or a location",
    ),
];

fn accounted_for(field: &str) -> Option<&'static str> {
    ACCOUNTED_FOR_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, why)| *why)
}

/// Go `ridingShapes`: `part` alone and alongside each thing that would otherwise be emitted
/// for it.
fn riding_shapes(part: &Part) -> Vec<(&'static str, Part)> {
    vec![
        ("alone", part.clone()),
        (
            "on text",
            Part {
                text: "here you go".to_string(),
                ..part.clone()
            },
        ),
        (
            "on reasoning text",
            Part {
                text: "scratchpad".to_string(),
                thought: true,
                ..part.clone()
            },
        ),
        (
            "on a function call",
            Part {
                function_call: Some(call("lookup", "c1")),
                ..part.clone()
            },
        ),
    ]
}

// parity: adapted: Go reflects over `genai.Part`; the walk here is `part_fields`, an exhaustive destructuring of gadk's `Part` (a field genai adds fails to compile, Go's "unexported field" failure), each field set as Go's `nonZero` sets it
#[test]
fn test_unsupported_payload_walks_every_part_field() {
    for (field, set) in part_fields() {
        let mut part = Part::default();
        set(&mut part);
        let got = unsupported_payload(&part, &[PartField::Media]);
        if let Some(why) = accounted_for(field) {
            assert_eq!(
                got, None,
                "{field}: UnsupportedPayload() for a part carrying only {field}: {why}"
            );
            continue;
        }
        assert_eq!(
            got,
            Some(field),
            "{field}: a field the package cannot send must be reported, not dropped"
        );
        // The caller has to see the error, also when the field rides on something sendable.
        let want = format!("unsupported content part: {field}");
        for (ride, ridden) in riding_shapes(&part) {
            let result = build_params("fallback", &request(vec![model_turn(vec![ridden])]));
            match result {
                Err(err) if err.to_string().ends_with(&want) => {}
                other => panic!(
                    "{field}: buildParams() = {other:?} for {field} carried {ride}, want an error ending with {want:?}"
                ),
            }
        }
    }
}

// TestReplayedReasoning_EveryUnaccountedFieldDisqualifies is the same walk seen from the drop:
// a field the package cannot send keeps the part out of the drop and on to the rejection.
// parity: adapted: the walk is `part_fields` (see test_unsupported_payload_walks_every_part_field)
#[test]
fn test_replayed_reasoning_every_unaccounted_field_disqualifies() {
    for (field, set) in part_fields() {
        if accounted_for(field).is_some() {
            continue;
        }
        let mut part = thought("scratch");
        set(&mut part);
        assert!(
            !replayed_reasoning(&part),
            "{field}: ReplayedReasoning() = true for a thought carrying {field}; it would be dropped instead of rejected"
        );
    }
}

#[test]
fn test_call_tracker_new_function_response_unknown_call_id() {
    let mut tracker = CallTracker {
        pending: vec!["call-1".to_string()],
        ..CallTracker::default()
    };
    let fr = response("lookup", "call-missing", json!({"ok": true}));
    let err = new_function_response(&mut tracker, &fr).unwrap_err();
    assert!(
        err.to_string().contains("unknown or already completed"),
        "expected error for unknown call id, got {err}"
    );
    assert_eq!(
        tracker.pending,
        ["call-1"],
        "pending calls should remain untouched"
    );
}

/// Go `systemInstructionAcceptedFields`: the `genai.Part` fields a system-instruction part may
/// carry beside its text, restated rather than derived.
const SYSTEM_INSTRUCTION_ACCEPTED_FIELDS: &[(&str, &str)] = &[
    (
        "Thought",
        "a marker no wire field carries; the text is sent, as adk-python does",
    ),
    (
        "ThoughtSignature",
        "no request field on either endpoint carries one",
    ),
    ("PartMetadata", "caller bookkeeping, never content"),
    (
        "VideoMetadata",
        "qualifies media, and the part carries none",
    ),
    (
        "MediaResolution",
        "qualifies media, and the part carries none",
    ),
];

// Every genai.Part field riding on a system-instruction text part is either accepted above or
// refused by name, because instructions carries text only.
// parity: adapted: the walk is `part_fields` (see test_unsupported_payload_walks_every_part_field)
#[test]
fn test_build_params_system_instruction_walks_every_part_field() {
    for (field, set) in part_fields() {
        if field == "Text" {
            continue;
        }
        let mut part = text("describe it");
        set(&mut part);
        let req = LlmRequest {
            contents: vec![user_turn(vec![text("hi")])],
            config: Some(GenerateContentConfig {
                system_instruction: Some(Content {
                    parts: vec![part],
                    ..Content::default()
                }),
                ..GenerateContentConfig::default()
            }),
            ..LlmRequest::default()
        };
        let result = build_params("fallback", &req);
        let accepted = SYSTEM_INSTRUCTION_ACCEPTED_FIELDS
            .iter()
            .find(|(name, _)| *name == field);
        if let Some((_, why)) = accepted {
            let params = result.unwrap_or_else(|err| {
                panic!("{field}: buildParams() err = {err} for a text part carrying {field}, want nil: {why}")
            });
            assert_eq!(
                params.instructions.as_deref(),
                Some("describe it"),
                "{field}: Instructions for a text part carrying {field}: {why}"
            );
            assert_eq!(
                wire_value(&params)["instructions"],
                "describe it",
                "{field}"
            );
            continue;
        }
        // An ending rather than a substring: one field name can prefix another.
        let want = format!("unsupported system instruction part: {field}");
        match result {
            Err(err) if err.to_string().ends_with(&want) => {}
            other => panic!(
                "{field}: buildParams() = {other:?} for a text part carrying {field}, want an error ending with {want:?}"
            ),
        }
    }
}

#[test]
fn test_normalize_role() {
    let tests: Vec<(&str, Option<EasyInputMessageRole>)> = vec![
        ("", Some(EasyInputMessageRole::User)),
        ("user", Some(EasyInputMessageRole::User)),
        ("model", Some(EasyInputMessageRole::Assistant)),
        ("system", Some(EasyInputMessageRole::System)),
        ("developer", Some(EasyInputMessageRole::Developer)),
        ("invalid", None),
    ];
    for (role, want) in tests {
        let got = normalize_role(role);
        assert_eq!(
            got.is_err(),
            want.is_none(),
            "{role:?}: normalizeRole() = {got:?}"
        );
        assert_eq!(got.ok(), want, "{role:?}: normalizeRole()");
    }
    assert_eq!(
        normalize_role("invalid").unwrap_err().to_string(),
        r#"openai: unsupported role "invalid""#
    );
}
