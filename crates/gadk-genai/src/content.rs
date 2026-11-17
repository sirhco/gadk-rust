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

//! Content and Part types.

use crate::serde_util::{base64_bytes, decode_duration, format_go_seconds, is_zero};
use bytes::Bytes;
use chrono::TimeDelta;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The role of messages from the user.
pub const ROLE_USER: &str = "user";
/// The role of messages from the model.
pub const ROLE_MODEL: &str = "model";

crate::string_enum!(
    /// How the model processes the media of a part.
    MediaProcessing {
        Unspecified => "MEDIA_PROCESSING_UNSPECIFIED",
        Static => "STATIC",
        Agentic => "AGENTIC",
    }
);

crate::string_enum!(
    /// The outcome of a code execution.
    Outcome {
        Unspecified => "OUTCOME_UNSPECIFIED",
        Ok => "OUTCOME_OK",
        Failed => "OUTCOME_FAILED",
        DeadlineExceeded => "OUTCOME_DEADLINE_EXCEEDED",
    }
);

crate::string_enum!(
    /// The programming language of executable code.
    Language {
        Unspecified => "LANGUAGE_UNSPECIFIED",
        Python => "PYTHON",
    }
);

crate::string_enum!(
    /// How a function response is scheduled into the conversation.
    FunctionResponseScheduling {
        Unspecified => "SCHEDULING_UNSPECIFIED",
        Silent => "SILENT",
        WhenIdle => "WHEN_IDLE",
        Interrupt => "INTERRUPT",
    }
);

crate::string_enum!(
    /// The media resolution level of a part.
    PartMediaResolutionLevel {
        MediaResolutionUnspecified => "MEDIA_RESOLUTION_UNSPECIFIED",
        MediaResolutionLow => "MEDIA_RESOLUTION_LOW",
        MediaResolutionMedium => "MEDIA_RESOLUTION_MEDIUM",
        MediaResolutionHigh => "MEDIA_RESOLUTION_HIGH",
        MediaResolutionUltraHigh => "MEDIA_RESOLUTION_ULTRA_HIGH",
    }
);

crate::string_enum!(
    /// The type of a server-side tool.
    ToolType {
        Unspecified => "TOOL_TYPE_UNSPECIFIED",
        GoogleSearchWeb => "GOOGLE_SEARCH_WEB",
        GoogleSearchImage => "GOOGLE_SEARCH_IMAGE",
        UrlContext => "URL_CONTEXT",
        GoogleMaps => "GOOGLE_MAPS",
        FileSearch => "FILE_SEARCH",
        MediaProcessing => "MEDIA_PROCESSING",
    }
);

/// Media resolution of a part: a [`PartMediaResolutionLevel`] or a token count.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartMediaResolution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PartMediaResolutionLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_tokens: Option<i32>,
}

/// A server-side tool call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<ToolType>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
}

/// The response to a server-side [`ToolCall`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponse {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<ToolType>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub response: Map<String, Value>,
}

/// Speaker and style metadata of a speech part.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechMetadata {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub speaker: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub style: String,
}

/// The result of executing [`ExecutableCode`]: an [`Outcome`] and its output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
}

/// Code in a given [`Language`] that the model generated and can run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableCode {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
}

/// A reference to a file by URI, with its MIME type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// One streamed fragment of a function-call argument, addressed by JSON path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialArg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bool_value: Option<bool>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub json_path: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub null_value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_value: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub string_value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
}

/// A function call predicted by the model: a name and its arguments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_args: Vec<PartialArg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
}

/// Inline media data carried in a [`FunctionResponsePart`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponseBlob {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub data: Bytes,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
}

/// A file reference carried in a [`FunctionResponsePart`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponseFileData {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
}

/// Omission rule for an optional [`MediaProcessing`]: unset or `""` is omitted (a decoded `""` is
/// `Some(MediaProcessing::Unknown(""))`).
fn media_processing_is_zero(v: &Option<MediaProcessing>) -> bool {
    v.as_ref().is_none_or(|m| m.as_str().is_empty())
}

/// A media part of a [`FunctionResponse`]: inline data or a file reference.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponsePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<FunctionResponseBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<FunctionResponseFileData>,
    /// Optional. How the model processes this part's media (video parts only). Not supported
    /// in Vertex AI. `""` is not serialized.
    #[serde(default, skip_serializing_if = "media_processing_is_zero")]
    pub media_processing: Option<MediaProcessing>,
}

