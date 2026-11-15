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

//! A gadk tool as an MCP tool definition (port of ADK-Go `tool/mcptoolset/conversion.go`:
//! `ToMCPTool`, `objectInputSchema`, `geminiToJSONSchema`, #2133).

use std::borrow::Cow;
use std::sync::Arc;

use gadk_agent::Tool;
use gadk_agent::go_quote;
use gadk_genai::{Schema, Type};
use serde_json::{Map, Value};

use crate::error::Error;

/// Go `ToMCPTool`: converts `tool` into an MCP tool definition for registration on an MCP
/// server (rmcp `ServerHandler::list_tools`). The caller supplies the handler that runs the
/// tool's logic when the server receives `tools/call`.
///
/// The input schema is the declaration's `parameters_json_schema` when present, else its
/// `parameters` converted to JSON Schema (an untyped top level is treated as `OBJECT`), else
/// `{"type": "object"}` (also for a tool without a declaration). MCP tool arguments are always
/// a JSON object, so a schema that leaves the top-level type out, or lists `"object"` among
/// several types, gets `"type": "object"`; a schema that is not a JSON object, or whose type
/// excludes `"object"`, is [`Error::ToMcpInputSchema`]. The tool's own declaration is never
/// modified. An empty description is left out.
pub fn to_mcp_tool(tool: &dyn Tool) -> Result<rmcp::model::Tool, Error> {
    // Go diverges here from adk-python's adk_to_mcp_tool_type, which returns {} when a tool has
    // no declaration or parameters: go-sdk's Server.AddTool panics unless InputSchema has type
    // "object".
    let mut input_schema = Value::Object(Map::from_iter([(
        "type".to_string(),
        Value::String("object".to_string()),
    )]));
    if let Some(decl) = tool.declaration() {
        if let Some(schema) = decl.parameters_json_schema {
            input_schema = schema;
        } else if let Some(mut params) = decl.parameters {
            if matches!(params.r#type, None | Some(Type::Unspecified)) {
                // MCP tool arguments are always an object. Treat an untyped top level as OBJECT
                // so its properties survive conversion, where gemini_to_json_schema would drop
                // them.
                params.r#type = Some(Type::Object);
            }
            let schema =
                gemini_to_json_schema(&params).map_err(|reason| Error::ToMcpParametersSchema {
                    name: tool.name().to_string(),
                    reason,
                })?;
            input_schema = Value::Object(schema);
        }
    }
    let input_schema =
        object_input_schema(input_schema).map_err(|reason| Error::ToMcpInputSchema {
            name: tool.name().to_string(),
            reason,
        })?;
    let description = tool.description();
    let description = (!description.is_empty()).then(|| Cow::Owned(description.to_string()));
    Ok(rmcp::model::Tool::new_with_raw(
        tool.name().to_string(),
        description,
        Arc::new(input_schema),
    ))
}

/// Go `objectInputSchema`: `schema` as a JSON object whose `"type"` is `"object"`. A schema that
/// omits the top-level type (a JSON `null`, Go's typed-nil schema, counts as `{}`), or lists
/// `"object"` among several types, gets `"type": "object"`; any other schema is an error with
/// Go's text.
fn object_input_schema(schema: Value) -> Result<Map<String, Value>, String> {
    let mut m = match schema {
        Value::Object(m) => m,
        // Go: `json.Unmarshal("null", &m)` leaves m nil, then `m = map[string]any{}`.
        Value::Null => Map::new(),
        other => {
            return Err(format!(
                "schema is not a JSON object: json: cannot unmarshal {} into Go value of type map[string]interface {{}}",
                json_kind(&other)
            ));
        }
    };
    match m.get("type") {
        None | Some(Value::Null) => {}
        Some(Value::String(typ)) => {
            if typ != "object" {
                return Err(format!("schema type {} is not \"object\"", go_quote(typ)));
            }
        }
        Some(Value::Array(types)) => {
            if !types.iter().any(|t| t.as_str() == Some("object")) {
                return Err(format!(
                    "schema types {} do not include \"object\"",
                    go_sprint(&Value::Array(types.clone()))
                ));
            }
        }
        Some(other) => {
            return Err(format!(
                "schema type {} is not \"object\"",
                go_sprint(other)
            ));
        }
    }
    m.insert("type".to_string(), Value::String("object".to_string()));
    Ok(m)
}

