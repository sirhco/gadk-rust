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
use serde_json::{Value, json};

/// Fields the live API sends that genai v1.77.0 does not model.
const UNMODELED: &[&str] = &[
    "turnToken",
    // generate_content_response_000.json: `usageMetadata.rawPromptTokenCount` and
    // `usageMetadata.serviceTier` are not fields of genai v1.77.0
    // `GenerateContentResponseUsageMetadata` (only Live `UsageMetadata` has `serviceTier`).
    "rawPromptTokenCount",
    "serviceTier",
];

/// Removes Go zero values (null, 0, "", false, [], {}) and unmodeled keys, recursively,
/// so value-type omitempty fields that the API sent as zero compare equal to their omission.
fn strip_zero(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|k, _| !UNMODELED.contains(&k.as_str()));
            m.values_mut().for_each(strip_zero);
            m.retain(|_, x| !is_go_zero(x));
        }
        Value::Array(a) => a.iter_mut().for_each(strip_zero),
        _ => {}
    }
}
fn is_go_zero(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}
/// Numbers compared as f64 (serde writes 0.5f32 as 0.5, Go fixtures may differ in formatting).
fn as_f64_numbers(v: &mut Value) {
    match v {
        Value::Object(m) => m.values_mut().for_each(as_f64_numbers),
        Value::Array(a) => a.iter_mut().for_each(as_f64_numbers),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                *v = serde_json::json!(f);
            }
        }
        _ => {}
    }
}

#[test]
fn serde_captured_responses_roundtrip() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let mut original: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let typed: GenerateContentResponse = serde_json::from_value(original.clone()).unwrap();
        let mut back = serde_json::to_value(&typed).unwrap();
        // decode → encode → decode is stable
        let again: GenerateContentResponse = serde_json::from_value(back.clone()).unwrap();
        assert_eq!(again, typed, "{}", path.display());
        // and nothing the API sent (apart from Go zero values / UNMODELED keys) is lost
        for v in [&mut original, &mut back] {
            strip_zero(v);
            as_f64_numbers(v);
        }
        assert_eq!(back, original, "{}", path.display());
        n += 1;
    }
    assert!(n >= 3);
}

#[test]
fn serde_temperature_zero_kept() {
    let cfg = GenerateContentConfig {
        temperature: Some(0.0),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&cfg).unwrap(),
        json!({"temperature": 0.0})
    );
}

#[test]
fn serde_unknown_finish_reason_roundtrip() {
    let c: Candidate = serde_json::from_value(json!({"finishReason": "FUTURE_VALUE"})).unwrap();
    assert_eq!(
        c.finish_reason,
        Some(FinishReason::Unknown("FUTURE_VALUE".into()))
    );
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"finishReason": "FUTURE_VALUE"})
    );
}

#[test]
fn serde_continuation_token_on_candidate() {
    let c: Candidate = serde_json::from_value(
        json!({"finishReason": "CONTINUATION", "continuationToken": "dG9r"}),
    )
    .unwrap();
    assert_eq!(c.continuation_token.as_deref(), Some(&b"tok"[..]));
    assert_eq!(c.finish_reason, Some(FinishReason::Continuation));
}

#[test]
fn serde_config_continuation_token_and_automatic_continuation() {
    let cfg = GenerateContentConfig {
        continuation_token: Some(bytes::Bytes::from_static(b"tok")),
        automatic_continuation: Some(false),
        ..Default::default()
    };
    let v = serde_json::to_value(&cfg).unwrap();
    assert_eq!(
        v,
        json!({"continuationToken": "dG9r", "automaticContinuation": false})
    );
    assert_eq!(
        serde_json::from_value::<GenerateContentConfig>(v).unwrap(),
        cfg
    );
}

/// genai v1.77.0 `GenerateContentConfig.{GenerationOutputConfig, Store}`: Go JSON names,
/// omitted when unset, `store: false` and an empty `generationOutputConfig` kept.
#[test]
fn serde_config_generation_output_config_and_store() {
    assert_eq!(
        serde_json::to_value(GenerateContentConfig::default()).unwrap(),
        json!({})
    );
    for (cfg, want) in [
        (
            GenerateContentConfig {
                generation_output_config: Some(GenerationOutputConfig {
                    output_uri_prefix: "gs://b/p/".into(),
                }),
                store: Some(true),
                ..Default::default()
            },
            json!({"generationOutputConfig": {"outputUriPrefix": "gs://b/p/"}, "store": true}),
        ),
        (
            GenerateContentConfig {
                generation_output_config: Some(GenerationOutputConfig::default()),
                store: Some(false),
                ..Default::default()
            },
            json!({"generationOutputConfig": {}, "store": false}),
        ),
    ] {
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(v, want);
        assert_eq!(
            serde_json::from_value::<GenerateContentConfig>(v).unwrap(),
            cfg
        );
    }
}

