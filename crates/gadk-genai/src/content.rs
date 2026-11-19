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
        /// Not specified.
        Unspecified => "MEDIA_PROCESSING_UNSPECIFIED",
        /// Process the media with a fixed, non-agentic pipeline.
        Static => "STATIC",
        /// Let the model process the media agentically (for example, zooming into regions).
        Agentic => "AGENTIC",
    }
);

crate::string_enum!(
    /// The outcome of a code execution.
    Outcome {
        /// Not specified.
        Unspecified => "OUTCOME_UNSPECIFIED",
        /// The code ran successfully.
        Ok => "OUTCOME_OK",
        /// The code ran but failed; the output usually contains the error.
        Failed => "OUTCOME_FAILED",
        /// The code ran too long and was cancelled.
        DeadlineExceeded => "OUTCOME_DEADLINE_EXCEEDED",
    }
);

crate::string_enum!(
    /// The programming language of executable code.
    Language {
        /// Not specified.
        Unspecified => "LANGUAGE_UNSPECIFIED",
        /// Python.
        Python => "PYTHON",
    }
);

crate::string_enum!(
    /// How a function response is scheduled into the conversation.
    FunctionResponseScheduling {
        /// Not specified.
        Unspecified => "SCHEDULING_UNSPECIFIED",
        /// Add the response to the conversation without interrupting or prompting the model.
        Silent => "SILENT",
        /// Add the response and let the model act on it once it is idle.
        WhenIdle => "WHEN_IDLE",
        /// Add the response and interrupt the model's current generation so it reacts immediately.
        Interrupt => "INTERRUPT",
    }
);

crate::string_enum!(
    /// The media resolution level of a part.
    PartMediaResolutionLevel {
        /// Not specified.
        MediaResolutionUnspecified => "MEDIA_RESOLUTION_UNSPECIFIED",
        /// Low resolution: fewer tokens per image or frame.
        MediaResolutionLow => "MEDIA_RESOLUTION_LOW",
        /// Medium resolution.
        MediaResolutionMedium => "MEDIA_RESOLUTION_MEDIUM",
        /// High resolution: more tokens per image or frame.
        MediaResolutionHigh => "MEDIA_RESOLUTION_HIGH",
        /// Ultra-high resolution.
        MediaResolutionUltraHigh => "MEDIA_RESOLUTION_ULTRA_HIGH",
    }
);

crate::string_enum!(
    /// The type of a server-side tool.
    ToolType {
        /// Not specified.
        Unspecified => "TOOL_TYPE_UNSPECIFIED",
        /// Google Search over web results.
        GoogleSearchWeb => "GOOGLE_SEARCH_WEB",
        /// Google Search over image results.
        GoogleSearchImage => "GOOGLE_SEARCH_IMAGE",
        /// URL context retrieval.
        UrlContext => "URL_CONTEXT",
        /// Google Maps grounding.
        GoogleMaps => "GOOGLE_MAPS",
        /// File search over file search stores.
        FileSearch => "FILE_SEARCH",
        /// Media processing.
        MediaProcessing => "MEDIA_PROCESSING",
    }
);

/// Media resolution of a part: a [`PartMediaResolutionLevel`] or a token count.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartMediaResolution {
    /// A predefined resolution level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<PartMediaResolutionLevel>,
    /// An explicit number of tokens to spend on the media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_tokens: Option<i32>,
}

/// A server-side tool call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// Identifier of the call, echoed by the matching [`ToolResponse`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    /// The server-side tool that was called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<ToolType>,
    /// The arguments of the call as a JSON object.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
}

/// The response to a server-side [`ToolCall`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponse {
    /// Identifier of the [`ToolCall`] this responds to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    /// The server-side tool that produced the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<ToolType>,
    /// The tool's output as a JSON object.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub response: Map<String, Value>,
}

/// Speaker and style metadata of a speech part.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechMetadata {
    /// Name of the speaker of the part.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub speaker: String,
    /// Speaking style of the part.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub style: String,
}

/// The result of executing [`ExecutableCode`]: an [`Outcome`] and its output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionResult {
    /// Whether the execution succeeded, failed or timed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Standard output on success, or the error message (or stderr) otherwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: String,
    /// Identifier of the [`ExecutableCode`] this result belongs to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
}

/// Code in a given [`Language`] that the model generated and can run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableCode {
    /// The source code to run.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub code: String,
    /// The language of `code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Identifier of the code, echoed by the matching [`CodeExecutionResult`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
}

