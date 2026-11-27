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

//! The document's operations: tool names, parameters, request bodies and skipped operations.

use std::collections::{HashMap, HashSet};

use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::resolve;
use crate::spec::OpenApiSpec;

/// The operation keys of a path item, in the order OpenAPI lists them.
const METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// The longest tool name, in bytes.
pub(crate) const MAX_NAME_BYTES: usize = 64;

/// The longest tool description, in characters.
const MAX_DESCRIPTION_CHARS: usize = 1024;

/// An operation that does not become a tool, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkippedOperation {
    /// The HTTP method in upper case (`GET`), or `*` for every operation of a path item.
    pub method: String,
    /// The path template (`/pets/{petId}`).
    pub path: String,
    /// Why the operation was skipped (`cookie parameter session`).
    pub reason: String,
}

impl SkippedOperation {
    /// A skipped operation.
    pub fn new(
        method: impl Into<String>,
        path: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// Where a parameter goes in the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    Path,
    Query,
    Header,
}

impl Location {
    /// The OpenAPI `in` value.
    fn as_str(self) -> &'static str {
        match self {
            Location::Path => "path",
            Location::Query => "query",
            Location::Header => "header",
        }
    }
}

/// One path, query or header parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Param {
    /// The parameter's name in the request.
    pub(crate) name: String,
    pub(crate) location: Location,
    /// The declaration property that carries it.
    pub(crate) property: String,
    pub(crate) required: bool,
    /// The cleaned schema, with the parameter's description.
    pub(crate) schema: Value,
}

/// An `application/json` request body.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Body {
    pub(crate) schema: Value,
    pub(crate) required: bool,
}

/// One operation that becomes a tool.
#[derive(Clone, Debug)]
pub(crate) struct Operation {
    pub(crate) method: Method,
    /// The path template.
    pub(crate) path: String,
    /// The unprefixed tool name.
    pub(crate) name: String,
    pub(crate) description: String,
    /// Parameters in declaration order (path-item parameters, then the operation's).
    pub(crate) params: Vec<Param>,
    pub(crate) body: Option<Body>,
    /// Whether the method changes state (`post`, `put`, `patch`, `delete`).
    pub(crate) mutating: bool,
}

impl Operation {
    /// The declaration's `parameters_json_schema`. Its `properties` is a `serde_json::Map`, so
    /// they come out in sorted key order; `required` follows declaration order.
    pub(crate) fn parameters_schema(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for param in &self.params {
            properties.insert(param.property.clone(), param.schema.clone());
            if param.required {
                required.push(Value::String(param.property.clone()));
            }
        }
        if let Some(body) = &self.body {
            properties.insert("body".to_string(), body.schema.clone());
            if body.required {
                required.push(Value::String("body".to_string()));
            }
        }
        json!({"type": "object", "properties": properties, "required": required})
    }
}

/// The document's operations.
#[derive(Debug)]
pub(crate) struct Collected {
    /// The operations that become tools, in document order.
    pub(crate) operations: Vec<Operation>,
    pub(crate) skipped: Vec<SkippedOperation>,
}

/// Walks `paths` into operations and skipped operations. Two operations with the same tool
/// name, or one tool with two properties of the same name, are [`Error::DuplicateName`].
pub(crate) fn collect(spec: &OpenApiSpec) -> Result<Collected, Error> {
    let doc = spec.document();
    let mut operations = Vec::new();
    let mut skipped = Vec::new();
    let mut names = HashSet::new();
    for (path, keys) in spec.path_order() {
        let Some(item) = doc.get("paths").and_then(|paths| paths.get(path)) else {
            continue;
        };
        if item.get("$ref").is_some() {
            skipped.push(SkippedOperation::new(
                "*",
                path.as_str(),
                "path item $ref is not supported",
            ));
            continue;
        }
        for key in keys {
            if !METHODS.contains(&key.as_str()) {
                continue;
            }
            match operation(spec, path, key, item, &item[key.as_str()])? {
                Ok(op) => {
                    if !names.insert(op.name.clone()) {
                        return Err(Error::DuplicateName { name: op.name });
                    }
                    operations.push(op);
                }
                Err(skip) => skipped.push(skip),
            }
        }
    }
    Ok(Collected {
        operations,
        skipped,
    })
}

