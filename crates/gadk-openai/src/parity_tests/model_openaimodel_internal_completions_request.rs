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

//! Port of ADK-Go `model/openaimodel/internal/completions/request_test.go`.
//!
//! Go marshals the params and decodes them back into a `map[string]any` (`requestWire`), so
//! assertions read the bytes openai-go sends; [`request_wire`] does the same with
//! [`encode_body`] (the bytes gadk sends) and a `serde_json::Map`, which, like Go's map, has
//! sorted keys. The helpers are shared with the `tools_test.go` and `media_test.go` ports, as
//! Go's are within the package.

use std::time::Duration;

use bytes::Bytes;
use gadk_genai::{
    AudioTranscriptionConfig, Blob, Content, FileData, FunctionCall, FunctionResponse,
    GenerateContentConfig, GenerationConfigRoutingConfig, ImageConfig, MediaResolution,
    ModelArmorConfig, ModelSelectionConfig, Part, PartMediaResolution, PartMediaResolutionLevel,
    SafetySetting, Schema, ServiceTier, SpeechConfig, ThinkingConfig, ThinkingLevel, Type,
    VideoMetadata,
};
use gadk_model::{HttpOptions, LlmRequest};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::client::encode_body;
use crate::completions::request::{build_params, build_params_with, supported_config_fields};
use crate::error::{ErrorKind, OpenAiError};
use crate::shared::config::{
    IGNORED_HTTP_OPTION_FIELDS, http_option_fields, reject_unsupported_fields,
    thinking_config_fields,
};
use crate::shared::schema::DEFAULT_RESPONSE_FORMAT_NAME;

use super::support::TEST_MODEL;

/// Go `requestWire`: the params [`build_params`] makes for `req` (model `gpt-4o-mini`), as the
/// JSON object gadk sends, decoded back into a map.
pub(super) fn request_wire(req: &LlmRequest) -> Map<String, Value> {
    let params =
        build_params(TEST_MODEL, req).unwrap_or_else(|err| panic!("buildParams() err = {err}"));
    let raw = encode_body(&params, false).unwrap_or_else(|err| panic!("marshal params: {err}"));
    serde_json::from_slice(&raw).unwrap_or_else(|err| panic!("unmarshal params: {err}"))
}

/// Go `wireMessages`: the `messages` array of a wire map, each an object.
pub(super) fn wire_messages(wire: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let Some(Value::Array(raw)) = wire.get("messages") else {
        panic!(
            "messages missing or not an array: {:?}",
            wire.get("messages")
        );
    };
    raw.iter()
        .map(|m| match m {
            Value::Object(msg) => msg.clone(),
            other => panic!("message is not an object: {other:?}"),
        })
        .collect()
}

/// Go's `m[key]` on a decoded map: the value, or `null` (Go's nil) when the key is absent.
pub(super) fn field(m: &Map<String, Value>, key: &str) -> Value {
    m.get(key).cloned().unwrap_or(Value::Null)
}

/// Go `userReq(parts...)`: one user content of `parts`.
pub(super) fn user_req(parts: Vec<Part>) -> LlmRequest {
    LlmRequest {
        contents: vec![content("user", parts)],
        ..LlmRequest::default()
    }
}

/// A content of `role` and `parts` (Go `&genai.Content{Role: role, Parts: parts}`).
pub(super) fn content(role: &str, parts: Vec<Part>) -> Content {
    Content {
        role: role.to_string(),
        parts,
    }
}

/// Go `&genai.Part{Text: text}`.
pub(super) fn text(text: &str) -> Part {
    Part {
        text: text.to_string(),
        ..Part::default()
    }
}

/// A request of one user "hi" turn and `cfg` (Go's `toolReq`, and the request most config
/// tests build inline).
pub(super) fn hi_req(cfg: GenerateContentConfig) -> LlmRequest {
    LlmRequest {
        contents: vec![content("user", vec![text("hi")])],
        config: Some(cfg),
        ..LlmRequest::default()
    }
}

/// Go `&genai.Part{FunctionCall: &genai.FunctionCall{ID: id, Name: name, Args: args}}`.
pub(super) fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(function_call(id, name, args)),
        ..Part::default()
    }
}