/// genai v1.77.0 `FunctionResponsePart.MediaProcessing`.
#[test]
fn serde_function_response_part_media_processing() {
    let part = FunctionResponsePart {
        media_processing: Some(MediaProcessing::Agentic),
        ..Default::default()
    };
    let v = serde_json::to_value(&part).unwrap();
    assert_eq!(v, json!({"mediaProcessing": "AGENTIC"}));
    assert_eq!(
        serde_json::from_value::<FunctionResponsePart>(v).unwrap(),
        part
    );
    assert_eq!(
        serde_json::to_value(FunctionResponsePart::default()).unwrap(),
        json!({})
    );
    // Go's `omitempty` on the string type: a decoded `""` is not re-encoded (Part too).
    let empty: FunctionResponsePart =
        serde_json::from_value(json!({"mediaProcessing": ""})).unwrap();
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
    let part: Part = serde_json::from_value(json!({"text": "t", "mediaProcessing": ""})).unwrap();
    assert_eq!(serde_json::to_value(&part).unwrap(), json!({"text": "t"}));
}

/// genai v1.77.0 `LiveConnectConfig.Labels` and `LiveClientContextUpdate{,Tools}`: an unset
/// tools wrapper is omitted, a set one with no tools is `{}`.
#[test]
fn serde_live_labels_and_context_update() {
    let cfg = LiveConnectConfig {
        labels: [("k".to_string(), "v".to_string())].into_iter().collect(),
        ..Default::default()
    };
    let v = serde_json::to_value(&cfg).unwrap();
    assert_eq!(v, json!({"labels": {"k": "v"}}));
    assert_eq!(serde_json::from_value::<LiveConnectConfig>(v).unwrap(), cfg);
    assert_eq!(
        serde_json::to_value(LiveConnectConfig::default()).unwrap(),
        json!({})
    );

    for (update, want) in [
        (LiveClientContextUpdate::default(), json!({})),
        (
            LiveClientContextUpdate {
                tools: Some(LiveClientContextUpdateTools::default()),
                ..Default::default()
            },
            json!({"tools": {}}),
        ),
        (
            LiveClientContextUpdate {
                system_instruction: Some(Content::from_text("s", "user")),
                tools: Some(LiveClientContextUpdateTools {
                    tools: vec![Tool {
                        google_search: Some(GoogleSearch::default()),
                        ..Default::default()
                    }],
                }),
            },
            json!({
                "systemInstruction": {"parts": [{"text": "s"}], "role": "user"},
                "tools": {"tools": [{"googleSearch": {}}]},
            }),
        ),
    ] {
        let v = serde_json::to_value(&update).unwrap();
        assert_eq!(v, want);
        assert_eq!(
            serde_json::from_value::<LiveClientContextUpdate>(v).unwrap(),
            update
        );
    }
}

#[test]
fn serde_create_time_omitted_when_none_and_roundtrips() {
    let r = GenerateContentResponse::default();
    assert_eq!(serde_json::to_value(&r).unwrap(), json!({}));
    let r: GenerateContentResponse =
        serde_json::from_value(json!({"createTime": "2026-11-04T10:20:30.5Z"})).unwrap();
    let t = r.create_time.unwrap();
    assert_eq!(t.to_rfc3339(), "2026-11-04T10:20:30.500+00:00");
    let back: GenerateContentResponse =
        serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn serde_go_away_time_left_duration() {
    // genai LiveServerGoAway: InternalDurationJSON, encoded as whole seconds, zero omitted.
    let m: LiveServerMessage =
        serde_json::from_value(json!({"goAway": {"timeLeft": "1.5s"}})).unwrap();
    let ga = m.go_away.clone().unwrap();
    assert_eq!(ga.time_left, chrono::TimeDelta::milliseconds(1500));
    assert_eq!(
        serde_json::to_value(&m).unwrap(),
        json!({"goAway": {"timeLeft": "2s"}})
    );
    let zero: LiveServerGoAway = serde_json::from_value(json!({"timeLeft": ""})).unwrap();
    assert_eq!(serde_json::to_value(&zero).unwrap(), json!({}));
    assert!(serde_json::from_value::<LiveServerGoAway>(json!({"timeLeft": "soon"})).is_err());
}

#[test]
fn serde_int64_string_fields() {
    // Go `int64` with `json:",omitempty,string"`.
    let u: LiveServerSessionResumptionUpdate = serde_json::from_value(
        json!({"newHandle": "h", "resumable": true, "lastConsumedClientMessageIndex": "42"}),
    )
    .unwrap();
    assert_eq!(u.last_consumed_client_message_index, 42);
    assert_eq!(
        serde_json::to_value(&u).unwrap(),
        json!({"newHandle": "h", "resumable": true, "lastConsumedClientMessageIndex": "42"})
    );
    let f: File = serde_json::from_value(json!({"sizeBytes": "1024"})).unwrap();
    assert_eq!(f.size_bytes, Some(1024));
    assert_eq!(
        serde_json::to_value(&f).unwrap(),
        json!({"sizeBytes": "1024"})
    );
    assert!(serde_json::from_value::<File>(json!({"sizeBytes": "x"})).is_err());
}

#[test]
fn serde_citation_publication_date() {
    let c: Citation =
        serde_json::from_value(json!({"publicationDate": {"year": 2024, "month": 5}})).unwrap();
    assert_eq!(
        c.publication_date,
        CivilDate {
            year: 2024,
            month: 5,
            day: 0
        }
    );
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"publicationDate": {"year": 2024, "month": 5}})
    );
    let empty: Citation = serde_json::from_value(json!({"publicationDate": {}})).unwrap();
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
    assert!(serde_json::from_value::<Citation>(json!({"publicationDate": {"month": 5}})).is_err());
}

