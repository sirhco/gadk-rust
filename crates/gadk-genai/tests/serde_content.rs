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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_genai::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn serde_value_zero_omitted() {
    // Go `Text string json:"text,omitempty"`: "" is omitted on encode.
    let p: Part = serde_json::from_value(json!({"text": "", "thought": false})).unwrap();
    assert_eq!(serde_json::to_value(&p).unwrap(), json!({}));
}

#[test]
fn serde_bytes_base64_roundtrip() {
    let p = Part {
        thought_signature: bytes::Bytes::from_static(b"sig"),
        ..Default::default()
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v, json!({"thoughtSignature": "c2ln"}));
    let back: Part = serde_json::from_value(v).unwrap();
    assert_eq!(back, p);
}

#[test]
fn serde_function_call_camel_case() {
    let c = Content::from_parts(
        vec![Part::from_function_call(
            "get_weather",
            serde_json::Map::from_iter([("city".into(), json!("Lisbon"))]),
        )],
        ROLE_MODEL,
    );
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"parts":[{"functionCall":{"args":{"city":"Lisbon"},"name":"get_weather"}}],"role":"model"})
    );
}

#[test]
fn serde_unknown_type_roundtrip() {
    let t: Type = serde_json::from_value(json!("FUTURE_TYPE")).unwrap();
    assert_eq!(t, Type::Unknown("FUTURE_TYPE".into()));
    assert_eq!(serde_json::to_value(&t).unwrap(), json!("FUTURE_TYPE"));
    assert_eq!(serde_json::to_value(Type::String).unwrap(), json!("STRING"));
}

#[test]
fn serde_pointer_field_keeps_zero() {
    // Go `WillContinue *bool`: Some(false) is serialized.
    let fc = FunctionCall {
        will_continue: Some(false),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&fc).unwrap(),
        json!({"willContinue": false})
    );
}

#[test]
fn text_helper_is_one_user_content() {
    assert_eq!(
        text("ping"),
        vec![Content {
            parts: vec![Part::from_text("ping")],
            role: ROLE_USER.into()
        }]
    );
}

// ---- additional coverage beyond the brief's six tests ----

#[test]
fn serde_bytes_null_and_inline_data() {
    // Go `[]byte` decodes JSON null to nil.
    let b: Blob = serde_json::from_value(json!({"data": null, "mimeType": "image/png"})).unwrap();
    assert!(b.data.is_empty());
    let p = Part::from_bytes(bytes::Bytes::from_static(b"\x00\xff"), "image/png");
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({"inlineData": {"data": "AP8=", "mimeType": "image/png"}})
    );
    assert!(serde_json::from_value::<Blob>(json!({"data": "!!"})).is_err());
}

#[test]
fn serde_video_metadata_durations() {
    // genai VideoMetadata.MarshalJSON: "%.0fs"; a set end offset forces startOffset "0s".
    let vm = VideoMetadata {
        end_offset: chrono::TimeDelta::milliseconds(10_400),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&vm).unwrap(),
        json!({"endOffset": "10s", "startOffset": "0s"})
    );
    assert_eq!(
        serde_json::to_value(VideoMetadata::default()).unwrap(),
        json!({})
    );
    let vm: VideoMetadata =
        serde_json::from_value(json!({"startOffset": "1m30.5s", "endOffset": "", "fps": 2.0}))
            .unwrap();
    assert_eq!(vm.start_offset, chrono::TimeDelta::milliseconds(90_500));
    assert_eq!(vm.end_offset, chrono::TimeDelta::zero());
    assert_eq!(vm.fps, Some(2.0));
    assert!(serde_json::from_value::<VideoMetadata>(json!({"startOffset": "5"})).is_err());
    assert!(serde_json::from_value::<VideoMetadata>(json!({"startOffset": 5})).is_err());
}

#[test]
fn serde_transcription_durations() {
    // genai Transcription.MarshalJSON: no "0s" fill-in, zero offsets omitted.
    let t = Transcription {
        text: "hi".into(),
        end_offset: chrono::TimeDelta::seconds(3),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        json!({"text": "hi", "endOffset": "3s"})
    );
    let t: Transcription = serde_json::from_value(json!({
        "startOffset": "250ms", "finished": true,
        "words": [{"word": "hi", "startOffset": "0.1s"}]
    }))
    .unwrap();
    assert_eq!(t.start_offset, chrono::TimeDelta::milliseconds(250));
    assert!(t.finished);
    assert_eq!(t.words[0].start_offset, "0.1s");
}

