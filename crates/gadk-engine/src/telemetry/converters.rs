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

//! Attribute value conversions: [`AttrValue`] is the OTel attribute and log value model,
//! [`to_log_value`] converts a [`serde_json::Value`] into it and [`from_log_value`] back.
//!
//! Recording on a `tracing` span: a string is recorded as `&str`, a bool, INT64 and FLOAT64 as
//! their `tracing` primitives, and every structured value (the slices and maps) as the bytes of its
//! JSON text ([`AttrValue::record`]), so a subscriber reads a structured attribute from
//! `Visit::record_bytes` and nothing else arrives there. A JSON array of strings is a `StringSlice`
//! (`gen_ai.response.finish_reasons`); any other array is a `Slice`.

use serde_json::{Map, Number, Value};

/// An OTel attribute value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AttrValue {
    /// No value.
    Empty,
    /// A boolean.
    Bool(bool),
    /// A 64-bit integer.
    Int64(i64),
    /// A 64-bit float.
    Float64(f64),
    /// A string.
    String(String),
    /// A slice of booleans.
    #[cfg_attr(not(test), allow(dead_code))] // The typed slices: ADK records none.
    BoolSlice(Vec<bool>),
    /// A slice of integers.
    #[cfg_attr(not(test), allow(dead_code))] // The typed slices: ADK records none.
    Int64Slice(Vec<i64>),
    /// A slice of floats.
    #[cfg_attr(not(test), allow(dead_code))] // The typed slices: ADK records none.
    Float64Slice(Vec<f64>),
    /// A slice of strings.
    StringSlice(Vec<String>),
    /// A slice of bytes.
    #[cfg_attr(not(test), allow(dead_code))] // The typed slices: ADK records none.
    ByteSlice(Vec<u8>),
    /// A slice of mixed values.
    Slice(Vec<AttrValue>),
    /// A map, in insertion order.
    Map(Vec<(String, AttrValue)>),
}

impl AttrValue {
    /// A string value.
    pub(crate) fn string(s: impl Into<String>) -> Self {
        Self::String(s.into())
    }

    /// Records `self` as field `key` of `span` (see the module docs); an [`AttrValue::Empty`]
    /// records nothing, as the OTel SDK drops an attribute without a value.
    pub(crate) fn record(&self, span: &tracing::Span, key: &str) {
        match self {
            Self::Empty => {}
            Self::Bool(b) => {
                span.record(key, *b);
            }
            Self::Int64(i) => {
                span.record(key, *i);
            }
            Self::Float64(f) => {
                span.record(key, *f);
            }
            Self::String(s) => {
                span.record(key, s.as_str());
            }
            _ => {
                let text = from_log_value(self).to_string();
                span.record(key, text.as_bytes());
            }
        }
    }
}

/// The log value of a JSON value: `null`, string, bool, number, array or object. A number goes
/// through [`json_number`].
pub(crate) fn to_log_value(v: &Value) -> AttrValue {
    match v {
        Value::Null => AttrValue::Empty,
        Value::String(s) => AttrValue::String(s.clone()),
        Value::Bool(b) => AttrValue::Bool(*b),
        Value::Number(n) => json_number(&n.to_string()),
        Value::Array(items) => AttrValue::Slice(items.iter().map(to_log_value).collect()),
        Value::Object(m) => AttrValue::Map(
            m.iter()
                .map(|(k, v)| (k.clone(), to_log_value(v)))
                .collect(),
        ),
    }
}

/// The value of a JSON number's text: an integer if it parses as `i64`, else a float if it parses
/// as a finite `f64`, else (out of float64 range) the exact JSON text as a string.
pub(crate) fn json_number(text: &str) -> AttrValue {
    if let Ok(i) = text.parse::<i64>() {
        return AttrValue::Int64(i);
    }
    match text.parse::<f64>() {
        Ok(f) if f.is_finite() => AttrValue::Float64(f),
        _ => AttrValue::String(text.to_owned()),
    }
}

/// The JSON value of a log value. A float that JSON cannot hold (NaN, ±Inf) becomes `null`; a byte
/// slice is an array of its bytes.
pub(crate) fn from_log_value(v: &AttrValue) -> Value {
    fn float(f: f64) -> Value {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
    match v {
        AttrValue::Empty => Value::Null,
        AttrValue::Bool(b) => Value::Bool(*b),
        AttrValue::Int64(i) => Value::from(*i),
        AttrValue::Float64(f) => float(*f),
        AttrValue::String(s) => Value::String(s.clone()),
        AttrValue::BoolSlice(s) => s.iter().copied().map(Value::Bool).collect(),
        AttrValue::Int64Slice(s) => s.iter().copied().map(Value::from).collect(),
        AttrValue::Float64Slice(s) => s.iter().copied().map(float).collect(),
        AttrValue::StringSlice(s) => s.iter().cloned().map(Value::String).collect(),
        AttrValue::ByteSlice(s) => s.iter().copied().map(Value::from).collect(),
        AttrValue::Slice(s) => s.iter().map(from_log_value).collect(),
        AttrValue::Map(kvs) => Value::Object(
            kvs.iter()
                .map(|(k, v)| (k.clone(), from_log_value(v)))
                .collect::<Map<_, _>>(),
        ),
    }
}