#[test]
fn serde_live_input_aliases_and_server_message() {
    let input: LiveClientContentInput = LiveSendClientContentParameters {
        turns: text("hi"),
        turn_complete: Some(true),
    };
    assert_eq!(
        serde_json::to_value(&input).unwrap(),
        json!({"turns": [{"parts": [{"text": "hi"}], "role": "user"}], "turnComplete": true})
    );
    let rt = LiveRealtimeInput {
        activity_start: Some(ActivityStart {}),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&rt).unwrap(),
        json!({"activityStart": {}})
    );
    let tr = LiveToolResponseInput::default();
    assert_eq!(serde_json::to_value(&tr).unwrap(), json!({}));

    let msg: LiveServerMessage = serde_json::from_value(json!({
        "serverContent": {"modelTurn": {"parts": [{"text": "ok"}], "role": "model"}, "turnComplete": true},
        "usageMetadata": {"totalTokenCount": 3, "serviceTier": "standard"}
    }))
    .unwrap();
    let sc = msg.server_content.unwrap();
    assert!(sc.turn_complete);
    assert_eq!(sc.model_turn.unwrap().parts[0].text, "ok");
    assert_eq!(
        msg.usage_metadata.unwrap().service_tier,
        Some(ServiceTier::Standard)
    );
}

#[test]
fn serde_tool_and_enums() {
    let tool = Tool {
        function_declarations: vec![FunctionDeclaration {
            name: "f".into(),
            ..Default::default()
        }],
        google_search: Some(GoogleSearch::default()),
        url_context: Some(URLContext {}),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&tool).unwrap(),
        json!({"functionDeclarations": [{"name": "f"}], "googleSearch": {}, "urlContext": {}})
    );
    let tc = ToolConfig {
        function_calling_config: Some(FunctionCallingConfig {
            mode: Some(FunctionCallingConfigMode::Validated),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&tc).unwrap(),
        json!({"functionCallingConfig": {"mode": "VALIDATED"}})
    );
    assert_eq!(
        FunctionCallingConfigMode::Unspecified.as_str(),
        "MODE_UNSPECIFIED"
    );
    assert_eq!(ServiceTier::Flex.as_str(), "flex");
    assert_eq!(FinishReason::Spii.as_str(), "SPII");
    assert_eq!(BlockedReason::ModelArmor.as_str(), "MODEL_ARMOR");
    assert_eq!(
        HarmBlockThreshold::BlockLowAndAbove.as_str(),
        "BLOCK_LOW_AND_ABOVE"
    );
    assert_eq!(MediaModality::Text.as_str(), "TEXT");
    assert_eq!(MediaResolution::High.as_str(), "MEDIA_RESOLUTION_HIGH");
    assert_eq!(
        ThinkingLevel::Unspecified.as_str(),
        "THINKING_LEVEL_UNSPECIFIED"
    );
    assert_eq!(ResourceScope::Collection.as_str(), "COLLECTION");
}

#[test]
fn part_from_file() {
    let f = File {
        uri: "gs://b/o".into(),
        mime_type: "image/png".into(),
        name: "files/1".into(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(Part::from_file(&f)).unwrap(),
        json!({"fileData": {"fileUri": "gs://b/o", "mimeType": "image/png"}})
    );
}

#[test]
fn serde_generated_image_bytes() {
    let r: GenerateImagesResponse = serde_json::from_value(json!({
        "generatedImages": [{"image": {"imageBytes": "dG9r", "mimeType": "image/png"}}]
    }))
    .unwrap();
    let img = r.generated_images[0].image.as_ref().unwrap();
    assert_eq!(&img.image_bytes[..], b"tok");
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"generatedImages": [{"image": {"imageBytes": "dG9r", "mimeType": "image/png"}}]})
    );
}
