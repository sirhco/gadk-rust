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

//! Chat Completions request building (port of ADK-Go `internal/completions/request.go`:
//! `buildParams`, `convertContents`, `convertContent`, `newMediaMessage`, `newMediaPart`,
//! `normalizeRole`, `joinText`, `newToolCall`, `newToolMessage`, `serviceTiers`,
//! `supportedConfigFields`, `applyGenerationConfig`, `newResponseFormat`). Only
//! [`build_params`] is shared; the rest are Task 4's private helpers.

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 4 fills the bodies.

use gadk_model::LlmRequest;

use crate::completions::wire::ChatCompletionNewParams;
use crate::error::OpenAiError;

/// Go `buildParams`: the Chat Completions params for `req`, sent as `model_name` unless
/// `req.model` names a model. Order: contents ([`ErrorKind::NoContents`](crate::error::ErrorKind::NoContents)
/// when they convert to no message), then the generation config (`req.config` with
/// `req.http_options`), then tools, then the tool choice (sent only alongside tools). Go's
/// nil-request check (`ErrRequestNil`) has no counterpart: an `LlmRequest` is never nil.
pub(crate) fn build_params(
    model_name: &str,
    req: &LlmRequest,
) -> Result<ChatCompletionNewParams, OpenAiError> {
    todo!("Task 4")
}
