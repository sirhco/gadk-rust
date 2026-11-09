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

//! The Responses JSON-schema text format (port of ADK-Go `internal/responses/request.go`'s
//! `newJSONSchemaFormat`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 5 fills the bodies.

use gadk_genai::GenerateContentConfig;

use crate::error::OpenAiError;
use crate::responses::wire::ResponseFormatTextJsonSchemaConfigParam;

/// Go `newJSONSchemaFormat`: `text.format` for the config's `response_json_schema` (first) or
/// `response_schema`, made strict
/// ([`enforce_strict_openai_schema`](crate::shared::schema::enforce_strict_openai_schema)),
/// named by [`response_format_name`](crate::shared::schema::response_format_name), `strict:
/// true`; neither set is `openai: json schema requested without schema` (plain). The
/// caller's schema is not mutated.
pub(crate) fn new_json_schema_format(
    cfg: &GenerateContentConfig,
) -> Result<ResponseFormatTextJsonSchemaConfigParam, OpenAiError> {
    todo!("Task 5")
}
