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

//! JSON-schema helpers both APIs share (port of ADK-Go `internal/shared/schema.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

use gadk_genai::Schema;
use serde_json::{Map, Value};

use crate::error::OpenAiError;

/// Go `DefaultResponseFormatName`: the structured-output format name when the schema's title
/// is not one OpenAI accepts.
pub(crate) const DEFAULT_RESPONSE_FORMAT_NAME: &str = "adk_response";

/// Go `ResponseFormatName`: the schema's title when it matches `^[a-zA-Z0-9_-]{1,64}$`, else
/// [`DEFAULT_RESPONSE_FORMAT_NAME`].
pub(crate) fn response_format_name(schema: Option<&Schema>) -> String {
    todo!("Task 3")
}

/// Go `NormalizeSchema`: a JSON schema of any shape as a map, numbers kept as decoded. `None`
/// (Go nil) is the bare [`ErrorKind::EmptyJsonSchema`](crate::error::ErrorKind::EmptyJsonSchema);
/// JSON `null` is `Ok(None)` (Go's nil map, no error); a non-object is
/// `openai: unmarshal json schema: <err>`.
pub(crate) fn normalize_schema(
    schema: Option<&Value>,
) -> Result<Option<Map<String, Value>>, OpenAiError> {
    todo!("Task 3")
}

/// Go `EnforceStrictOpenAISchema`: every object type gets `properties` (added empty when
/// missing), `additionalProperties: false` and a sorted `required` naming every property; a
/// `$ref` keeps no siblings; recurses through `$defs`, `properties`, `anyOf`/`oneOf`/`allOf`
/// and an object `items`. Mutates in place.
pub(crate) fn enforce_strict_openai_schema(schema: &mut Map<String, Value>) {
    todo!("Task 3")
}

/// Go `SchemaToMap`: a genai schema as the JSON-schema map both APIs take, type names
/// lowercased at every depth; `None` is `Ok(None)`. Failures are `openai: marshal schema: …` /
/// `openai: unmarshal schema: …`.
pub(crate) fn schema_to_map(
    schema: Option<&Schema>,
) -> Result<Option<Map<String, Value>>, OpenAiError> {
    todo!("Task 3")
}
