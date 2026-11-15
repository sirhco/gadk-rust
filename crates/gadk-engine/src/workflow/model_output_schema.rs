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

//! Port of Go workflow/model_output_schema.go (be529e7b, #2194): the schema an agent node asks
//! the model to answer with, derived from the node's resolved output schema.

use gadk_genai::{Schema, Type};
use serde_json::{Map, Value};

/// Port of Go modelOutputSchema: the schema an agent node asks the model to answer with, derived
/// from the node's output schema, or `None` to leave the model unconstrained as it was before
/// typed nodes passed their schema on.
///
/// Only a closed, non-null object with at least one property qualifies. A string output sent as
/// a response schema makes the model answer with a quoted JSON string, and the set_model_response
/// tool needs object parameters, so primitive, array, map and nullable outputs stay
/// unconstrained. Every object must also set `additionalProperties` to `false`, as a Go struct
/// does: set_model_response rejects keys its schema does not list, so an open object would turn
/// an answer the node accepts into an error.
///
/// A nullable property (a `"null"` in its type list) keeps `nullable`: set_model_response accepts
/// a null argument only when its schema is nullable.
///
/// The result is a hint, not the contract: the node still validates the reply against the full
/// schema. So a schema that `genai.Schema` cannot carry yields `None` rather than an error, and
/// constructing a node never fails because of it.
pub(crate) fn model_output_schema(resolved: Option<&Value>) -> Option<Schema> {
    let root = resolved?.as_object()?;
    if type_list(root).is_some_and(|ts| ts.iter().any(|t| t == "null")) {
        return None;
    }
    let s = to_genai_schema(resolved?)?;
    (s.r#type == Some(Type::Object)).then_some(s)
}

/// The `"type"` keyword as a list of names when it is an array (Go `Schema.Types`).
fn type_list(s: &Map<String, Value>) -> Option<Vec<String>> {
    match s.get("type")? {
        Value::Array(ts) => Some(
            ts.iter()
                .map(|t| t.as_str().unwrap_or_default().to_string())
                .collect(),
        ),
        _ => None,
    }
}

/// Go genaiTypes.
fn genai_type(name: &str) -> Option<Type> {
    match name {
        "string" => Some(Type::String),
        "integer" => Some(Type::Integer),
        "number" => Some(Type::Number),
        "boolean" => Some(Type::Boolean),
        "array" => Some(Type::Array),
        "object" => Some(Type::Object),
        _ => None,
    }
}

/// Port of Go toGenaiSchema: converts the shape of `s` (types, properties, required fields, array
/// items, string enums, title and description). Value constraints (bounds, lengths, pattern) and
/// annotations (format, default, examples) are dropped. A `"null"` in a type list becomes
/// `nullable`. `None` for anything else: an untyped (`true` / `{}`) schema, a map or other open
/// object, a required name with no property, a composition keyword, a reference, or an enum that
/// is not on a string.
fn to_genai_schema(v: &Value) -> Option<Schema> {
    let s = v.as_object()?;
    if !only_known_keywords(s) {
        return None;
    }
    let (typ, nullable) = schema_type(s)?;
    let enum_values = match s.get("enum") {
        None => Vec::new(),
        Some(Value::Array(vals)) => vals.clone(),
        Some(_) => return None,
    };
    if !enum_values.is_empty() && typ != Type::String {
        return None;
    }
    let mut out = Schema {
        r#type: Some(typ.clone()),
        title: string_keyword(s, "title")?,
        description: string_keyword(s, "description")?,
        ..Schema::default()
    };
    if nullable {
        out.nullable = Some(true);
    }
    for e in &enum_values {
        out.r#enum.push(e.as_str()?.to_string());
    }
    match typ {
        Type::Object => {
            let props = match s.get("properties") {
                Some(Value::Object(p)) if !p.is_empty() => p,
                _ => return None,
            };
            if !s.get("additionalProperties").is_some_and(is_false_schema) {
                return None;
            }
            let required = match s.get("required") {
                None => Vec::new(),
                Some(Value::Array(r)) => r
                    .iter()
                    .map(|n| n.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?,
                Some(_) => return None,
            };
            if required.iter().any(|n| !props.contains_key(n)) {
                return None;
            }
            for (name, prop) in props {
                out.properties.insert(name.clone(), to_genai_schema(prop)?);
            }
            out.required = required;
        }
        Type::Array => {
            out.items = Some(Box::new(to_genai_schema(s.get("items")?)?));
        }
        _ => {}
    }
    Some(out)
}

/// A string keyword's value (`""` when absent); `None` when it is not a string.
fn string_keyword(s: &Map<String, Value>, key: &str) -> Option<String> {
    match s.get(key) {
        None => Some(String::new()),
        Some(Value::String(v)) => Some(v.clone()),
        Some(_) => None,
    }
}

/// Port of Go onlyKnownKeywords: whether `s` uses nothing beyond the keywords
/// [`to_genai_schema`] converts or deliberately drops, so an unknown keyword is rejected rather
/// than silently lost. `additionalProperties` is dropped only when it is the false schema; any
/// other value describes a map.
fn only_known_keywords(s: &Map<String, Value>) -> bool {
    const CONVERTED: &[&str] = &[
        "type",
        "enum",
        "title",
        "description",
        "properties",
        "required",
        "items",
    ];
    const DROPPED: &[&str] = &[
        "propertyOrder",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "format",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
        "default",
        "examples",
        "$comment",
        "$schema",
        "$id",
        "$anchor",
        "deprecated",
        "readOnly",
        "writeOnly",
    ];
    s.iter().all(|(k, v)| {
        CONVERTED.contains(&k.as_str())
            || DROPPED.contains(&k.as_str())
            || (k == "additionalProperties" && is_false_schema(v))
    })
}

/// Port of Go isFalseSchema: the schema that matches nothing (`false`, or jsonschema-go's
/// `{"not": {}}`).
fn is_false_schema(v: &Value) -> bool {
    match v {
        Value::Bool(b) => !b,
        Value::Object(o) => {
            o.len() == 1
                && o.get("not")
                    .is_some_and(|n| n.as_object().is_some_and(Map::is_empty))
        }
        _ => false,
    }
}

/// Port of Go schemaType: the genai type of `s` and whether it is nullable. A type list may pair
/// one type with `"null"`; no type, or more than one non-null type, has no genai equivalent.
fn schema_type(s: &Map<String, Value>) -> Option<(Type, bool)> {
    match s.get("type")? {
        Value::String(t) => Some((genai_type(t)?, false)),
        Value::Array(ts) => {
            let mut name: Option<&str> = None;
            let mut nullable = false;
            for t in ts {
                let t = t.as_str()?;
                if t == "null" {
                    nullable = true;
                    continue;
                }
                if name.is_some() {
                    return None;
                }
                name = Some(t);
            }
            Some((genai_type(name?)?, nullable))
        }
        _ => None,
    }
}