/// The result of a function call, sent back to the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<FunctionResponseScheduling>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<FunctionResponsePart>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub response: Map<String, Value>,
}

/// Inline binary data with its MIME type; serialized as base64.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub data: Bytes,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// Start and end offsets (and optional frame rate) of a video part.
///
/// Offsets are [`TimeDelta`] values; on the wire they are duration strings, encoded as whole
/// seconds (`"3s"`), zero offsets omitted, and a non-zero end offset forces `"startOffset": "0s"`
/// when the start offset is zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "VideoMetadataWire", into = "VideoMetadataWire")]
pub struct VideoMetadata {
    pub end_offset: TimeDelta,
    pub fps: Option<f64>,
    pub start_offset: TimeDelta,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoMetadataWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end_offset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_offset: Option<String>,
}

impl TryFrom<VideoMetadataWire> for VideoMetadata {
    type Error = serde_json::Error;
    fn try_from(w: VideoMetadataWire) -> Result<Self, Self::Error> {
        Ok(Self {
            end_offset: decode_duration(w.end_offset)?,
            fps: w.fps,
            start_offset: decode_duration(w.start_offset)?,
        })
    }
}

impl From<VideoMetadata> for VideoMetadataWire {
    fn from(v: VideoMetadata) -> Self {
        let mut start_offset =
            (!v.start_offset.is_zero()).then(|| format_go_seconds(v.start_offset));
        let end_offset = (!v.end_offset.is_zero()).then(|| format_go_seconds(v.end_offset));
        if end_offset.is_some() && start_offset.is_none() {
            start_offset = Some("0s".to_string());
        }
        Self {
            end_offset,
            fps: v.fps,
            start_offset,
        }
    }
}

/// A word of a transcription with its start and end offsets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordInfo {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub word: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_offset: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub end_offset: String,
}

/// A transcription of audio, with its language and optional word-level detail.
///
/// Offsets are [`TimeDelta`] values; on the wire they are duration strings (whole seconds, zero
/// offsets omitted).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TranscriptionWire", into = "TranscriptionWire")]
pub struct Transcription {
    pub text: String,
    pub finished: bool,
    pub language_code: String,
    pub speaker_label: String,
    pub words: Vec<WordInfo>,
    pub start_offset: TimeDelta,
    pub end_offset: TimeDelta,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptionWire {
    #[serde(default, skip_serializing_if = "is_zero")]
    text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    finished: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    language_code: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    speaker_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    words: Vec<WordInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_offset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end_offset: Option<String>,
}

impl TryFrom<TranscriptionWire> for Transcription {
    type Error = serde_json::Error;
    fn try_from(w: TranscriptionWire) -> Result<Self, Self::Error> {
        Ok(Self {
            text: w.text,
            finished: w.finished,
            language_code: w.language_code,
            speaker_label: w.speaker_label,
            words: w.words,
            start_offset: decode_duration(w.start_offset)?,
            end_offset: decode_duration(w.end_offset)?,
        })
    }
}

impl From<Transcription> for TranscriptionWire {
    fn from(t: Transcription) -> Self {
        Self {
            text: t.text,
            finished: t.finished,
            language_code: t.language_code,
            speaker_label: t.speaker_label,
            words: t.words,
            start_offset: (!t.start_offset.is_zero()).then(|| format_go_seconds(t.start_offset)),
            end_offset: (!t.end_offset.is_zero()).then(|| format_go_seconds(t.end_offset)),
        }
    }
}

/// A piece of a [`Content`] message: text, inline data, a file reference, a function call or
/// response, executable code and its result, or a server-side tool call or response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<PartMediaResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<CodeExecutionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_code: Option<ExecutableCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<FileData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thought: bool,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub thought_signature: Bytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_metadata: Option<VideoMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<ToolResponse>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub part_metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_transcription: Option<Transcription>,
    #[serde(default, skip_serializing_if = "media_processing_is_zero")]
    pub media_processing: Option<MediaProcessing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_metadata: Option<SpeechMetadata>,
}

/// A message: an ordered list of [`Part`]s attributed to a role.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub role: String,
}