/// Go `&genai.FunctionCall{ID: id, Name: name, Args: args}` (`args` a JSON object or `null`).
fn function_call(id: &str, name: &str, args: Value) -> FunctionCall {
    FunctionCall {
        id: id.to_string(),
        name: name.to_string(),
        args: match args {
            Value::Object(map) => map,
            _ => Map::new(),
        },
        ..FunctionCall::default()
    }
}

/// Go `&genai.Part{FunctionResponse: &genai.FunctionResponse{ID: id, Name: name, Response:
/// response}}` (`response` a JSON object or `null`).
pub(super) fn response_part(id: &str, name: &str, response: Value) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            id: id.to_string(),
            name: name.to_string(),
            response: match response {
                Value::Object(map) => map,
                _ => Map::new(),
            },
            ..FunctionResponse::default()
        }),
        ..Part::default()
    }
}

/// The error [`build_params`] returns for `req` (Go's `_, err := buildParams("m", req)`).
pub(super) fn build_err(req: &LlmRequest) -> OpenAiError {
    match build_params("m", req) {
        Ok(params) => panic!("buildParams() err = nil, params = {params:?}"),
        Err(err) => err,
    }
}

/// Go `&genai.Part{InlineData: &genai.Blob{MIMEType: mime_type, Data: data}}`.
pub(super) fn inline_part(mime_type: &str, data: &'static [u8]) -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: mime_type.to_string(),
            data: Bytes::from_static(data),
            ..Blob::default()
        }),
        ..Part::default()
    }
}

/// Go `&genai.FileData{MIMEType: mime_type, FileURI: file_uri}`.
pub(super) fn file_data(mime_type: &str, file_uri: &str) -> FileData {
    FileData {
        mime_type: mime_type.to_string(),
        file_uri: file_uri.to_string(),
        ..FileData::default()
    }
}

/// `true` within 1e-6 of `want` (Go compares widened float32 values with this tolerance).
fn near(got: &Value, want: f64) -> bool {
    got.as_f64().is_some_and(|got| (got - want).abs() <= 1e-6)
}

#[test]
fn test_build_params_roles() {
    let tests = [
        ("empty defaults to user", "", "user"),
        ("user", "user", "user"),
        ("model becomes assistant", "model", "assistant"),
        ("system survives", "system", "system"),
        ("developer survives", "developer", "developer"),
    ];
    for (name, role, want_role) in tests {
        let wire = request_wire(&LlmRequest {
            contents: vec![content(role, vec![text("hi")])],
            ..LlmRequest::default()
        });
        let msgs = wire_messages(&wire);
        assert_eq!(msgs.len(), 1, "{name}: messages = {}, want 1", msgs.len());
        assert_eq!(field(&msgs[0], "role"), json!(want_role), "{name}: role");
        assert_eq!(field(&msgs[0], "content"), json!("hi"), "{name}: content");
    }
}

#[test]
fn test_build_params_unsupported_role() {
    let err = build_err(&LlmRequest {
        contents: vec![content("wizard", vec![text("hi")])],
        ..LlmRequest::default()
    });
    assert!(
        err.to_string().contains("wizard"),
        "err = {err}, want one naming the role"
    );
}

#[test]
fn test_build_params_system_instruction_leads_messages() {
    let wire = request_wire(&LlmRequest {
        contents: vec![content("user", vec![text("hi")])],
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content::from_text("be terse", "user")),
            ..GenerateContentConfig::default()
        }),
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    assert_eq!(msgs.len(), 2, "messages = {}, want 2", msgs.len());
    assert!(
        field(&msgs[0], "role") == json!("system")
            && field(&msgs[0], "content") == json!("be terse"),
        "first message = {:?}, want the system instruction",
        msgs[0]
    );
    assert_eq!(
        field(&msgs[1], "role"),
        json!("user"),
        "second message role"
    );
    assert!(
        !wire.contains_key("instructions"),
        "instructions field sent; Chat Completions has no such field"
    );
}

