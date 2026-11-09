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

//! Responses request building (port of ADK-Go `internal/responses/request.go`: `buildParams`,
//! `convertContents`, `bufferedContent`, `newMessage`, `newMediaContent`, `newOutputMessage`,
//! `normalizeRole`, `newFunctionCall`, `newFunctionResponse`; the config half is
//! [`crate::responses::generation_config`], the schema half [`crate::responses::schema`]).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 5 fills the bodies.

use gadk_genai::{Content, FunctionResponse};
use gadk_model::LlmRequest;

use crate::error::OpenAiError;
use crate::responses::wire::{
    EasyInputMessageRole, ResponseInputItem, ResponseInputItemFunctionCallOutputParam,
    ResponseNewParams,
};
use crate::shared::calls::CallTracker;

/// Go `buildParams`: the Responses params for `req`, sent as `model_name` unless `req.model`
/// names a model. Contents that convert to no item are the bare
/// [`ErrorKind::NoContents`](crate::error::ErrorKind::NoContents), or
/// `ErrNoContents: every part was dropped as replayed reasoning` when a dropped thought
/// emptied them; then the generation config
/// ([`apply_generation_config`](crate::responses::generation_config::apply_generation_config)
/// with `req.config` and `req.http_options`), tools, and the tool choice. Go's nil-request
/// check has no counterpart.
pub(crate) fn build_params(
    model_name: &str,
    req: &LlmRequest,
) -> Result<ResponseNewParams, OpenAiError> {
    todo!("Task 5")
}

/// Go `convertContents`: the input items for `contents`, and whether any part was dropped as
/// replayed reasoning.
pub(crate) fn convert_contents(
    contents: &[Content],
) -> Result<(Vec<ResponseInputItem>, bool), OpenAiError> {
    todo!("Task 5")
}

/// Go `normalizeRole`: `""`/`user` → user, `model` → assistant, `system`, `developer`; else
/// `openai: unsupported role "<role>"` (plain).
pub(crate) fn normalize_role(role: &str) -> Result<EasyInputMessageRole, OpenAiError> {
    todo!("Task 5")
}

/// Go `newFunctionResponse`: the `function_call_output` item answering `fr`'s call
/// ([`CallTracker::resolve_response_id`]; payload
/// [`marshal_function_response`](crate::shared::calls::marshal_function_response)).
pub(crate) fn new_function_response(
    tracker: &mut CallTracker,
    fr: &FunctionResponse,
) -> Result<ResponseInputItemFunctionCallOutputParam, OpenAiError> {
    todo!("Task 5")
}
