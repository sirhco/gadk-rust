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

//! Content helpers both APIs share (port of ADK-Go `internal/shared/contents.go`).

use gadk_genai::{Content, Part};

use crate::error::OpenAiError;

/// Go `PartField`: a group of `genai.Part` fields an API emits itself and passes to
/// [`unsupported_payload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PartField {
    /// Go `PartFieldMedia`: `InlineData` and `FileData`.
    Media,
}

/// Go `FlattenContentText`: the content's text parts joined with `\n`; `None` is `""`. A part
/// without text is `non-text system instruction part *genai.Part`; a part carrying anything
/// besides its text is `unsupported system instruction part: <Field>` (plain errors; callers
/// wrap them as `openai: system instruction: %w`).
pub(crate) fn flatten_content_text(content: Option<&Content>) -> Result<String, OpenAiError> {
    let Some(content) = content else {
        return Ok(String::new());
    };
    let mut out = String::new();
    for part in &content.parts {
        if part.text.is_empty() {
            return Err(OpenAiError::plain(
                "non-text system instruction part *genai.Part",
            ));
        }
        // Only the text is sent, so anything else on the part would be dropped while the text
        // beside it goes out. A call and its result count as sent in contents, not here.
        let field = if part.function_call.is_some() {
            Some("FunctionCall")
        } else if part.function_response.is_some() {
            Some("FunctionResponse")
        } else {
            unsupported_payload(part, &[])
        };
        if let Some(field) = field {
            return Err(OpenAiError::plain(format!(
                "unsupported system instruction part: {field}"
            )));
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&part.text);
    }
    Ok(out)
}

/// Go `ReplayedReasoning`: whether `part` is reasoning from an earlier turn carrying nothing
/// else (dropping it loses nothing).
pub(crate) fn replayed_reasoning(part: &Part) -> bool {
    // A signature can arrive on a part of its own with the marker unset.
    if !part.thought && part.thought_signature.is_empty() {
        return false;
    }
    // Text on a part not marked as a thought is an answer, signature or not.
    if !part.text.is_empty() && !part.thought {
        return false;
    }
    // Marking a call or anything else as a thought must not make it vanish.
    part.function_call.is_none()
        && part.function_response.is_none()
        && unsupported_payload(part, &[]).is_none()
}

/// Go `UnsupportedPayload`: the Go name of the first field on `part` the calling API has no
/// way to send (`None` for Go's `""`), fields in `emitted` excepted. Text, `Thought`,
/// `ThoughtSignature`, `FunctionCall`, `FunctionResponse`, `PartMetadata` and (unless the part
/// carries media an API emits) `VideoMetadata` and `MediaResolution` never count. Exhaustive
/// over `Part`, so a field genai adds is reported by default.
pub(crate) fn unsupported_payload(part: &Part, emitted: &[PartField]) -> Option<&'static str> {
    let Part {
        media_resolution,
        code_execution_result,
        executable_code,
        file_data,
        function_call: _,     // sent as a call
        function_response: _, // sent as the call's result
        inline_data,
        text: _,              // sent, or dropped when it is reasoning
        thought: _,           // the marker deciding which
        thought_signature: _, // no request field on either API carries one
        video_metadata,
        tool_call,
        tool_response,
        part_metadata: _, // caller bookkeeping, never content
        audio_transcription,
        media_processing,
        speech_metadata,
    } = part;
    let emits_media = emitted.contains(&PartField::Media);
    // A qualifier is harmless only while nothing is sent for it to qualify: on a part whose
    // media an API emits, no wire field carries it, so it is reported by name.
    let qualifies_sent_media = emits_media && (inline_data.is_some() || file_data.is_some());
    // genai `Part` declaration order: the first set field is the one reported.
    [
        (
            "MediaResolution",
            qualifies_sent_media && media_resolution.is_some(),
        ),
        ("CodeExecutionResult", code_execution_result.is_some()),
        ("ExecutableCode", executable_code.is_some()),
        ("FileData", !emits_media && file_data.is_some()),
        ("InlineData", !emits_media && inline_data.is_some()),
        (
            "VideoMetadata",
            qualifies_sent_media && video_metadata.is_some(),
        ),
        ("ToolCall", tool_call.is_some()),
        ("ToolResponse", tool_response.is_some()),
        ("AudioTranscription", audio_transcription.is_some()),
        (
            "MediaProcessing",
            media_processing
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
        ("SpeechMetadata", speech_metadata.is_some()),
    ]
    .into_iter()
    .find_map(|(name, set)| set.then_some(name))
}