/// Go `geminiToJSONSchema`: a Gemini schema as a JSON Schema map (its JSON encoding, normalized
/// by [`normalize_gemini_schema_map`]). Go's nil-schema and unmarshal errors cannot occur here;
/// only the encoding can fail (`mcptoolset: marshal gemini schema: …`).
pub(crate) fn gemini_to_json_schema(schema: &Schema) -> Result<Map<String, Value>, String> {
    let raw = serde_json::to_value(schema)
        .map_err(|e| format!("mcptoolset: marshal gemini schema: {e}"))?;
    let Value::Object(mut raw) = raw else {
        return Err("mcptoolset: unmarshal gemini schema: not a JSON object".to_string());
    };
    normalize_gemini_schema_map(&mut raw);
    Ok(raw)
}

/// The upper-cased Gemini type of a schema map (`""` when absent or not a string).
fn gemini_type(raw: &Map<String, Value>) -> String {
    raw.get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase()
}

/// Whether a Gemini type string imposes no type constraint (unset or `TYPE_UNSPECIFIED`).
fn is_unspecified(typ: &str) -> bool {
    typ.is_empty() || typ == "TYPE_UNSPECIFIED"
}

/// Go `normalizeGeminiSchemaMap`: normalizes an encoded Gemini schema in place into JSON
/// Schema, recursively for its schema children. Like adk-python's gemini_to_json_schema, it
/// keeps the OpenAPI `nullable` keyword and leaves enum values as strings, so the result is
/// close to, but not strictly, standard JSON Schema.
fn normalize_gemini_schema_map(raw: &mut Map<String, Value>) {
    raw.remove("propertyOrdering");

    let typ = gemini_type(raw);

    // Omitted/unspecified types impose no JSON Schema type constraint. Vertex function
    // declarations also mark a union as OBJECT plus anyOf; that OBJECT is a placeholder, so the
    // anyOf branches carry the type.
    let is_union_placeholder = typ == "OBJECT"
        && raw
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|any_of| {
                any_of.iter().filter_map(Value::as_object).any(|sub| {
                    let sub_type = gemini_type(sub);
                    !is_unspecified(&sub_type) && sub_type != "OBJECT"
                })
            });

    if is_unspecified(&typ) || is_union_placeholder {
        raw.remove("type");
    } else {
        raw.insert("type".to_string(), Value::String(typ.to_lowercase()));
    }

    if raw.get("nullable") == Some(&Value::Bool(false)) {
        raw.remove("nullable");
    }

    if typ != "STRING" {
        raw.remove("pattern");
        raw.remove("minLength");
        raw.remove("maxLength");
    }

    if typ != "NUMBER" && typ != "INTEGER" {
        raw.remove("minimum");
        raw.remove("maximum");
    }

    if typ != "ARRAY" {
        raw.remove("items");
        raw.remove("minItems");
        raw.remove("maxItems");
    } else if let Some(Value::Object(items)) = raw.get_mut("items") {
        normalize_gemini_schema_map(items);
    }

    if typ != "OBJECT" {
        raw.remove("properties");
        raw.remove("required");
        raw.remove("minProperties");
        raw.remove("maxProperties");
    } else if let Some(Value::Object(props)) = raw.get_mut("properties") {
        for prop in props.values_mut() {
            if let Value::Object(prop) = prop {
                normalize_gemini_schema_map(prop);
            }
        }
    }

    if let Some(Value::Array(any_of)) = raw.get_mut("anyOf") {
        for sub in any_of {
            if let Value::Object(sub) = sub {
                normalize_gemini_schema_map(sub);
            }
        }
    }
}

/// The JSON kind Go's `UnmarshalTypeError` names for a value that is not an object.
fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Go `fmt.Sprint` (`%v`) of a decoded JSON value: strings bare, `null` as `<nil>`, arrays as
/// `[a b]`, objects as `map[k:v]` with sorted keys, numbers as Go prints a `float64`.
fn go_sprint(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
            Some(f) => f.to_string(),
            None => n.to_string(),
        },
        Value::String(s) => s.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(go_sprint).collect::<Vec<_>>().join(" ")
        ),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            format!(
                "map[{}]",
                keys.iter()
                    .map(|k| {
                        let value = m.get(k.as_str()).map(go_sprint).unwrap_or_default();
                        format!("{k}:{value}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    }
}
