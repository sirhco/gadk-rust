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

//! Port of Go internal/utils and internal/typeutil.

use gadk_agent::go_quote;
use std::sync::Arc;

use gadk_genai::{Content, FunctionCall, FunctionResponse, Schema};
use gadk_platform::Platform;
use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use serde_json::{Map, Value};

use crate::llm::processors::{as_go_floats, go_sprint};

/// Go `afFunctionCallIDPrefix`: the prefix of client-generated function call ids.
pub(crate) const AF_FUNCTION_CALL_ID_PREFIX: &str = "adk-";

// ---------------------------------------------------------------------------------------------
// internal/typeutil/convert.go
// ---------------------------------------------------------------------------------------------

/// Port of Go typeutil.schemaExpectsObject on the root of a JSON schema value:
/// `"type": "object"` or a `"type"` array containing `"object"`.
pub(crate) fn schema_expects_object(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "object"),
        _ => false,
    }
}

/// Port of Go typeutil.ConvertToWithJSONSchema: serialize `v`, validate the JSON form (a `null` against an
/// object schema becomes `{}`), then deserialize.
///
/// Rust mapping decision: Go unmarshals the original raw JSON; Rust deserializes the substituted value, so a
/// tool called with no arguments yields `Args` built from `{}` (Go: the zero struct) instead of a serde error.
pub(crate) fn convert_with_schema<In: serde::Serialize, Out: serde::de::DeserializeOwned>(
    v: &In,
    schema: Option<&CompiledSchema>,
) -> Result<Out, String> {
    let mut decoded = serde_json::to_value(v).map_err(|e| e.to_string())?;
    if let Some(s) = schema {
        if decoded.is_null() && schema_expects_object(s.schema()) {
            decoded = Value::Object(Map::new());
        }
        s.validate(&decoded)?;
    }
    serde_json::from_value(decoded).map_err(|e| e.to_string())
}

/// [`convert_with_schema`] for a function tool's call arguments: Go's functiontool decodes them
/// through `json.Marshal`, which writes an integral `float64` without a fraction, so a typed
/// decode into an integer field accepts it ([`go_marshaled_numbers`]).
pub(crate) fn convert_args_with_schema<Out: serde::de::DeserializeOwned>(
    args: &Value,
    schema: Option<&CompiledSchema>,
) -> Result<Out, String> {
    convert_with_schema(&go_marshaled_numbers(args.clone()), schema)
}

