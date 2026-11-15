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

//! Parity port of Go internal/llminternal/foreign_payloads_test.go.

use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, MediaProcessing, Part,
    PartMediaResolution, PartMediaResolutionLevel, SpeechMetadata, ToolCall, ToolResponse,
    Transcription, VideoMetadata,
};
use gadk_model::LlmResponse;
use gadk_session::Event;
use serde_json::json;

use crate::llm::processors::contents::convert_foreign_event;

/// The payload parts of Go's table: (name, part).
fn payloads() -> Vec<(&'static str, Part)> {
    vec![
        (
            "inline data",
            Part {
                inline_data: Some(Blob {
                    mime_type: "image/png".into(),
                    data: vec![1u8].into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "file data",
            Part {
                file_data: Some(FileData {
                    file_uri: "gs://test/image".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "executable code",
            Part {
                executable_code: Some(ExecutableCode {
                    code: "1 + 1".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "code result",
            Part {
                code_execution_result: Some(CodeExecutionResult {
                    output: "2".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "server tool call",
            Part {
                tool_call: Some(ToolCall {
                    id: "call".into(),
                    args: json!({"input": "test"}).as_object().cloned().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "server tool response",
            Part {
                tool_response: Some(ToolResponse {
                    id: "call".into(),
                    response: json!({"output": "test"}).as_object().cloned().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "audio transcription",
            Part {
                audio_transcription: Some(Transcription {
                    text: "spoken answer".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ]
}

fn video_metadata() -> VideoMetadata {
    VideoMetadata {
        end_offset: chrono::TimeDelta::seconds(1),
        ..Default::default()
    }
}

fn media_resolution() -> PartMediaResolution {
    PartMediaResolution {
        level: Some(PartMediaResolutionLevel::MediaResolutionHigh),
        ..Default::default()
    }
}

fn speech_metadata() -> SpeechMetadata {
    SpeechMetadata {
        speaker: "speaker".into(),
        style: "calm".into(),
    }
}

fn foreign_event(parts: Vec<Part>) -> Event {
    Event {
        author: "otherAgent".into(),
        response: LlmResponse {
            content: Some(Content::from_parts(parts, "model")),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn test_convert_foreign_event_retains_payload_parts() {
    for (payload_name, payload) in payloads() {
        for signed in [false, true] {
            for metadata in [false, true] {
                let mut name = format!(
                    "{} {payload_name}",
                    if signed { "signed" } else { "unsigned" }
                );
                if metadata {
                    name.push_str(" with metadata");
                }
                let mut part = payload.clone();
                if metadata {
                    part.video_metadata = Some(video_metadata());
                    part.media_resolution = Some(media_resolution());
                    part.media_processing = Some(MediaProcessing::Static);
                    part.speech_metadata = Some(speech_metadata());
                }
                if signed {
                    part.thought_signature = vec![3u8, 4].into();
                }
                let event = foreign_event(vec![
                    Part::default(),
                    Part {
                        thought_signature: vec![1u8, 2].into(),
                        ..Default::default()
                    },
                    Part {
                        part_metadata: json!({"source": "test"}).as_object().cloned().unwrap(),
                        ..Default::default()
                    },
                    Part {
                        video_metadata: Some(video_metadata()),
                        ..Default::default()
                    },
                    Part {
                        media_resolution: Some(media_resolution()),
                        ..Default::default()
                    },
                    Part {
                        media_processing: Some(MediaProcessing::Static),
                        ..Default::default()
                    },
                    Part {
                        speech_metadata: Some(speech_metadata()),
                        ..Default::default()
                    },
                    part.clone(),
                ]);
                let before = serde_json::to_string(&event).expect("could not snapshot the event");
                let got = convert_foreign_event(&event);
                let got_parts = got
                    .as_ref()
                    .and_then(|e| e.response.content.as_ref())
                    .map(|c| c.parts.clone())
                    .unwrap_or_else(|| panic!("{name}: foreign payload was discarded"));
                assert!(
                    got_parts == vec![Part::from_text("For context:"), part.clone()],
                    "{name}: foreign conversion did not retain only the payload and its attached metadata: {got_parts:?}"
                );
                let after = serde_json::to_string(&event)
                    .expect("could not snapshot the event after conversion");
                assert_eq!(
                    before, after,
                    "{name}: foreign conversion modified the stored event"
                );
            }
        }
    }
}

#[test]
fn test_convert_foreign_event_drops_thought_payload_parts() {
    for (payload_name, payload) in payloads() {
        for text in ["", "private reasoning"] {
            for mixed in [false, true] {
                let mut name = format!(
                    "{payload_name} {}",
                    if text.is_empty() {
                        "without text"
                    } else {
                        "with text"
                    }
                );
                if mixed {
                    name.push_str(" beside public payload");
                }
                let mut thought = payload.clone();
                thought.thought = true;
                thought.text = text.into();
                let mut parts = vec![thought];
                let mut want = Vec::new();
                if mixed {
                    let public = payload.clone();
                    parts.push(Part::from_text("public answer"));
                    parts.push(public.clone());
                    want = vec![
                        Part::from_text("For context:"),
                        Part::from_text("[otherAgent] said: public answer"),
                        public,
                    ];
                }
                let event = foreign_event(parts);
                let before = serde_json::to_string(&event).expect("could not snapshot the event");
                let got = convert_foreign_event(&event);
                if mixed {
                    let got_parts = got
                        .as_ref()
                        .and_then(|e| e.response.content.as_ref())
                        .map(|c| c.parts.clone());
                    assert!(
                        got_parts.as_ref() == Some(&want),
                        "{name}: foreign conversion did not retain only the public payloads: {got_parts:?}"
                    );
                } else {
                    assert!(
                        got.is_none(),
                        "{name}: foreign thought-only payload was relayed"
                    );
                }
                let after = serde_json::to_string(&event)
                    .expect("could not snapshot the event after conversion");
                assert_eq!(
                    before, after,
                    "{name}: foreign conversion modified the stored event"
                );
            }
        }
    }
}
