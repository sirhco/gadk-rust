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

//! Port of ADK-Go `internal/llminternal/googlellm/live_artifacts_test.go`.

use bytes::Bytes;
use gadk_genai::{Blob, FileData, Part};
use gadk_model::is_internal_live_artifact_part;

fn file(uri: &str, mime: &str) -> Part {
    Part {
        file_data: Some(FileData {
            file_uri: uri.to_string(),
            mime_type: mime.to_string(),
            ..FileData::default()
        }),
        ..Part::default()
    }
}

/// Both halves of the predicate: the `artifact://` scheme, and `_adk_live` as a whole path
/// segment rather than a substring of a name.
// parity: adapted: Go's "NilPart" case (a nil *genai.Part) has no Rust value; the other cases are Go's
#[test]
fn test_is_internal_live_artifact_part() {
    let cases = [
        (
            "LiveAudioRef",
            file(
                "artifact://app/user/session/_adk_live/adk_live_audio_storage_input_audio_1700000000000.pcm#0",
                "audio/pcm",
            ),
            true,
        ),
        // Only ADK's own artifact store uses this namespace, so the same segment under another
        // scheme is someone else's file.
        (
            "OtherSchemeWithLiveSegment",
            file("gs://bucket/_adk_live/clip.wav", "audio/wav"),
            false,
        ),
        // _adk_live has to match a whole path segment: a user's file may well have the string
        // in its name.
        (
            "LiveSegmentInsideFilename",
            file(
                "artifact://app/user/session/my_adk_live_notes.txt",
                "text/plain",
            ),
            false,
        ),
        (
            "UserVisibleArtifactRef",
            file(
                "artifact://app/user/session/report.pdf#1",
                "application/pdf",
            ),
            false,
        ),
        (
            "TextPart",
            Part {
                text: "artifact://app/user/session/_adk_live/audio.pcm#0".to_string(),
                ..Part::default()
            },
            false,
        ),
        (
            "InlineAudio",
            Part {
                inline_data: Some(Blob {
                    mime_type: "audio/pcm".to_string(),
                    data: Bytes::from_static(&[0x01]),
                    ..Blob::default()
                }),
                ..Part::default()
            },
            false,
        ),
    ];
    for (name, part, want) in cases {
        assert_eq!(
            is_internal_live_artifact_part(&part),
            want,
            "{name}: IsInternalLiveArtifactPart()"
        );
    }
}