#[test]
fn go_duration_parse_and_format() {
    use chrono::TimeDelta;
    use gadk_genai::serde_util::{format_go_seconds, parse_go_duration};
    assert_eq!(parse_go_duration("0").unwrap(), TimeDelta::zero());
    assert_eq!(parse_go_duration("-1.5h").unwrap(), TimeDelta::minutes(-90));
    assert_eq!(
        parse_go_duration("1h2m3s4ms5us6ns").unwrap(),
        TimeDelta::nanoseconds(3_723_004_005_006)
    );
    assert_eq!(
        parse_go_duration("2µs").unwrap(),
        TimeDelta::microseconds(2)
    );
    assert_eq!(
        parse_go_duration(".5s").unwrap(),
        TimeDelta::milliseconds(500)
    );
    for bad in [
        "",
        "-",
        "s",
        "1",
        "1x",
        ".s",
        "9999999999999999999s",
        "3000000h",
    ] {
        assert!(parse_go_duration(bad).is_err(), "{bad}");
    }
    // Go %.0f rounds half to even.
    assert_eq!(format_go_seconds(TimeDelta::milliseconds(2500)), "2s");
    assert_eq!(format_go_seconds(TimeDelta::milliseconds(3500)), "4s");
    assert_eq!(format_go_seconds(TimeDelta::seconds(-7)), "-7s");
}

#[test]
fn serde_schema_recursive() {
    let s: Schema = serde_json::from_value(json!({
        "type": "OBJECT",
        "properties": {"b": {"type": "STRING"}, "a": {"type": "ARRAY", "items": {"type": "INTEGER"}}},
        "required": ["a"],
        "anyOf": [{"type": "NULL"}],
        "nullable": false,
        "minimum": 0.0,
        "default": null
    }))
    .unwrap();
    assert_eq!(s.r#type, Some(Type::Object));
    assert_eq!(
        s.properties["a"].items.as_deref().unwrap().r#type,
        Some(Type::Integer)
    );
    assert_eq!(s.default, None);
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        json!({
            "type": "OBJECT",
            "properties": {"a": {"type": "ARRAY", "items": {"type": "INTEGER"}}, "b": {"type": "STRING"}},
            "required": ["a"],
            "anyOf": [{"type": "NULL"}],
            "nullable": false,
            "minimum": 0.0
        })
    );
}

#[test]
fn content_constructors_match_genai() {
    // genai roleString: empty role defaults to "user".
    assert_eq!(Content::from_text("hi", "").role, ROLE_USER);
    assert_eq!(
        serde_json::to_value(Content::from_uri("gs://b/o", "video/mp4", ROLE_USER)).unwrap(),
        json!({"parts":[{"fileData":{"fileUri":"gs://b/o","mimeType":"video/mp4"}}],"role":"user"})
    );
    assert_eq!(
        serde_json::to_value(Content::from_executable_code(
            "print(1)",
            Language::Python,
            ROLE_MODEL
        ))
        .unwrap(),
        json!({"parts":[{"executableCode":{"code":"print(1)","language":"PYTHON"}}],"role":"model"})
    );
    assert_eq!(
        serde_json::to_value(Content::from_code_execution_result(
            Outcome::Ok,
            "1\n",
            ROLE_USER
        ))
        .unwrap(),
        json!({"parts":[{"codeExecutionResult":{"outcome":"OUTCOME_OK","output":"1\n"}}],"role":"user"})
    );
    let fr = FunctionResponse {
        name: "f".into(),
        response: serde_json::Map::from_iter([("output".into(), json!(1))]),
        ..Default::default()
    };
    assert_eq!(
        Content::from_function_response(fr.clone(), ROLE_USER).parts,
        vec![Part {
            function_response: Some(fr),
            ..Default::default()
        }]
    );
    assert_eq!(
        Part::from_function_response("f", serde_json::Map::new())
            .function_response
            .unwrap()
            .name,
        "f"
    );
    let fc = FunctionCall {
        name: "g".into(),
        ..Default::default()
    };
    assert_eq!(
        Content::from_function_call(fc.clone(), ROLE_MODEL).parts[0].function_call,
        Some(fc)
    );
    assert_eq!(
        Content::from_bytes(bytes::Bytes::from_static(b"x"), "text/plain", ROLE_USER).parts[0],
        Part::from_bytes(bytes::Bytes::from_static(b"x"), "text/plain")
    );
}

#[test]
fn enum_display_and_from_str() {
    assert_eq!(
        Outcome::DeadlineExceeded.to_string(),
        "OUTCOME_DEADLINE_EXCEEDED"
    );
    assert_eq!(
        "WHEN_IDLE".parse::<FunctionResponseScheduling>().unwrap(),
        FunctionResponseScheduling::WhenIdle
    );
    assert_eq!(
        serde_json::to_value(PartMediaResolutionLevel::MediaResolutionHigh).unwrap(),
        json!("MEDIA_RESOLUTION_HIGH")
    );
    assert_eq!(ToolType::UrlContext.as_str(), "URL_CONTEXT");
    assert_eq!(MediaProcessing::Agentic.as_str(), "AGENTIC");
}
