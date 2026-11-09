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

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

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
    todo!("Task 3")
}

/// Go `ReplayedReasoning`: whether `part` is reasoning from an earlier turn carrying nothing
/// else (dropping it loses nothing).
pub(crate) fn replayed_reasoning(part: &Part) -> bool {
    todo!("Task 3")
}

/// Go `UnsupportedPayload`: the Go name of the first field on `part` the calling API has no
/// way to send (`None` for Go's `""`), fields in `emitted` excepted. Text, `Thought`,
/// `ThoughtSignature`, `FunctionCall`, `FunctionResponse`, `PartMetadata` and (unless the part
/// carries media an API emits) `VideoMetadata` and `MediaResolution` never count. Exhaustive
/// over `Part`, so a field genai adds is reported by default.
pub(crate) fn unsupported_payload(part: &Part, emitted: &[PartField]) -> Option<&'static str> {
    todo!("Task 3")
}
