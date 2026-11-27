// Copyright 2026 Chris Olson
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

//! Local `$ref` resolution and schema cleanup for the model.

use serde_json::{Map, Value, json};

use crate::error::Error;

/// How many `$ref`s are expanded along one path through a schema; a deeper `$ref` becomes `{}`.
pub(crate) const MAX_REF_DEPTH: usize = 8;

/// Keys removed from every schema object.
const DROPPED_KEYS: [&str; 5] = [
    "nullable",
    "discriminator",
    "xml",
    "example",
    "externalDocs",
];

/// Keys whose value maps names to schemas (the names are kept as they are).
const NAMED_SCHEMA_KEYS: [&str; 4] = ["properties", "patternProperties", "$defs", "definitions"];

/// Keys whose value is data, not a schema (copied without cleanup or `$ref` expansion).
const DATA_KEYS: [&str; 4] = ["enum", "const", "default", "examples"];

/// The value a local `$ref` (`#/…`, a JSON pointer with `~0` and `~1` escapes) points at.
/// `at` names the operation, for the error.
pub(crate) fn lookup<'a>(doc: &'a Value, reference: &str, at: &str) -> Result<&'a Value, Error> {
    let error = || Error::Ref {
        reference: reference.to_string(),
        at: at.to_string(),
    };
    let pointer = reference
        .strip_prefix('#')
        .filter(|pointer| pointer.starts_with('/'))
        .ok_or_else(error)?;
    doc.pointer(pointer).ok_or_else(error)
}

/// A parameter or request body object with its `$ref` chain followed (at most
/// [`MAX_REF_DEPTH`] links).
pub(crate) fn object<'a>(doc: &'a Value, value: &'a Value, at: &str) -> Result<&'a Value, Error> {
    let mut current = value;
    for _ in 0..=MAX_REF_DEPTH {
        match current.get("$ref").and_then(Value::as_str) {
            Some(reference) => current = lookup(doc, reference, at)?,
            None => return Ok(current),
        }
    }
    let reference = current
        .get("$ref")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(Error::Ref {
        reference: reference.to_string(),
        at: at.to_string(),
    })
}

/// `schema` with its `$ref`s expanded (up to [`MAX_REF_DEPTH`] nested levels; a deeper one
/// becomes `{}`) and cleaned for the model: [`DROPPED_KEYS`] are removed from every schema
/// object, and when `openapi_3_0` is set, `nullable: true` next to a string `type` turns `type`
/// into `[type, "null"]`.
pub(crate) fn schema(
    doc: &Value,
    schema: &Value,
    at: &str,
    openapi_3_0: bool,
) -> Result<Value, Error> {
    Expander {
        doc,
        at,
        openapi_3_0,
    }
    .schema(schema, 0)
}

struct Expander<'a> {
    doc: &'a Value,
    at: &'a str,
    openapi_3_0: bool,
}

impl Expander<'_> {
    /// One schema, `depth` `$ref`s below the root.
    fn schema(&self, value: &Value, depth: usize) -> Result<Value, Error> {
        let Value::Object(map) = value else {
            return Ok(value.clone());
        };
        if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
            if depth >= MAX_REF_DEPTH {
                return Ok(Value::Object(Map::new()));
            }
            let target = lookup(self.doc, reference, self.at)?;
            let mut expanded = self.schema(target, depth + 1)?;
            // Keys next to `$ref` apply on top of the target.
            if let Value::Object(out) = &mut expanded {
                for (key, member) in map {
                    if key == "$ref" || DROPPED_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    out.insert(key.clone(), self.member(key, member, depth)?);
                }
            }
            return Ok(expanded);
        }
        let mut out = Map::new();
        for (key, member) in map {
            if DROPPED_KEYS.contains(&key.as_str()) {
                continue;
            }
            out.insert(key.clone(), self.member(key, member, depth)?);
        }
        if self.openapi_3_0
            && map.get("nullable") == Some(&Value::Bool(true))
            && let Some(Value::String(ty)) = out.get("type")
        {
            let ty = json!([ty, "null"]);
            out.insert("type".to_string(), ty);
        }
        Ok(Value::Object(out))
    }

    /// The value of a schema object's `key`.
    fn member(&self, key: &str, member: &Value, depth: usize) -> Result<Value, Error> {
        if DATA_KEYS.contains(&key) {
            return Ok(member.clone());
        }
        match member {
            Value::Object(named) if NAMED_SCHEMA_KEYS.contains(&key) => {
                let mut out = Map::new();
                for (name, schema) in named {
                    out.insert(name.clone(), self.schema(schema, depth)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(items) => items
                .iter()
                .map(|item| self.schema(item, depth))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            _ => self.schema(member, depth),
        }
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
