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

//! Event and part helpers shared by the summarizer and the engine's compaction (ports of Go
//! internal/utils `Content` and `IsProsePart`).

use gadk_genai::{Content, Part};

use crate::Event;

/// Port of Go utils.Content: the content of the event's response.
#[doc(hidden)]
pub fn content(ev: &Event) -> Option<&Content> {
    ev.response.content.as_ref()
}

/// Port of Go utils.IsProsePart: plain text meant to be read, and nothing else (thoughts are not
/// prose).
#[doc(hidden)]
pub fn is_prose_part(p: &Part) -> bool {
    !p.text.is_empty()
        && !p.thought
        && p.function_call.is_none()
        && p.function_response.is_none()
        && p.executable_code.is_none()
        && p.code_execution_result.is_none()
        && p.file_data.is_none()
        && p.inline_data.is_none()
        && p.tool_call.is_none()
        && p.tool_response.is_none()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use gadk_genai::{Blob, FunctionCall};
    use gadk_model::LlmResponse;

    #[test]
    fn content_is_the_response_content() {
        let mut ev = Event::default();
        assert!(content(&ev).is_none());
        ev.response = LlmResponse {
            content: Some(Content::from_text("hi", "user")),
            ..Default::default()
        };
        assert_eq!(content(&ev).unwrap().parts[0].text, "hi");
    }

    #[test]
    fn is_prose_part_matches_go() {
        assert!(is_prose_part(&Part::from_text("hello")));
        assert!(!is_prose_part(&Part::default()), "empty text is not prose");
        assert!(
            !is_prose_part(&Part {
                thought: true,
                ..Part::from_text("thinking")
            }),
            "a thought is not prose"
        );
        assert!(!is_prose_part(&Part {
            function_call: Some(FunctionCall {
                name: "f".into(),
                ..Default::default()
            }),
            ..Part::from_text("text beside a call")
        }));
        assert!(!is_prose_part(&Part {
            inline_data: Some(Blob::default()),
            ..Part::from_text("caption")
        }));
    }
}
