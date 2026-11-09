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

//! Port of ADK-Go `model/openaimodel/internal/shared/contents_test.go` (Task 3).

use bytes::Bytes;
use gadk_genai::{
    Blob, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse, Part,
    PartMediaResolution, PartMediaResolutionLevel, VideoMetadata,
};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::shared::contents::{
    PartField, flatten_content_text, replayed_reasoning, unsupported_payload,
};

fn text(t: &str) -> Part {
    Part {
        text: t.to_string(),
        ..Part::default()
    }
}

fn png_blob() -> Blob {
    Blob {
        mime_type: "image/png".to_string(),
        data: Bytes::from_static(&[1]),
        ..Blob::default()
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

// parity: adapted: a gadk `Content` holds `Part`s, not pointers, so the nil part between the two text parts of "valid text parts" has no counterpart and is left out
#[test]
fn test_flatten_content_text() {
    struct Case {
        name: &'static str,
        content: Option<Content>,
        want: &'static str,
        want_err: bool,
        want_err_text: &'static str,
    }
    let parts = |parts: Vec<Part>| {
        Some(Content {
            parts,
            ..Content::default()
        })
    };
    let ok = |name, content, want| Case {
        name,
        content,
        want,
        want_err: false,
        want_err_text: "",
    };
    let err = |name, content, want_err_text| Case {
        name,
        content,
        want: "",
        want_err: true,
        want_err_text,
    };
    let tests = vec![
        ok("nil content", None, ""),
        ok(
            "valid text parts",
            parts(vec![text("part1"), text("part2")]),
            "part1\npart2",
        ),
        err(
            "non-text part",
            parts(vec![Part {
                function_call: Some(FunctionCall {
                    name: "fn".to_string(),
                    ..FunctionCall::default()
                }),
                ..Part::default()
            }]),
            "non-text system instruction part",
        ),
        err(
            "media part without text keeps the non-text error",
            parts(vec![Part {
                inline_data: Some(png_blob()),
                ..Part::default()
            }]),
            "non-text system instruction part",
        ),
        // The instruction is sent as a string, so the image would be dropped while the text
        // beside it goes out.
        err(
            "text part carrying inline data",
            parts(vec![Part {
                inline_data: Some(png_blob()),
                ..text("describe it")
            }]),
            "InlineData",
        ),
        // The text gathered before the refused part is not returned.
        err(
            "text part carrying file data after a text part",
            parts(vec![
                text("part1"),
                Part {
                    file_data: Some(FileData {
                        mime_type: "image/png".to_string(),
                        file_uri: "https://example.com/a.png".to_string(),
                        ..FileData::default()
                    }),
                    ..text("describe it")
                },
            ]),
            "FileData",
        ),
        err(
            "text part carrying executable code",
            parts(vec![Part {
                executable_code: Some(ExecutableCode {
                    code: "print(1)".to_string(),
                    ..ExecutableCode::default()
                }),
                ..text("run this")
            }]),
            "ExecutableCode",
        ),
        // UnsupportedPayload counts a call as sent, which holds for contents but not for an
        // instruction.
        err(
            "text part carrying a function call",
            parts(vec![Part {
                function_call: Some(FunctionCall {
                    name: "fn".to_string(),
                    ..FunctionCall::default()
                }),
                ..text("be terse")
            }]),
            "FunctionCall",
        ),
        err(
            "text part carrying a function response",
            parts(vec![Part {
                function_response: Some(FunctionResponse {
                    name: "fn".to_string(),
                    response: object(json!({"ok": true})),
                    ..FunctionResponse::default()
                }),
                ..text("be terse")
            }]),
            "FunctionResponse",
        ),
        // A signature and caller metadata carry nothing a model reads, so the text still goes
        // out alone.
        ok(
            "text part with fields that carry no payload",
            parts(vec![Part {
                thought_signature: Bytes::from_static(&[1]),
                part_metadata: object(json!({"k": "v"})),
                ..text("part1")
            }]),
            "part1",
        ),
    ];

    for tc in tests {
        let name = tc.name;
        let result = flatten_content_text(tc.content.as_ref());
        assert_eq!(
            result.is_err(),
            tc.want_err,
            "{name}: FlattenContentText() error = {result:?}, wantErr {}",
            tc.want_err
        );
        let txt = match result {
            Ok(txt) => txt,
            Err(err) => {
                assert!(
                    err.to_string().contains(tc.want_err_text),
                    "{name}: FlattenContentText() error = {err}, want it to name {:?}",
                    tc.want_err_text
                );
                String::new()
            }
        };
        assert_eq!(txt, tc.want, "{name}");
    }
}

/// Pins the default answer: while one API can emit media and the other cannot, the one that
/// cannot must keep refusing it rather than dropping it in silence.
#[test]
fn test_unsupported_payload_media_is_reported_until_an_endpoint_claims_it() {
    let tests = vec![
        (
            "inline_data",
            Part {
                inline_data: Some(png_blob()),
                ..Part::default()
            },
            "InlineData",
        ),
        (
            "file_data",
            Part {
                file_data: Some(FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: "file-a".to_string(),
                    ..FileData::default()
                }),
                ..Part::default()
            },
            "FileData",
        ),
        // A part carrying both is reported for FileData, which genai.Part declares ahead of
        // InlineData; ClassifyMedia emits them the other way round, and that is not a defect
        // in either.
        (
            "both_fields_report_the_one_declared_first",
            Part {
                inline_data: Some(png_blob()),
                file_data: Some(FileData {
                    mime_type: "application/pdf".to_string(),
                    file_uri: "file-b".to_string(),
                    ..FileData::default()
                }),
                ..Part::default()
            },
            "FileData",
        ),
    ];

    for (name, part, want) in tests {
        assert_eq!(
            unsupported_payload(&part, &[]),
            Some(want),
            "{name}: an endpoint with no wire field for media must refuse it, not drop it"
        );
        assert_eq!(
            unsupported_payload(&part, &[PartField::Media]),
            None,
            "{name}: UnsupportedPayload(PartFieldMedia)"
        );
    }
}

/// A part carrying media and something unsendable is still reported for the something.
#[test]
fn test_unsupported_payload_emitting_media_does_not_hide_anything_else() {
    let part = Part {
        inline_data: Some(png_blob()),
        executable_code: Some(ExecutableCode {
            code: "print(1)".to_string(),
            ..ExecutableCode::default()
        }),
        ..Part::default()
    };
    assert_eq!(
        unsupported_payload(&part, &[PartField::Media]),
        Some("ExecutableCode")
    );
}

/// Reasoning is droppable only when the part holds nothing else, and media is something else
/// no matter which API asks.
#[test]
fn test_replayed_reasoning_media_still_disqualifies() {
    let part = Part {
        thought: true,
        inline_data: Some(png_blob()),
        ..text("scratch")
    };
    assert!(
        !replayed_reasoning(&part),
        "ReplayedReasoning() = true for a thought carrying an image, want false: dropping the part would drop the image with it"
    );
}

/// MediaResolution and VideoMetadata qualify media: reported on a part whose media is sent,
/// accounted for on a part with none.
#[test]
fn test_unsupported_payload_media_resolution_is_lost_no_longer_silently() {
    let high = || {
        Some(PartMediaResolution {
            level: Some(PartMediaResolutionLevel::MediaResolutionHigh),
            ..PartMediaResolution::default()
        })
    };
    let tests = vec![
        (
            "on_media_that_is_sent_it_is_reported",
            Part {
                inline_data: Some(png_blob()),
                media_resolution: high(),
                ..Part::default()
            },
            Some("MediaResolution"),
        ),
        (
            "on_file_data_that_is_sent_it_is_reported",
            Part {
                file_data: Some(FileData {
                    mime_type: "image/png".to_string(),
                    file_uri: "file-a".to_string(),
                    ..FileData::default()
                }),
                media_resolution: high(),
                ..Part::default()
            },
            Some("MediaResolution"),
        ),
        // A GIF part with an FPS: the image is sent, the FPS has no field.
        (
            "video_metadata_on_media_that_is_sent_is_reported",
            Part {
                inline_data: Some(Blob {
                    mime_type: "image/gif".to_string(),
                    ..png_blob()
                }),
                video_metadata: Some(VideoMetadata::default()),
                ..Part::default()
            },
            Some("VideoMetadata"),
        ),
        // Nothing for it to qualify, so nothing is lost by dropping it.
        (
            "without_media_it_qualifies_nothing",
            Part {
                media_resolution: high(),
                ..text("hi")
            },
            None,
        ),
        (
            "video_metadata_without_media_qualifies_nothing",
            Part {
                video_metadata: Some(VideoMetadata::default()),
                ..text("hi")
            },
            None,
        ),
    ];
    for (name, part, want) in tests {
        assert_eq!(
            unsupported_payload(&part, &[PartField::Media]),
            want,
            "{name}: UnsupportedPayload(PartFieldMedia)"
        );
        // Without the claim the media itself is reported first, exactly as before.
        if part.inline_data.is_some() || part.file_data.is_some() {
            assert_ne!(
                unsupported_payload(&part, &[]),
                Some("MediaResolution"),
                "{name}: UnsupportedPayload() must name the media field itself"
            );
        }
    }
}