/// Go's `json.Marshal` → `json.Unmarshal` round trip of numbers: a float64 with an integral value
/// is written without a fraction (`1`, not `1.0`), so a typed decode into an integer field
/// accepts it. serde_json keeps a `1.0` float a float, which an `i64` field rejects; such floats
/// (as streamed partial arguments carry them) become integers here.
fn go_marshaled_numbers(v: Value) -> Value {
    /// Go's `float64` encoding switches to an exponent from 1e21 on.
    const GO_EXPONENT_FROM: f64 = 1e21;
    match v {
        Value::Number(n) if !n.is_i64() && !n.is_u64() => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < GO_EXPONENT_FROM => {
                if f >= i64::MIN as f64 && f < i64::MAX as f64 {
                    #[allow(clippy::cast_possible_truncation)] // integral and in range
                    Value::from(f as i64)
                } else if f >= 0.0 && f < u64::MAX as f64 {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    // integral, in range
                    Value::from(f as u64)
                } else {
                    Value::Number(n)
                }
            }
            _ => Value::Number(n),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(go_marshaled_numbers).collect()),
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (k, go_marshaled_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Port of Go typeutil.ValidateWithJSONSchema.
pub(crate) fn validate_with_schema<In: serde::Serialize>(
    v: &In,
    schema: Option<&CompiledSchema>,
) -> Result<(), String> {
    let Some(s) = schema else { return Ok(()) };
    let mut decoded = serde_json::to_value(v).map_err(|e| e.to_string())?;
    if decoded.is_null() && schema_expects_object(s.schema()) {
        decoded = Value::Object(Map::new());
    }
    s.validate(&decoded)
}

// ---------------------------------------------------------------------------------------------
// Schema inference and compilation (Go jsonschema.For[T] + Resolve)
// ---------------------------------------------------------------------------------------------

fn settings() -> SchemaSettings {
    SchemaSettings::draft2020_12()
        .with(|s| {
            s.meta_schema = None; // no "$schema" key (Go jsonschema.For emits none)
            s.inline_subschemas = true; // no "$defs"/"$ref" except for recursive types
        })
        .with_transform(RecursiveTransform(
            go_schema_shape as fn(&mut schemars::Schema),
        ))
}

/// Aligns one schemars subschema with the shape Go jsonschema-go (`jsonschema.For`) infers:
/// - struct schemas forbid unknown keys: an object schema (with `properties`, or a bare `"type": "object"`
///   from a field-less struct) and no `additionalProperties` gets `"additionalProperties": false`
///   (jsonschema-go infer.go, `reflect.Struct` case);
/// - integer/number schemas carry no `format` (schemars adds `"int64"`, `"double"`, …; Go emits none).
///
/// Applied to every subschema via [`RecursiveTransform`].
fn go_schema_shape(schema: &mut schemars::Schema) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let is_numeric = |t: &Value| t == "integer" || t == "number";
    let numeric = match obj.get("type") {
        Some(Value::Array(ts)) => ts.iter().any(is_numeric),
        Some(t) => is_numeric(t),
        None => false,
    };
    if numeric {
        obj.remove("format");
    }
    // A field-less struct (Go `struct{}`) is a bare `{"type": "object"}` in schemars; maps always carry
    // `additionalProperties`, so a bare object schema is a struct too.
    let is_struct =
        obj.contains_key("properties") || obj.get("type").is_some_and(|t| t == "object");
    if is_struct && !obj.contains_key("additionalProperties") {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    }
}

/// Port of Go jsonschema.For[T] as used by functiontool.resolvedSchema: draft 2020-12, no `$schema`,
/// inlined subschemas, and the root `title` (schemars' `T::schema_name()`) removed. Nested titles that
/// derive from doc comments are kept. Deserialize contract (describes what `T` accepts).
pub(crate) fn infer_schema<T: schemars::JsonSchema>() -> Value {
    let mut schema = settings().into_generator().into_root_schema_for::<T>();
    schema.remove("title");
    schema.to_value()
}

/// Same as [`infer_schema`] with the serialize contract (describes what `T` emits); used for tool outputs.
pub(crate) fn infer_output_schema<T: schemars::JsonSchema>() -> Value {
    let mut schema = settings()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    schema.remove("title");
    schema.to_value()
}

/// Port of Go jsonschema.Resolved: a schema compiled once, validated many times.
#[derive(Debug, Clone)]
pub(crate) struct CompiledSchema {
    schema: Value,
    // Arc: Clone is cheap and does not depend on `jsonschema::Validator: Clone`.
    validator: Arc<jsonschema::Validator>,
}

impl CompiledSchema {
    /// Port of Go Schema.Resolve(nil). `$schema` absent ⇒ draft 2020-12 (jsonschema's default).
    pub(crate) fn new(schema: Value) -> Result<Self, String> {
        let validator = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
        Ok(Self {
            schema,
            validator: Arc::new(validator),
        })
    }

    /// The schema this was compiled from (Go `Resolved.Schema`).
    pub(crate) fn schema(&self) -> &Value {
        &self.schema
    }

    /// Port of Go Resolved.Validate: the first error's text.
    pub(crate) fn validate(&self, v: &Value) -> Result<(), String> {
        self.validator.validate(v).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// internal/utils/schema_canonical.go
// ---------------------------------------------------------------------------------------------

/// Go `unorderedArrayKeywords`: JSON Schema keywords whose array value is a set.
const UNORDERED_ARRAY_KEYWORDS: [&str; 2] = ["required", "type"];

/// Port of Go utils.CanonicalSchemaJSON: the schema re-emitted with object keys sorted (recursively) and the
/// unordered set keywords (`required`, `type`) sorted, so equivalent schemas canonicalize identically.
pub(crate) fn canonical_schema_json(schema: &Value) -> String {
    canonicalize(schema)
}

/// Port of Go utils.canonicalize: serializes `v` with sorted object keys; arrays keep their order unless
/// they are the value of an unordered set keyword.
pub(crate) fn canonicalize(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&encode_string(k));
                out.push(':');
                match &map[k] {
                    Value::Array(items) if UNORDERED_ARRAY_KEYWORDS.contains(&k.as_str()) => {
                        out.push_str(&canonicalize_unordered_array(items));
                    }
                    other => out.push_str(&canonicalize(other)),
                }
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let encoded: Vec<String> = items.iter().map(canonicalize).collect();
            format!("[{}]", encoded.join(","))
        }
        Value::Number(n) => encode_number(n),
        Value::String(s) => encode_string(s),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
    }
}