#[test]
fn test_build_params_multi_turn_history() {
    let wire = request_wire(&LlmRequest {
        contents: vec![
            content("user", vec![text("my locker is 8123")]),
            content("model", vec![text("noted")]),
            content("user", vec![text("which locker?")]),
        ],
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    assert_eq!(msgs.len(), 3, "messages = {}, want 3", msgs.len());
    for (i, want) in ["user", "assistant", "user"].into_iter().enumerate() {
        assert_eq!(field(&msgs[i], "role"), json!(want), "message {i} role");
    }
    // The assistant turn goes back as plain content. Nothing here resembles the Responses
    // API's output_text wrapper.
    assert_eq!(
        field(&msgs[1], "content"),
        json!("noted"),
        "assistant content"
    );
}

#[test]
fn test_build_params_text_parts_join() {
    let wire = request_wire(&user_req(vec![text("one"), text("  "), text("two")]));
    let msgs = wire_messages(&wire);
    assert_eq!(
        field(&msgs[0], "content"),
        json!("one\ntwo"),
        "whitespace-only parts are dropped"
    );
}

#[test]
fn test_build_params_tool_call_and_result_pairing() {
    let wire = request_wire(&LlmRequest {
        contents: vec![
            content("user", vec![text("weather?")]),
            content(
                "model",
                vec![call_part(
                    "call_1",
                    "get_weather",
                    json!({"city": "Lisbon"}),
                )],
            ),
            content(
                "user",
                vec![response_part("call_1", "get_weather", json!({"c": -7}))],
            ),
        ],
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    assert_eq!(msgs.len(), 3, "messages = {}, want 3", msgs.len());

    let Value::Array(calls) = field(&msgs[1], "tool_calls") else {
        panic!(
            "assistant tool_calls = {:?}, want one",
            msgs[1].get("tool_calls")
        );
    };
    assert_eq!(calls.len(), 1, "assistant tool_calls = {calls:?}, want one");
    let call = &calls[0];
    assert!(
        call["id"] == json!("call_1") && call["type"] == json!("function"),
        "tool call = {call:?}, want id call_1 and type function"
    );
    let function = &call["function"];
    assert_eq!(function["name"], json!("get_weather"), "function name");
    assert_eq!(
        function["arguments"],
        json!(r#"{"city":"Lisbon"}"#),
        "arguments, want the JSON-encoded args"
    );

    // The result is its own tool-role message keyed to the call, not a part of the user turn
    // that carried it.
    assert_eq!(field(&msgs[2], "role"), json!("tool"), "result role");
    assert_eq!(
        field(&msgs[2], "tool_call_id"),
        json!("call_1"),
        "tool_call_id"
    );
}

#[test]
fn test_build_params_tool_result_without_call_id() {
    let wire = request_wire(&LlmRequest {
        contents: vec![
            content("model", vec![call_part("", "f", Value::Null)]),
            content("user", vec![response_part("", "f", Value::Null)]),
        ],
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    let minted = field(&msgs[0], "tool_calls")[0]["id"].clone();
    assert!(
        minted.as_str().is_some_and(|id| !id.is_empty()),
        "no call id minted"
    );
    // A response with no ID pairs with the oldest outstanding call, so the two still line up
    // on the wire.
    assert_eq!(
        field(&msgs[1], "tool_call_id"),
        minted,
        "tool_call_id, want the minted id"
    );
}

#[test]
fn test_build_params_unknown_tool_result_id_rejected() {
    let err = build_err(&LlmRequest {
        contents: vec![content(
            "user",
            vec![response_part("nope", "f", Value::Null)],
        )],
        ..LlmRequest::default()
    });
    assert!(
        err.to_string().contains("nope"),
        "err = {err}, want one naming the unknown call id"
    );
}

/// Prior-turn reasoning is dropped rather than sent back as assistant text, as on the
/// Responses path.
#[test]
fn test_build_params_thoughts_not_replayed() {
    let wire = request_wire(&LlmRequest {
        contents: vec![content(
            "model",
            vec![
                Part {
                    text: "the user wants a joke".to_string(),
                    thought: true,
                    ..Part::default()
                },
                text("here it is"),
            ],
        )],
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    assert_eq!(msgs.len(), 1, "messages = {}, want 1", msgs.len());
    assert_eq!(
        field(&msgs[0], "content"),
        json!("here it is"),
        "want only the answer"
    );
}

#[test]
fn test_build_params_thought_only_turn_produces_no_message() {
    let err = build_err(&LlmRequest {
        contents: vec![content(
            "model",
            vec![Part {
                text: "thinking".to_string(),
                thought: true,
                ..Part::default()
            }],
        )],
        ..LlmRequest::default()
    });
    assert!(
        err.is(ErrorKind::NoContents),
        "err = {err}, want {}",
        ErrorKind::NoContents
    );
}

#[test]
fn test_build_params_no_contents() {
    let err = build_err(&LlmRequest::default());
    assert!(
        err.is(ErrorKind::NoContents),
        "err = {err}, want {}",
        ErrorKind::NoContents
    );
}

// parity: skipped TestBuildParams_NilRequest: an `LlmRequest` is passed by reference and is never nil, so `ErrRequestNil` cannot arise (contract compile fix 7)

/// A payload this endpoint cannot send is rejected by name, including when text on the same
/// part would otherwise have carried the request out without it.
#[test]
fn test_build_params_unsupported_part() {
    let tests: Vec<(&str, Part, &str)> = vec![
        (
            "media resolution beside inline data",
            Part {
                media_resolution: Some(PartMediaResolution {
                    level: Some(PartMediaResolutionLevel::MediaResolutionHigh),
                    ..PartMediaResolution::default()
                }),
                ..inline_part("image/png", &[1])
            },
            "unsupported content part: MediaResolution",
        ),
        (
            "video metadata beside file data",
            Part {
                file_data: Some(file_data("video/mp4", "https://example.com/v.mp4")),
                video_metadata: Some(VideoMetadata::default()),
                ..Part::default()
            },
            "unsupported content part: VideoMetadata",
        ),
        (
            "empty part",
            Part::default(),
            "unsupported content part: carries nothing to send",
        ),
    ];
    for (name, part, want_err) in tests {
        let err = build_err(&user_req(vec![part]));
        assert!(
            err.to_string().contains(want_err),
            "{name}: err = {err}, want one containing {want_err:?}"
        );
    }
}

/// A call on a part also carrying text or the thought marker is still sent, rather than lost
/// to whichever field matched first.
#[test]
fn test_build_params_part_fields_read_independently() {
    let call = function_call("call_1", "get_weather", json!({"city": "Lisbon"}));
    let result = content(
        "user",
        vec![response_part("call_1", "get_weather", json!({"c": -7}))],
    );
    let tests: Vec<(&str, Part, Value)> = vec![
        (
            "text and call",
            Part {
                text: "let me check".to_string(),
                function_call: Some(call.clone()),
                ..Part::default()
            },
            json!("let me check"),
        ),
        (
            "thought text and call",
            Part {
                text: "scratch".to_string(),
                thought: true,
                function_call: Some(call.clone()),
                ..Part::default()
            },
            Value::Null,
        ),
        (
            "signed call",
            Part {
                function_call: Some(call.clone()),
                thought_signature: Bytes::from_static(b"sig"),
                ..Part::default()
            },
            Value::Null,
        ),
    ];
    for (name, part, want_content) in tests {
        let msgs = wire_messages(&request_wire(&LlmRequest {
            contents: vec![content("model", vec![part]), result.clone()],
            ..LlmRequest::default()
        }));
        assert_eq!(
            msgs.len(),
            2,
            "{name}: messages = {}, want the assistant turn and its tool result",
            msgs.len()
        );
        assert_eq!(field(&msgs[0], "content"), want_content, "{name}: content");
        let calls = match field(&msgs[0], "tool_calls") {
            Value::Array(calls) => calls,
            _ => Vec::new(),
        };
        assert_eq!(
            calls.len(),
            1,
            "{name}: tool_calls = {calls:?}, want the call"
        );
        assert_eq!(calls[0]["id"], json!("call_1"), "{name}: tool call id");
        assert_eq!(
            field(&msgs[1], "tool_call_id"),
            json!("call_1"),
            "{name}: tool result = {:?}, want it paired with call_1",
            msgs[1]
        );
    }
}

/// The part a Gemini thinking model leaves in shared history holding nothing but its
/// signature, which Chat Completions has no field for.
#[test]
fn test_build_params_signature_only_part_dropped() {
    let signature = || Bytes::from_static(b"sig");
    let tests: Vec<(&str, Vec<Part>, Vec<&str>)> = vec![
        (
            "after text",
            vec![
                text("sunny"),
                Part {
                    thought_signature: signature(),
                    ..Part::default()
                },
            ],
            vec!["user", "assistant"],
        ),
        (
            "marked as a thought",
            vec![Part {
                thought: true,
                thought_signature: signature(),
                ..Part::default()
            }],
            vec!["user"],
        ),
        (
            "alone",
            vec![Part {
                thought_signature: signature(),
                ..Part::default()
            }],
            vec!["user"],
        ),
    ];
    for (name, parts, want) in tests {
        let msgs = wire_messages(&request_wire(&LlmRequest {
            contents: vec![
                content("user", vec![text("weather?")]),
                content("model", parts),
            ],
            ..LlmRequest::default()
        }));
        let roles: Vec<Value> = msgs.iter().map(|msg| field(msg, "role")).collect();
        let want: Vec<Value> = want.into_iter().map(Value::from).collect();
        assert_eq!(roles, want, "{name}: roles");
    }
}

/// A call only an assistant message could carry fails by name, rather than going missing and
/// leaving its result to point at a call the request never declared.
#[test]
fn test_build_params_call_outside_model_turn_rejected() {
    for role in ["user", "system", "developer"] {
        let err = build_err(&LlmRequest {
            contents: vec![content(
                role,
                vec![call_part("call_1", "get_weather", Value::Null)],
            )],
            ..LlmRequest::default()
        });
        let want = format!("function call in a {role} turn");
        assert!(
            err.to_string().contains(&want),
            "{role}: err = {err}, want one naming the misplaced call"
        );
    }
}

/// The three settings Chat Completions honors that the Responses path rejects outright.
#[test]
fn test_apply_generation_config_endpoint_only_fields() {
    let wire = request_wire(&hi_req(GenerateContentConfig {
        stop_sequences: vec!["STOP".to_string(), "END".to_string()],
        seed: Some(42),
        frequency_penalty: Some(0.5),
        presence_penalty: Some(-0.25),
        ..GenerateContentConfig::default()
    }));
    let stop = field(&wire, "stop");
    assert!(
        stop.as_array()
            .is_some_and(|stop| stop.len() == 2 && stop[0] == json!("STOP")),
        "stop = {stop:?}, want the two sequences"
    );
    assert_eq!(field(&wire, "seed"), json!(42), "seed");
    assert!(
        near(&field(&wire, "frequency_penalty"), 0.5),
        "frequency_penalty = {:?}, want 0.5",
        wire.get("frequency_penalty")
    );
    assert!(
        near(&field(&wire, "presence_penalty"), -0.25),
        "presence_penalty = {:?}, want -0.25",
        wire.get("presence_penalty")
    );
}

#[test]
fn test_apply_generation_config_translated_fields() {
    let wire = request_wire(&hi_req(GenerateContentConfig {
        temperature: Some(0.3),
        top_p: Some(0.9),
        max_output_tokens: 256,
        response_logprobs: true,
        logprobs: Some(3),
        service_tier: Some(ServiceTier::Flex),
        ..GenerateContentConfig::default()
    }));
    // float32 config widened to float64 on the wire, so compare with tolerance.
    assert!(
        near(&field(&wire, "temperature"), 0.3),
        "temperature = {:?}, want 0.3",
        wire.get("temperature")
    );
    assert!(
        near(&field(&wire, "top_p"), 0.9),
        "top_p = {:?}, want 0.9",
        wire.get("top_p")
    );
    // The cap is max_completion_tokens here, not the Responses name.
    assert_eq!(
        field(&wire, "max_completion_tokens"),
        json!(256),
        "max_completion_tokens"
    );
    assert!(
        !wire.contains_key("max_output_tokens"),
        "max_output_tokens sent; that is the Responses field"
    );
    assert!(
        field(&wire, "logprobs") == json!(true) && field(&wire, "top_logprobs") == json!(3),
        "logprobs = {:?}, top_logprobs = {:?}",
        wire.get("logprobs"),
        wire.get("top_logprobs")
    );
    assert_eq!(field(&wire, "service_tier"), json!("flex"), "service_tier");
}

/// AutomaticContinuation has no effect on Chat Completions, which never pauses a generation
/// with CONTINUATION, so a config shared with a Gemini model that sets it must be accepted.
#[test]
fn test_apply_generation_config_accepts_automatic_continuation() {
    for on in [false, true] {
        let result = build_params(
            "m",
            &hi_req(GenerateContentConfig {
                automatic_continuation: Some(on),
                ..GenerateContentConfig::default()
            }),
        );
        assert!(
            result.is_ok(),
            "{on}: buildParams() error = {:?}, want nil",
            result.err()
        );
    }
}

#[test]
fn test_apply_generation_config_rejected_fields() {
    let tests: Vec<(&str, GenerateContentConfig, ErrorKind)> = vec![
        (
            "topK",
            GenerateContentConfig {
                top_k: Some(5.0),
                ..GenerateContentConfig::default()
            },
            ErrorKind::TopKNotSupported,
        ),
        (
            "candidate count",
            GenerateContentConfig {
                candidate_count: 2,
                ..GenerateContentConfig::default()
            },
            ErrorKind::MultipleCandidatesNotSupported,
        ),
        (
            "labels",
            GenerateContentConfig {
                labels: [("a".to_string(), "b".to_string())].into(),
                ..GenerateContentConfig::default()
            },
            ErrorKind::LabelsNotSupported,
        ),
        (
            "safety settings",
            GenerateContentConfig {
                safety_settings: vec![SafetySetting::default()],
                ..GenerateContentConfig::default()
            },
            ErrorKind::SafetySettingsNotSupported,
        ),
        (
            "mime type",
            GenerateContentConfig {
                response_mime_type: "text/csv".to_string(),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedMimeType,
        ),
        (
            "routing config",
            GenerateContentConfig {
                routing_config: Some(GenerationConfigRoutingConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "model selection config",
            GenerateContentConfig {
                model_selection_config: Some(ModelSelectionConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "cached content",
            GenerateContentConfig {
                cached_content: "c".to_string(),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "response modalities",
            GenerateContentConfig {
                response_modalities: vec!["AUDIO".to_string()],
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "media resolution",
            GenerateContentConfig {
                media_resolution: Some(MediaResolution::Low),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "speech config",
            GenerateContentConfig {
                speech_config: Some(SpeechConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "audio timestamp",
            GenerateContentConfig {
                audio_timestamp: true,
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "image config",
            GenerateContentConfig {
                image_config: Some(ImageConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "enhanced civic answers",
            GenerateContentConfig {
                enable_enhanced_civic_answers: Some(true),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "model armor config",
            GenerateContentConfig {
                model_armor_config: Some(ModelArmorConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "audio transcription config",
            GenerateContentConfig {
                audio_transcription_config: Some(AudioTranscriptionConfig::default()),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
        (
            "continuation token",
            GenerateContentConfig {
                continuation_token: Some(Bytes::from_static(b"token")),
                ..GenerateContentConfig::default()
            },
            ErrorKind::UnsupportedConfigField,
        ),
    ];
    for (name, cfg, want) in tests {
        let err = build_err(&hi_req(cfg));
        assert!(err.is(want), "{name}: err = {err}, want {want}");
    }
}

// parity: adapted: Go swaps three package variables; gadk passes the top-level list to build_params_with (Temperature runs through build_params), while the HTTP and thinking lists are `shared::config` consts, so those two cases run the narrowed walk through reject_unsupported_fields over the field lists build_params checks
#[test]
fn test_apply_generation_config_refuses_fields_outside_the_supported_lists() {
    let want = |field: &str| format!("{}: {field}", ErrorKind::UnsupportedConfigField);

    // "Temperature": the top-level list without it.
    let supported: Vec<&str> = supported_config_fields()
        .into_iter()
        .filter(|name| *name != "Temperature")
        .collect();
    let err = match build_params_with(
        "m",
        &hi_req(GenerateContentConfig {
            temperature: Some(0.5),
            ..GenerateContentConfig::default()
        }),
        &supported,
    ) {
        Ok(_) => panic!("Temperature: err = nil, want {:?}", want("Temperature")),
        Err(err) => err,
    };
    assert!(
        err.is(ErrorKind::UnsupportedConfigField),
        "Temperature: err = {err}"
    );
    assert_eq!(err.to_string(), want("Temperature"), "Temperature");

    // "HTTPOptions.Timeout": SupportedHTTPOptionFields = nil leaves only the ignored ones.
    let options = HttpOptions {
        timeout: Some(Duration::from_secs(1)),
        ..HttpOptions::default()
    };
    let err = match reject_unsupported_fields(
        "HTTPOptions.",
        &http_option_fields(&options),
        IGNORED_HTTP_OPTION_FIELDS,
    ) {
        Ok(()) => panic!("HTTPOptions.Timeout: err = nil"),
        Err(err) => err,
    };
    assert!(
        err.is(ErrorKind::UnsupportedConfigField),
        "HTTPOptions.Timeout: err = {err}"
    );
    assert_eq!(
        err.to_string(),
        want("HTTPOptions.Timeout"),
        "HTTPOptions.Timeout"
    );

    // "ThinkingConfig.ThinkingLevel": SupportedThinkingConfigFields without ThinkingLevel.
    let thinking = ThinkingConfig {
        thinking_level: Some(ThinkingLevel::Low),
        ..ThinkingConfig::default()
    };
    let err = match reject_unsupported_fields(
        "ThinkingConfig.",
        &thinking_config_fields(&thinking),
        &["IncludeThoughts", "ThinkingBudget"],
    ) {
        Ok(()) => panic!("ThinkingConfig.ThinkingLevel: err = nil"),
        Err(err) => err,
    };
    assert!(
        err.is(ErrorKind::UnsupportedConfigField),
        "ThinkingConfig.ThinkingLevel: err = {err}"
    );
    assert_eq!(
        err.to_string(),
        want("ThinkingConfig.ThinkingLevel"),
        "ThinkingConfig.ThinkingLevel"
    );
}

/// The counterpart of the rejected list: Seed is refused on Responses and must not be refused
/// here.
#[test]
fn test_apply_generation_config_seed_accepted() {
    let result = build_params(
        "m",
        &hi_req(GenerateContentConfig {
            seed: Some(7),
            ..GenerateContentConfig::default()
        }),
    );
    assert!(
        result.is_ok(),
        "buildParams() err = {:?}, want nil",
        result.err()
    );
}

/// An object schema titled `title` with one string property `city`.
fn city_schema(title: &str) -> Schema {
    Schema {
        title: title.to_string(),
        r#type: Some(Type::Object),
        properties: [(
            "city".to_string(),
            Schema {
                r#type: Some(Type::String),
                ..Schema::default()
            },
        )]
        .into(),
        ..Schema::default()
    }
}

#[test]
fn test_apply_generation_config_response_format() {
    // "json object without a schema"
    let wire = request_wire(&hi_req(GenerateContentConfig {
        response_mime_type: "application/json".to_string(),
        ..GenerateContentConfig::default()
    }));
    let format = field(&wire, "response_format");
    assert_eq!(
        format["type"],
        json!("json_object"),
        "json object without a schema: response_format = {format:?}"
    );
    assert!(
        !wire.contains_key("text"),
        "json object without a schema: text field sent; that is the Responses slot"
    );

    // "json schema"
    let wire = request_wire(&hi_req(GenerateContentConfig {
        response_schema: Some(city_schema("answer")),
        ..GenerateContentConfig::default()
    }));
    let format = field(&wire, "response_format");
    assert_eq!(format["type"], json!("json_schema"), "json schema: type");
    let schema = &format["json_schema"];
    assert!(
        schema["name"] == json!("answer") && schema["strict"] == json!(true),
        "json schema: json_schema = {schema:?}, want name answer and strict true"
    );
    assert_eq!(
        schema["schema"]["additionalProperties"],
        json!(false),
        "json schema: strict rewriting did not run on the schema"
    );

    // "json schema title that is not a legal format name": OpenAI accepts only
    // ^[a-zA-Z0-9_-]{1,64}$ in the name while genai puts no constraint on Title, so an
    // illegal title falls back to the default name and the body keeps the caller's title.
    let wire = request_wire(&hi_req(GenerateContentConfig {
        response_schema: Some(city_schema("Weather Report")),
        ..GenerateContentConfig::default()
    }));
    let schema = &field(&wire, "response_format")["json_schema"];
    assert_eq!(
        schema["name"],
        json!(DEFAULT_RESPONSE_FORMAT_NAME),
        "illegal title: name"
    );
    assert_eq!(
        schema["schema"]["title"],
        json!("Weather Report"),
        "illegal title: schema title, want the caller's title unchanged"
    );

    // "response json schema"
    let wire = request_wire(&hi_req(GenerateContentConfig {
        response_json_schema: Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
        })),
        ..GenerateContentConfig::default()
    }));
    let schema = &field(&wire, "response_format")["json_schema"];
    assert!(
        schema["name"] == json!("adk_response") && schema["strict"] == json!(true),
        "response json schema: json_schema = {schema:?}, want the default name and strict true"
    );
    let body = &schema["schema"];
    assert!(
        body["properties"].get("city").is_some(),
        "response json schema: schema = {body:?}, want the caller's city property"
    );
    assert_eq!(
        body["additionalProperties"],
        json!(false),
        "response json schema: strict rewriting did not run on the schema"
    );
}

#[test]
fn test_apply_thinking_config() {
    let budget = |n: i32| ThinkingConfig {
        thinking_budget: Some(n),
        ..ThinkingConfig::default()
    };
    // (name, thinking config, want; None = absent)
    let tests: Vec<(&str, Option<ThinkingConfig>, Option<&str>)> = vec![
        (
            "level low",
            Some(ThinkingConfig {
                thinking_level: Some(ThinkingLevel::Low),
                ..ThinkingConfig::default()
            }),
            Some("low"),
        ),
        ("budget zero means none", Some(budget(0)), Some("none")),
        (
            "positive budget means medium",
            Some(budget(1024)),
            Some("medium"),
        ),
        ("dynamic budget defers to the model", Some(budget(-1)), None),
        // No reasoning summary exists on this endpoint, so asking for thoughts is accepted
        // and changes nothing rather than failing the call.
        (
            "include thoughts alone is ignored",
            Some(ThinkingConfig {
                include_thoughts: true,
                ..ThinkingConfig::default()
            }),
            None,
        ),
        // Non-reasoning models reject reasoning_effort, so nothing asked for must mean
        // nothing sent.
        ("no thinking config sends nothing", None, None),
    ];
    for (name, cfg, want) in tests {
        let wire = request_wire(&hi_req(GenerateContentConfig {
            thinking_config: cfg,
            ..GenerateContentConfig::default()
        }));
        let got = wire.get("reasoning_effort");
        let Some(want) = want else {
            assert!(
                got.is_none(),
                "{name}: reasoning_effort = {got:?}, want it absent"
            );
            continue;
        };
        assert_eq!(got, Some(&json!(want)), "{name}: reasoning_effort");
        assert!(
            !wire.contains_key("reasoning"),
            "{name}: reasoning object sent; this endpoint takes a scalar"
        );
    }
}

#[test]
fn test_apply_thinking_config_rejects_nonsense_budget() {
    let err = build_err(&hi_req(GenerateContentConfig {
        thinking_config: Some(ThinkingConfig {
            thinking_budget: Some(-5),
            ..ThinkingConfig::default()
        }),
        ..GenerateContentConfig::default()
    }));
    assert!(
        err.is(ErrorKind::UnsupportedConfigField),
        "err = {err}, want {}",
        ErrorKind::UnsupportedConfigField
    );
}

/// The shape a streamed turn leaves in the session: speech either side of a tool call arrives
/// as two parts in one content. A Chat Completions assistant message has one content string
/// and one tool_calls array, so the interleaving flattens; neither half of the speech may go
/// missing.
#[test]
fn test_build_params_interleaved_text_and_call() {
    let wire = request_wire(&LlmRequest {
        contents: vec![
            content(
                "model",
                vec![
                    Part {
                        text: "thinking about it".to_string(),
                        thought: true,
                        ..Part::default()
                    },
                    text("let me check"),
                    call_part("call_1", "get_weather", Value::Null),
                    text("it is hailing at -7"),
                ],
            ),
            content(
                "user",
                vec![response_part("call_1", "get_weather", json!({"c": -7}))],
            ),
        ],
        ..LlmRequest::default()
    });
    let msgs = wire_messages(&wire);
    assert_eq!(
        msgs.len(),
        2,
        "messages = {}, want the assistant turn and its tool result",
        msgs.len()
    );

    // Both text parts survive, joined, with the thought left out.
    assert_eq!(
        field(&msgs[0], "content"),
        json!("let me check\nit is hailing at -7"),
        "want both text parts joined and the thought dropped"
    );
    let calls = field(&msgs[0], "tool_calls");
    assert!(
        calls.as_array().is_some_and(|calls| calls.len() == 1),
        "tool_calls = {calls:?}, want the one call alongside the text"
    );
    assert!(
        field(&msgs[1], "role") == json!("tool")
            && field(&msgs[1], "tool_call_id") == json!("call_1"),
        "second message = {:?}, want the paired tool result",
        msgs[1]
    );
}
