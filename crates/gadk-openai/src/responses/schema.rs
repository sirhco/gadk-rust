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

//! The Responses JSON-schema text format.

use gadk_genai::GenerateContentConfig;

use crate::error::OpenAiError;
use crate::oaishared::JsonSchemaTag;
use crate::responses::wire::ResponseFormatTextJsonSchemaConfigParam;
use crate::shared::schema::{
    enforce_strict_openai_schema, normalize_schema, response_format_name, schema_to_map,
};

/// `text.format` for the config's `response_json_schema` (first) or
/// `response_schema`, made strict
/// ([`enforce_strict_openai_schema`](crate::shared::schema::enforce_strict_openai_schema)),
/// named by [`response_format_name`](crate::shared::schema::response_format_name), `strict:
/// true`; neither set is `openai: json schema requested without schema` (plain). The
/// caller's schema is not mutated.
pub(crate) fn new_json_schema_format(
    cfg: &GenerateContentConfig,
) -> Result<ResponseFormatTextJsonSchemaConfigParam, OpenAiError> {
    // Both conversions build a new map, so the strict rewrite below never reaches the
    // caller's schema.
    let mut schema = if let Some(json_schema) = &cfg.response_json_schema {
        normalize_schema(Some(json_schema))?
    } else if let Some(schema) = &cfg.response_schema {
        schema_to_map(Some(schema))?
    } else {
        return Err(OpenAiError::plain(
            "openai: json schema requested without schema",
        ));
    };
    if let Some(schema) = schema.as_mut() {
        enforce_strict_openai_schema(schema);
    }
    Ok(ResponseFormatTextJsonSchemaConfigParam {
        name: response_format_name(cfg.response_schema.as_ref()),
        schema,
        strict: Some(true),
        description: None,
        kind: JsonSchemaTag,
    })
}