/// Port of Go utils.canonicalizeUnorderedArray: canonicalized elements sorted by their encoded bytes.
fn canonicalize_unordered_array(items: &[Value]) -> String {
    let mut encoded: Vec<String> = items.iter().map(canonicalize).collect();
    encoded.sort();
    format!("[{}]", encoded.join(","))
}

fn encode_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// Go `json.Marshal(float64)` for the integral values a decoded `any` holds: `123.0` → `123`.
fn encode_number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    match n.as_f64() {
        Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{f:.0}"),
        _ => n.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// internal/utils/schema_utils.go
// ---------------------------------------------------------------------------------------------

pub(crate) use gadk_platform::go_type_name;

fn type_str(schema: &Schema) -> &str {
    schema.r#type.as_ref().map(|t| t.as_str()).unwrap_or("")
}

/// Port of Go utils.matchType. Go returns (false, err) on an object error; Rust returns Err.
pub(crate) fn match_type(
    value: &Value,
    schema: Option<&Schema>,
    is_input: bool,
) -> Result<bool, String> {
    let Some(schema) = schema else {
        return Err("schema is nil".to_string());
    };
    if value.is_null() {
        return Ok(schema.nullable == Some(true));
    }
    // Convert type to upper case to match the type in the schema.
    match type_str(schema).to_uppercase().as_str() {
        "STRING" => Ok(value.is_string()),
        "INTEGER" => Ok(value.as_f64().is_some_and(|f| f == f.trunc())),
        "BOOLEAN" => Ok(value.is_boolean()),
        "NUMBER" => Ok(value.is_number()),
        "ARRAY" => {
            let Value::Array(items) = value else {
                return Ok(false);
            };
            let Some(item_schema) = schema.items.as_deref() else {
                return Err("array schema missing items definition".to_string());
            };
            for (i, item) in items.iter().enumerate() {
                match match_type(item, Some(item_schema), is_input) {
                    Err(e) => return Err(format!("array item {i}: {e}")),
                    Ok(false) => return Ok(false),
                    Ok(true) => {}
                }
            }
            Ok(true)
        }
        "OBJECT" => {
            let Value::Object(obj) = value else {
                return Ok(false);
            };
            validate_map_on_schema(obj, schema, is_input).map(|()| true)
        }
        _ => Err(format!("unsupported type: {}", type_str(schema))),
    }
}

/// Port of Go utils.ValidateMapOnSchema. The schema is a reference, so Go's nil-schema error is unreachable.
pub(crate) fn validate_map_on_schema(
    args: &Map<String, Value>,
    schema: &Schema,
    is_input: bool,
) -> Result<(), String> {
    let arg_type = if is_input { "input" } else { "output" };
    for (key, value) in args {
        let Some(prop_schema) = schema.properties.get(key) else {
            // Note: OpenAPI schemas can allow additional properties. This implementation assumes strictness.
            return Err(format!(
                "{arg_type} arg: '{}' does not exist in schema properties",
                go_quote(key)
            ));
        };
        match match_type(value, Some(prop_schema), is_input) {
            Err(e) => {
                return Err(format!(
                    "{arg_type} arg: '{}' validation failed: {e}",
                    go_quote(key)
                ));
            }
            Ok(false) => {
                return Err(format!(
                    "{arg_type} arg: '{}' type mismatch, expected schema type {}, got value {} of type {}",
                    go_quote(key),
                    type_str(prop_schema),
                    go_sprint(&as_go_floats(value)),
                    go_type_name(value)
                ));
            }
            Ok(true) => {}
        }
    }
    for required_key in &schema.required {
        if !args.contains_key(required_key) {
            return Err(format!(
                "{} args does not contain required key: '{}'",
                go_quote(arg_type),
                go_quote(required_key)
            ));
        }
    }
    Ok(())
}

/// Port of Go utils.ValidateOutputSchema. The schema is a reference, so Go's nil-schema error is unreachable.
pub(crate) fn validate_output_schema(
    output: &str,
    schema: &Schema,
) -> Result<Map<String, Value>, String> {
    let parsed: Value =
        serde_json::from_str(output).map_err(|e| format!("failed to parse output JSON: {e}"))?;
    let map = match parsed {
        Value::Object(m) => m,
        // Go unmarshals `null` into a nil map.
        Value::Null => Map::new(),
        other => {
            let kind = match other {
                Value::Array(_) => "array",
                Value::String(_) => "string",
                Value::Number(_) => "number",
                _ => "bool",
            };
            return Err(format!(
                "failed to parse output JSON: json: cannot unmarshal {kind} into Go value of type map[string]interface {{}}"
            ));
        }
    };
    validate_map_on_schema(&map, schema, false)?;
    Ok(map)
}

// ---------------------------------------------------------------------------------------------
// internal/utils/utils.go
// ---------------------------------------------------------------------------------------------

/// Port of Go utils.PopulateClientFunctionCallID: assigns a client id to every function call without one.
pub(crate) fn populate_client_function_call_id(platform: &Platform, c: Option<&mut Content>) {
    let Some(c) = c else { return };
    for fc in c.parts.iter_mut().filter_map(|p| p.function_call.as_mut()) {
        if fc.id.is_empty() {
            fc.id = generate_function_call_id(platform);
        }
    }
}

/// Port of Go utils.GenerateFunctionCallID.
pub(crate) fn generate_function_call_id(platform: &Platform) -> String {
    format!("{AF_FUNCTION_CALL_ID_PREFIX}{}", platform.new_uuid())
}

/// Port of Go utils.RemoveClientFunctionCallID: clears client-generated ids on function calls and responses.
pub(crate) fn remove_client_function_call_id(c: &mut Content) {
    for p in &mut c.parts {
        if let Some(fc) = p.function_call.as_mut()
            && fc.id.starts_with(AF_FUNCTION_CALL_ID_PREFIX)
        {
            fc.id.clear();
        }
        if let Some(fr) = p.function_response.as_mut()
            && fr.id.starts_with(AF_FUNCTION_CALL_ID_PREFIX)
        {
            fr.id.clear();
        }
    }
}

/// Port of Go utils.Content (shared with `gadk_session::compaction`).
pub(crate) use gadk_session::compaction::content;

/// Port of Go `utils.FunctionCalls`: the function calls of `c`, in order (none for `None`).
pub fn function_calls(c: Option<&Content>) -> Vec<&FunctionCall> {
    c.map(|c| {
        c.parts
            .iter()
            .filter_map(|p| p.function_call.as_ref())
            .collect()
    })
    .unwrap_or_default()
}

/// Port of Go utils.FunctionResponses.
pub(crate) fn function_responses(c: Option<&Content>) -> Vec<&FunctionResponse> {
    c.map(|c| {
        c.parts
            .iter()
            .filter_map(|p| p.function_response.as_ref())
            .collect()
    })
    .unwrap_or_default()
}

/// Port of Go utils.TextParts: the non-empty texts, in part order.
pub(crate) fn text_parts(c: Option<&Content>) -> Vec<String> {
    c.map(|c| {
        c.parts
            .iter()
            .filter(|p| !p.text.is_empty())
            .map(|p| p.text.clone())
            .collect()
    })
    .unwrap_or_default()
}

/// The content's text parts concatenated without a separator (no Go counterpart; plan D5).
#[allow(dead_code)] // Pinned by plan D5; no Go counterpart and no caller in the 1b port.
pub(crate) fn content_text(c: &Content) -> String {
    text_parts(Some(c)).concat()
}

/// Port of Go utils.FunctionDecls.
#[cfg(test)]
pub(crate) fn function_decls(
    c: Option<&gadk_genai::GenerateContentConfig>,
) -> Vec<&gadk_genai::FunctionDeclaration> {
    c.map(|c| {
        c.tools
            .iter()
            .flat_map(|t| t.function_declarations.iter())
            .collect()
    })
    .unwrap_or_default()
}

/// Port of Go utils.EventBelongsToBranch: an event belongs to its own branch and every descendant of it.
pub(crate) fn event_belongs_to_branch(invocation_branch: &str, event_branch: &str) -> bool {
    invocation_branch.is_empty()
        || event_branch.is_empty()
        || event_branch == invocation_branch
        || invocation_branch.starts_with(&format!("{event_branch}."))
}

/// The text of a `recover()`ed panic payload (Go `%v`): `&str`/`String` payloads verbatim.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_string()
    }
}

