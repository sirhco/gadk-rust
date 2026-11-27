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

//! Turns a call's arguments into the request's URL, headers and body.

use gadk_agent::ToolError;
use serde_json::{Map, Value};
use url::Url;

use crate::operation::{Location, Operation};

/// The parts of one request taken from the arguments.
#[derive(Debug, PartialEq)]
pub(crate) struct Parts {
    /// The full URL: base, substituted path and query.
    pub(crate) url: Url,
    /// Header parameters, in declaration order.
    pub(crate) headers: Vec<(String, String)>,
    /// The JSON body, when the operation has one and `body` is given.
    pub(crate) body: Option<Value>,
}

/// Builds the request parts for `op` from `args`. `base` is the base URL without a trailing
/// `/`; `query_api_key` is appended after the query parameters. An argument problem is
/// [`ToolError::InvalidArgument`] naming the argument.
pub(crate) fn build(
    op: &Operation,
    base: &str,
    query_api_key: Option<&(String, String)>,
    args: &Value,
) -> Result<Parts, ToolError> {
    let Value::Object(args) = args else {
        return Err(invalid("the arguments must be a JSON object".to_string()));
    };
    for param in op.params.iter().filter(|p| p.required) {
        if arg(args, &param.property).is_none() {
            return Err(invalid(format!(
                "missing required argument {}",
                param.property
            )));
        }
    }
    if op.body.as_ref().is_some_and(|body| body.required) && arg(args, "body").is_none() {
        return Err(invalid("missing required argument body".to_string()));
    }

    let mut path = op.path.clone();
    let mut query = Vec::new();
    let mut headers = Vec::new();
    for param in &op.params {
        let Some(value) = arg(args, &param.property) else {
            continue;
        };
        match param.location {
            Location::Path => {
                let text = scalar(value).ok_or_else(|| not_scalar(&param.property))?;
                path = path.replace(&format!("{{{}}}", param.name), &encode_segment(&text));
            }
            Location::Header => {
                let text = scalar(value).ok_or_else(|| not_scalar(&param.property))?;
                headers.push((param.name.clone(), text));
            }
            Location::Query => match value {
                Value::Array(items) => {
                    for item in items.iter().filter(|item| !item.is_null()) {
                        let text = scalar(item).ok_or_else(|| not_query(&param.property))?;
                        query.push((param.name.clone(), text));
                    }
                }
                _ => {
                    let text = scalar(value).ok_or_else(|| not_query(&param.property))?;
                    query.push((param.name.clone(), text));
                }
            },
        }
    }
    if let Some((name, value)) = query_api_key {
        query.push((name.clone(), value.clone()));
    }

    let mut url = Url::parse(&format!("{base}{path}"))
        .map_err(|err| invalid(format!("cannot build the request URL {base}{path}: {err}")))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in &query {
            pairs.append_pair(name, value);
        }
    }
    let body = match &op.body {
        Some(_) => arg(args, "body").cloned(),
        None => None,
    };
    Ok(Parts { url, headers, body })
}

/// An argument that is present and not `null`.
fn arg<'a>(args: &'a Map<String, Value>, property: &str) -> Option<&'a Value> {
    args.get(property).filter(|value| !value.is_null())
}

/// A string as is, a number or boolean as JSON; `None` for an object or array.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// Percent-encodes everything but the unreserved characters (`A-Z a-z 0-9 - . _ ~`), so the
/// value stays one path segment.
fn encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn invalid(message: String) -> ToolError {
    ToolError::InvalidArgument(message)
}

fn not_scalar(property: &str) -> ToolError {
    invalid(format!(
        "argument {property} must be a string, number or boolean"
    ))
}

fn not_query(property: &str) -> ToolError {
    invalid(format!(
        "argument {property} must be a string, number, boolean or an array of them"
    ))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