/// A reference to a file by URI, with its MIME type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    /// Optional display name of the file.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    /// URI of the file, such as a Files API URI or a `gs://` URI.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_uri: String,
    /// IANA MIME type of the file data.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// One streamed fragment of a function-call argument, addressed by JSON path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialArg {
    /// A boolean value for the argument at `json_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bool_value: Option<bool>,
    /// JSON path of the argument this fragment belongs to (for example `$.foo.bar[0]`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub json_path: String,
    /// Set when the argument value is JSON `null` (the wire value is `"NULL_VALUE"`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub null_value: String,
    /// A numeric value for the argument at `json_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_value: Option<f64>,
    /// A string value (or string fragment) for the argument at `json_path`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub string_value: String,
    /// Whether more fragments for the same `json_path` follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
}

/// A function call predicted by the model: a name and its arguments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    /// Identifier of the call, echoed by the matching [`FunctionResponse`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    /// The function arguments as a JSON object.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
    /// Name of the function to call.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    /// Streamed argument fragments, when arguments are streamed incrementally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_args: Vec<PartialArg>,
    /// Whether more fragments of this call follow in later chunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
}

/// Inline media data carried in a [`FunctionResponsePart`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponseBlob {
    /// IANA MIME type of `data`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    /// The raw bytes; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub data: Bytes,
    /// Optional display name of the data.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
}

/// A file reference carried in a [`FunctionResponsePart`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponseFileData {
    /// URI of the file.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_uri: String,
    /// IANA MIME type of the file.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    /// Optional display name of the file.
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
    /// Inline media data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<FunctionResponseBlob>,
    /// A file reference.
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
    /// Whether the function keeps producing responses after this one (non-blocking calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub will_continue: Option<bool>,
    /// How the response is scheduled into the conversation (non-blocking calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<FunctionResponseScheduling>,
    /// Media parts of the response, such as images.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<FunctionResponsePart>,
    /// Identifier of the [`FunctionCall`] this responds to.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: String,
    /// Name of the function that was called.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    /// The function output as a JSON object; by convention under `"output"` or `"error"`.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub response: Map<String, Value>,
}

/// Inline binary data with its MIME type; serialized as base64.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    /// The raw bytes; base64 on the wire.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub data: Bytes,
    /// Optional display name of the data.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    /// IANA MIME type of `data`.
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
    /// Where the video segment ends; zero means the end of the video.
    pub end_offset: TimeDelta,
    /// Frame rate at which frames are sampled from the video.
    pub fps: Option<f64>,
    /// Where the video segment starts.
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
    /// The transcribed word.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub word: String,
    /// Start offset of the word in the audio, as a duration string.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_offset: String,
    /// End offset of the word in the audio, as a duration string.
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
    /// The transcribed text.
    pub text: String,
    /// Whether this is the final transcription of the utterance.
    pub finished: bool,
    /// BCP-47 language code of the transcribed speech.
    pub language_code: String,
    /// Label of the speaker, when speaker diarization is on.
    pub speaker_label: String,
    /// Per-word detail.
    pub words: Vec<WordInfo>,
    /// Start offset of the transcription in the audio.
    pub start_offset: TimeDelta,
    /// End offset of the transcription in the audio.
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
    /// Media resolution to use for this part's media, overriding the request-level setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_resolution: Option<PartMediaResolution>,
    /// The result of running [`ExecutableCode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<CodeExecutionResult>,
    /// Code generated by the model to be run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_code: Option<ExecutableCode>,
    /// A reference to a file by URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<FileData>,
    /// A function call predicted by the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    /// The result of a function call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    /// Inline binary data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    /// Text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text: String,
    /// Whether this part is the model's thought (reasoning) rather than its answer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub thought: bool,
    /// Opaque signature of the model's thought, to send back unchanged in later turns; base64.
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub thought_signature: Bytes,
    /// Which segment of the video, and at what frame rate, the model should see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_metadata: Option<VideoMetadata>,
    /// A server-side tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    /// The response to a server-side tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<ToolResponse>,
    /// Custom metadata attached to the part.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub part_metadata: Map<String, Value>,
    /// Transcription of audio in this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_transcription: Option<Transcription>,
    /// How the model processes this part's media.
    #[serde(default, skip_serializing_if = "media_processing_is_zero")]
    pub media_processing: Option<MediaProcessing>,
    /// Speaker and style metadata of a speech part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_metadata: Option<SpeechMetadata>,
}

/// A message: an ordered list of [`Part`]s attributed to a role.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    /// The parts of the message, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The producer of the message: [`ROLE_USER`] or [`ROLE_MODEL`]; empty when unset.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub role: String,
}