/// One operation, or why it is skipped.
fn operation(
    spec: &OpenApiSpec,
    path: &str,
    method: &str,
    item: &Value,
    op: &Value,
) -> Result<Result<Operation, SkippedOperation>, Error> {
    let doc = spec.document();
    let upper = method.to_ascii_uppercase();
    let at = format!("{upper} {path}");
    let skip = |reason: String| Ok(Err(SkippedOperation::new(upper.as_str(), path, reason)));

    // Path-item parameters, then the operation's; the operation's wins for the same name and
    // location, in the path-item parameter's place.
    let mut merged: Vec<&Value> = Vec::new();
    for raw in array(item.get("parameters")) {
        merged.push(resolve::object(doc, raw, &at)?);
    }
    for raw in array(op.get("parameters")) {
        let param = resolve::object(doc, raw, &at)?;
        let key = (text(param, "name"), text(param, "in"));
        match merged
            .iter()
            .position(|m| (text(m, "name"), text(m, "in")) == key)
        {
            Some(i) => merged[i] = param,
            None => merged.push(param),
        }
    }
    let mut located = Vec::with_capacity(merged.len());
    for param in merged {
        let location = match text(param, "in") {
            "path" => Location::Path,
            "query" => Location::Query,
            "header" => Location::Header,
            "cookie" => return skip(format!("cookie parameter {}", text(param, "name"))),
            other => {
                return skip(format!(
                    "parameter {} has unsupported location {other:?}",
                    text(param, "name")
                ));
            }
        };
        located.push((param, location));
    }

    let mut json_body = None;
    if let Some(raw) = op.get("requestBody") {
        let body = resolve::object(doc, raw, &at)?;
        // `content` is a `serde_json::Map`, so its media types come in sorted key order, not
        // document order: the body is the first JSON one in that order, and the skip reason
        // lists them in that order.
        let content = body.get("content").and_then(Value::as_object);
        match content.and_then(|c| c.iter().find(|(media, _)| is_json(media))) {
            Some((_, media)) => {
                json_body = Some((media, body.get("required") == Some(&Value::Bool(true))));
            }
            None => {
                let types = content
                    .map(|c| c.keys().cloned().collect::<Vec<_>>().join(", "))
                    .filter(|types| !types.is_empty())
                    .unwrap_or_else(|| "(none)".to_string());
                return skip(format!("request body content {types} is not JSON"));
            }
        }
    }

    let name = tool_name(op.get("operationId").and_then(Value::as_str), method, path);
    let openapi_3_0 = spec.is_3_0();
    let mut params = Vec::with_capacity(located.len());
    for (param, location) in located {
        let mut schema = match param.get("schema") {
            Some(schema) => resolve::schema(doc, schema, &at, openapi_3_0)?,
            None => json!({}),
        };
        if let (Some(description), Value::Object(schema)) = (
            param.get("description").and_then(Value::as_str),
            &mut schema,
        ) && !schema.contains_key("description")
        {
            schema.insert("description".to_string(), json!(description));
        }
        params.push(Param {
            name: text(param, "name").to_string(),
            location,
            property: property_name(text(param, "name")),
            required: location == Location::Path
                || param.get("required") == Some(&Value::Bool(true)),
            schema,
        });
    }
    disambiguate(&name, &mut params, json_body.is_some())?;

    let body = match json_body {
        Some((media, required)) => Some(Body {
            schema: match media.get("schema") {
                Some(schema) => resolve::schema(doc, schema, &at, openapi_3_0)?,
                None => json!({}),
            },
            required,
        }),
        None => None,
    };

    let method = Method::from_bytes(upper.as_bytes()).unwrap_or(Method::GET);
    let mutating = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    Ok(Ok(Operation {
        description: description(op, &upper, path),
        method,
        path: path.to_string(),
        name,
        params,
        body,
        mutating,
    }))
}

/// Renames colliding properties `<property>_<in>` (every parameter that shares a property name
/// with another, or is named `body`); a collision that remains is [`Error::DuplicateName`]
/// named `<tool name>.<property>`.
fn disambiguate(tool: &str, params: &mut [Param], has_body: bool) -> Result<(), Error> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for param in params.iter() {
        *counts.entry(param.property.clone()).or_default() += 1;
    }
    for param in params.iter_mut() {
        if counts[&param.property] > 1 || param.property == "body" {
            param.property = format!("{}_{}", param.property, param.location.as_str());
        }
    }
    let mut seen = HashSet::new();
    if has_body {
        seen.insert("body".to_string());
    }
    for param in params.iter() {
        if !seen.insert(param.property.clone()) {
            return Err(Error::DuplicateName {
                name: format!("{tool}.{}", param.property),
            });
        }
    }
    Ok(())
}

/// The tool description: `summary` and `description` separated by a blank line, whichever
/// exists, or `<METHOD> <path>`; at most 1,024 characters.
fn description(op: &Value, upper: &str, path: &str) -> String {
    let field = |key: &str| {
        op.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let full = match (field("summary"), field("description")) {
        (Some(summary), Some(description)) => format!("{summary}\n\n{description}"),
        (Some(one), None) | (None, Some(one)) => one.to_string(),
        (None, None) => format!("{upper} {path}"),
    };
    full.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

/// The tool name: the snake_case of `operation_id`, or of `<method>_<path>` when there is none
/// or it becomes empty; at most 64 bytes.
pub(crate) fn tool_name(operation_id: Option<&str>, method: &str, path: &str) -> String {
    let mut name = operation_id.map(snake_case).unwrap_or_default();
    if name.is_empty() {
        name = snake_case(&format!("{method}_{path}"));
    }
    if name.len() > MAX_NAME_BYTES {
        name.truncate(MAX_NAME_BYTES);
        name.truncate(name.trim_end_matches('_').len());
    }
    name
}

/// `_` before an upper-case letter that follows a lower-case letter or digit, or that follows
/// an upper-case letter and precedes a lower-case one; every character outside `[A-Za-z0-9_]`
/// becomes `_`; runs of `_` collapse, edges are trimmed, the result is lower case, and a
/// leading digit gets `op_`.
pub(crate) fn snake_case(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut spaced = String::with_capacity(input.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                spaced.push('_');
            }
        }
        if c.is_ascii_alphanumeric() || c == '_' {
            spaced.push(c.to_ascii_lowercase());
        } else {
            spaced.push('_');
        }
    }
    let collapsed = spaced
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if collapsed.starts_with(|c: char| c.is_ascii_digit()) {
        format!("op_{collapsed}")
    } else {
        collapsed
    }
}

/// The declaration property for a parameter: every character outside `[A-Za-z0-9_]` becomes
/// `_`.
fn property_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether a media type is JSON (`application/json` or `application/<x>+json`, parameters
/// ignored).
pub(crate) fn is_json(media_type: &str) -> bool {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "application/json"
        || (essence.starts_with("application/") && essence.ends_with("+json"))
}

/// A string field of a JSON object, or `""`.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The items of an optional JSON array.
fn array(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;