/// Go `toolinternal.ModelErrorText` (e83ab5b0): the text a failed call of the tool named
/// `tool_name` may show the model, and whether `err` turned out to be a recovered panic. A
/// [`ToolError::Panic`](gadk_agent::ToolError::Panic) anywhere in the error chain (Go
/// `errors.AsType[*PanicError]`) gives the generic ``Invoking `<tool>()` failed with an internal
/// error.``, because the panic's own text carries the panic value; any other error gives its
/// own text.
pub(crate) fn model_error_text(tool_name: &str, err: &gadk_agent::ToolError) -> (String, bool) {
    if chain_has_panic(err) {
        return (
            format!("Invoking `{tool_name}()` failed with an internal error."),
            true,
        );
    }
    (err.to_string(), false)
}

/// Whether a [`ToolError::Panic`](gadk_agent::ToolError::Panic) is anywhere in `err`'s chain.
/// Transparent layers (`ToolError::Execution`, `SharedToolError`, `AgentError::Other` /
/// `AgentError::Workflow`) forward `source()` past their inner error, so the walk descends into
/// them directly.
fn chain_has_panic(err: &(dyn std::error::Error + 'static)) -> bool {
    use gadk_agent::{AgentError, SharedToolError, ToolError};
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        cur = if let Some(te) = e.downcast_ref::<ToolError>() {
            match te {
                ToolError::Panic(_) => return true,
                ToolError::Execution(inner) => Some(&**inner),
                _ => std::error::Error::source(te),
            }
        } else if let Some(shared) = e.downcast_ref::<SharedToolError>() {
            Some(&*shared.0)
        } else if let Some(AgentError::Other(inner) | AgentError::Workflow(inner)) =
            e.downcast_ref::<AgentError>()
        {
            Some(&**inner)
        } else {
            e.source()
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gadk_genai::Type;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    /// Go schema_utils.go:111 formats the value with `%v` of the JSON-decoded value: strings
    /// unquoted, objects as `map[...]`, numbers as float64.
    #[test]
    fn type_mismatch_formats_value_like_go_percent_v() {
        let schema = Schema {
            r#type: Some(Type::Object),
            properties: BTreeMap::from([(
                "answer".to_owned(),
                Schema {
                    r#type: Some(Type::Integer),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let check = |args: Value| {
            let Value::Object(m) = args else {
                unreachable!()
            };
            validate_map_on_schema(&m, &schema, false).unwrap_err()
        };
        assert_eq!(
            check(json!({"answer": "42"})),
            "output arg: '\"answer\"' type mismatch, expected schema type INTEGER, got value 42 of type string"
        );
        assert_eq!(
            check(json!({"answer": {"a": 1, "b": 1_000_000}})),
            "output arg: '\"answer\"' type mismatch, expected schema type INTEGER, got value map[a:1 b:1e+06] of type map[string]interface {}"
        );
    }
}
