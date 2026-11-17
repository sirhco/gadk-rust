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

//! The `load_artifacts` tool.
//!
//! The tool tells the model which artifacts the session has. When the model asks for them with a
//! `load_artifacts` function call, the next request receives their content. The call itself only
//! echoes the names; the loading happens in the following `process_request`.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::try_join_all;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Artifacts, Context, RunnableTool, Tool, ToolError};
use gadk_genai::{Content, FunctionDeclaration, Part, ROLE_USER, Schema, Type};
use gadk_model::LlmRequest;
use gadk_platform::go_type_name;
use serde_json::{Map, Value};

use crate::go_fmt::wrap_err;
use crate::load_memory::go_error;

const NAME: &str = "load_artifacts";
const DESCRIPTION: &str = "Loads the artifacts and adds them to the session.";

/// A `load_artifacts` tool.
pub fn new() -> Arc<dyn Tool> {
    Arc::new(ArtifactsTool)
}

/// The `load_artifacts` tool.
struct ArtifactsTool;

#[async_trait]
impl Tool for ArtifactsTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    /// The schema is typed, with no `required` and no property description.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: NAME.into(),
            description: DESCRIPTION.into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: BTreeMap::from([(
                    "artifact_names".to_string(),
                    Schema {
                        r#type: Some(Type::Array),
                        items: Some(Box::new(Schema {
                            r#type: Some(Type::String),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    /// Lists the session's artifacts in the instructions and loads those the model asked for. The
    /// flow has already packed the tool by now; a missing artifact service fails the request.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let Some(artifacts) = ctx.artifacts() else {
            return Err(go_error(
                "load_artifacts tool requires an artifact service to be configured",
            ));
        };
        append_initial_instructions(artifacts.as_ref(), req).await?;
        process_load_artifacts_function_call(artifacts.as_ref(), req).await
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for ArtifactsTool {
    /// Echoes `{"artifact_names": [..]}`.
    async fn run(&self, _ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let Value::Object(m) = &args else {
            return Err(ToolError::InvalidArgument(format!(
                "unexpected args type, got: {}",
                go_type_name(&args)
            )));
        };
        let names = match m.get("artifact_names") {
            None => Vec::new(),
            // As in adk-python, arguments the model can correct are answered with a result, not a
            // tool error. A JSON null element would decode to "", which names no artifact, so it is
            // rejected as well.
            Some(raw) => match decode_string_slice(raw) {
                Ok(names) if !has_null_element(raw) => names,
                _ => {
                    let mut result = Map::new();
                    result.insert(
                        "error".into(),
                        Value::String("'artifact_names' must be a list of strings.".into()),
                    );
                    result.insert(
                        "error_code".into(),
                        Value::String("INVALID_ARGUMENTS".into()),
                    );
                    return Ok(result);
                }
            },
        };
        let mut result = Map::new();
        result.insert(
            "artifact_names".into(),
            Value::Array(names.into_iter().map(Value::String).collect()),
        );
        Ok(result)
    }
}

/// Whether `v` is a list that holds a JSON null.
fn has_null_element(v: &Value) -> bool {
    matches!(v, Value::Array(items) if items.iter().any(Value::is_null))
}

/// Decodes a JSON list of strings. `null` decodes to an empty list and a `null` element to `""`. A
/// type mismatch returns the "cannot unmarshal" error text for the first offending value.
fn decode_string_slice(raw: &Value) -> Result<Vec<String>, String> {
    match raw {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.clone()),
                Value::Null => Ok(String::new()),
                other => Err(unmarshal_type_error(other, "string")),
            })
            .collect(),
        other => Err(unmarshal_type_error(other, "[]string")),
    }
}

/// The "cannot unmarshal" error text for a JSON value of the wrong kind; `go_type` is the expected
/// type (`string` or `[]string`).
fn unmarshal_type_error(v: &Value, go_type: &str) -> String {
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    format!("json: cannot unmarshal {kind} into Go value of type {go_type}")
}

/// Appends the instruction listing the session's artifacts, when there are any.
async fn append_initial_instructions(
    artifacts: &dyn Artifacts,
    req: &mut LlmRequest,
) -> Result<(), ToolError> {
    let resp = artifacts
        .list()
        .await
        .map_err(|e| wrap_err("failed to list artifacts", e))?;
    if resp.file_names.is_empty() {
        return Ok(());
    }
    let names_json = serde_json::to_string(&resp.file_names)
        .map_err(|e| wrap_err("failed to marshal artifact names", e))?;
    let instructions = format!(
        "You have a list of artifacts:\n  {}\n\nWhen the user asks questions about any of the artifacts, you should call the `load_artifacts` function to load the artifact. Do not generate any text other than the function call. Whenever you are asked about artifacts, you should first load it. You must always load an artifact to access its content, even if it has been loaded before.",
        go_html_safe(&names_json)
    );
    append_instructions(req, &[&instructions]);
    Ok(())
}

/// HTML-safe JSON text: in serde_json's compact output `<`, `>`, `&`, U+2028 and U+2029 become
/// `\u003c`, `\u003e`, `\u0026`, `\u2028` and `\u2029`.
fn go_html_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// Loads the artifacts a `load_artifacts` function response names. Every part of the last content
/// is scanned (adk-python checks only `parts[0]`). The first `load_artifacts` response wins.
async fn process_load_artifacts_function_call(
    artifacts: &dyn Artifacts,
    req: &mut LlmRequest,
) -> Result<(), ToolError> {
    let Some(last) = req.contents.last() else {
        return Ok(());
    };
    let Some(fr) = last
        .parts
        .iter()
        .find_map(|p| p.function_response.as_ref().filter(|fr| fr.name == NAME))
    else {
        return Ok(());
    };
    let Some(raw) = fr.response.get("artifact_names") else {
        return Ok(());
    };
    let names = artifact_names_from_response(raw)?;
    if names.is_empty() {
        return Ok(());
    }
    // Concurrent loads, results kept in input order, first error aborts.
    let results = try_join_all(
        names
            .iter()
            .map(|name| load_individual_artifact(artifacts, name)),
    )
    .await?;
    req.contents.extend(results);
    Ok(())
}

/// The artifact names in a function response's `artifact_names` value, which must be a list of
/// strings.
fn artifact_names_from_response(raw: &Value) -> Result<Vec<String>, ToolError> {
    let Value::Array(items) = raw else {
        return Err(go_error(format!(
            "invalid artifact names type: {}, expected []string or []any",
            go_type_name(raw)
        )));
    };
    items
        .iter()
        .enumerate()
        .map(|(i, v)| match v {
            Value::String(s) => Ok(s.clone()),
            other => Err(go_error(format!(
                "invalid artifact name type at index {i}: {}, expected string",
                go_type_name(other)
            ))),
        })
        .collect()
}

/// Loads one artifact as a user content: a header part and the artifact's part. The load error is
/// wrapped, so the artifact service's not-found error stays in the source chain. A response without
/// a part appends only the header part.
async fn load_individual_artifact(
    artifacts: &dyn Artifacts,
    name: &str,
) -> Result<Content, ToolError> {
    let resp = artifacts
        .load(name)
        .await
        .map_err(|e| wrap_err(format!("failed to load artifact {name}"), e))?;
    let mut parts = vec![Part::from_text(format!("Artifact {name} is:"))];
    parts.extend(resp.part);
    Ok(Content {
        parts,
        role: ROLE_USER.to_string(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn declaration_is_gos_typed_schema() {
        let decl = ArtifactsTool.declaration().unwrap();
        assert_eq!(
            serde_json::to_value(&decl).unwrap(),
            json!({
                "name": "load_artifacts",
                "description": "Loads the artifacts and adds them to the session.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {"artifact_names": {"type": "ARRAY", "items": {"type": "STRING"}}}
                }
            })
        );
    }

    #[test]
    fn artifact_name_json_escapes_html_like_go() {
        let s =
            serde_json::to_string(&vec!["a<b>&c\u{2028}".to_string(), "d".to_string()]).unwrap();
        assert_eq!(go_html_safe(&s), r#"["a\u003cb\u003e\u0026c\u2028","d"]"#);
    }

    #[test]
    fn response_name_type_errors_use_go_type_names() {
        assert_eq!(
            artifact_names_from_response(&json!("x"))
                .unwrap_err()
                .to_string(),
            "invalid artifact names type: string, expected []string or []any"
        );
        assert_eq!(
            artifact_names_from_response(&json!(["a", 1]))
                .unwrap_err()
                .to_string(),
            "invalid artifact name type at index 1: float64, expected string"
        );
    }
}
