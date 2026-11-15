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

//! The openai-go v3.91.1 `shared` package types both APIs send (ADK-Go imports it as
//! `oaishared`), and the serde conventions every wire type in this crate follows.
//!
//! Request (param) types serialize their fields in openai-go's struct declaration order. Go's
//! `param.Opt[T]` is an `Option<T>` skipped when `None`; a plain `omitzero` string or slice is
//! skipped when empty; a `constant.*` field (`type`, `role`) is a zero-sized [`tag!`] type that
//! always serializes its one value; a param union (`OfX` fields) is an untagged enum that
//! serializes as the variant's own JSON. Fields ADK-Go never sets are not modelled: they are
//! `omitzero` in openai-go, so they never reach the wire. Bodies are written by
//! [`crate::client::encode_body`] with [`gadk_model::to_go_json`] (Go's float format and HTML
//! escaping; `serde_json::Map` keys sorted, as Go sorts `map[string]any` keys).
//!
//! Response and stream types decode with openai-go `apijson`'s leniency, through the helpers
//! of [`de`]: a missing, `null` or wrongly typed field is its zero value, and a field ADK-Go
//! checks with `JSON.X.Valid()` is an `Option` that is `Some` only when the value was present,
//! non-null and of the right shape. Decode them only from text (`serde_json::from_str` /
//! `from_slice`): the helpers read each field as a [`RawValue`](serde_json::value::RawValue).

use serde::Serialize;
use serde_json::{Map, Value};

/// Declares a zero-sized type that serializes as one constant string: openai-go's
/// `constant.*` fields (`"type"`, `"role"`), which are always sent.
macro_rules! tag {
    ($(#[$m:meta])* $name:ident = $value:literal) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub(crate) struct $name;

        impl $name {
            /// The constant's value.
            #[allow(dead_code)] // Not every tag's value is read outside its `Serialize`.
            pub(crate) const VALUE: &'static str = $value;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($value)
            }
        }
    };
}
pub(crate) use tag;

/// openai-go `shared.ReasoningEffort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go never sends `xhigh` or `max`.
pub(crate) enum ReasoningEffort {
    /// `"none"`.
    None,
    /// `"minimal"`.
    Minimal,
    /// `"low"`.
    Low,
    /// `"medium"`.
    Medium,
    /// `"high"`.
    High,
    /// `"xhigh"`.
    Xhigh,
    /// `"max"`.
    Max,
}

/// openai-go `shared.ReasoningSummary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // D5: openai-go's whole value set; ADK-Go sends only `auto`.
pub(crate) enum ReasoningSummary {
    /// `"auto"`.
    Auto,
    /// `"concise"`.
    Concise,
    /// `"detailed"`.
    Detailed,
}

/// openai-go `shared.ReasoningParam`, the fields ADK-Go sets, in declaration order: `effort,
/// summary` (`context`, `generate_summary` and `mode` are never set). Omitted from a request
/// when both are `None` ([`is_zero`](Self::is_zero), Go `omitzero`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ReasoningParam {
    /// `effort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<ReasoningEffort>,
    /// `summary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<ReasoningSummary>,
}

impl ReasoningParam {
    /// Go `omitzero`: no field set.
    pub(crate) fn is_zero(&self) -> bool {
        self.effort.is_none() && self.summary.is_none()
    }
}

/// openai-go `shared.FunctionDefinitionParam`: `name, strict, description, parameters`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct FunctionDefinitionParam {
    /// `name` (required, always sent).
    pub(crate) name: String,
    /// `strict`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict: Option<bool>,
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// `parameters` (Go `FunctionParameters`, a `map[string]any`; a nil map is omitted, an
    /// empty one is sent as `{}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<Map<String, Value>>,
}

tag!(
    /// `"json_object"`.
    JsonObjectTag = "json_object"
);
tag!(
    /// `"json_schema"`.
    JsonSchemaTag = "json_schema"
);
tag!(
    /// `"text"`.
    TextTag = "text"
);

/// openai-go `shared.ResponseFormatJSONObjectParam`: `type`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ResponseFormatJsonObjectParam {
    /// `type`: `"json_object"`.
    #[serde(rename = "type")]
    pub(crate) kind: JsonObjectTag,
}

/// openai-go `shared.ResponseFormatTextParam`: `type`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ResponseFormatTextParam {
    /// `type`: `"text"`.
    #[serde(rename = "type")]
    pub(crate) kind: TextTag,
}

/// openai-go `shared.ResponseFormatJSONSchemaParam`: `json_schema, type` (Chat Completions'
/// `response_format`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseFormatJsonSchemaParam {
    /// `json_schema`.
    pub(crate) json_schema: ResponseFormatJsonSchemaJsonSchemaParam,
    /// `type`: `"json_schema"`.
    #[serde(rename = "type")]
    pub(crate) kind: JsonSchemaTag,
}

/// openai-go `shared.ResponseFormatJSONSchemaJSONSchemaParam`: `name, strict, description,
/// schema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ResponseFormatJsonSchemaJsonSchemaParam {
    /// `name` (required, always sent).
    pub(crate) name: String,
    /// `strict`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) strict: Option<bool>,
    /// `description`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// `schema` (Go `any`, a `map[string]any`; nil is omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schema: Option<Map<String, Value>>,
}

/// openai-go `apijson` decoding, field by field.
///
/// Every helper reads the field's raw JSON text first (so a number like `1e400` that does not
/// fit an `f64` cannot fail the whole document), then converts it the way `apijson` does:
/// - [`lenient`]: structs, slices and `Option`s. A value of the wrong shape is the zero value
///   (`None` for an `Option`); a slice with one element of the wrong shape is empty, as Go's.
/// - [`go_string`]: a string; a number, boolean, object or array is its raw JSON text (Go's
///   `apijson` copies the raw value into a string field); `null` is `""`.
/// - [`go_i64`]: gjson `Result.Int()`, which `apijson` stores whatever its error: `true` is 1;
///   a string goes through gjson's `parseInt` (optional `-`, ASCII digits only, nothing
///   trimmed, wrapping on overflow as Go's `int64` does), anything else in it is 0; a number
///   within ±(2^53 − 1) truncates toward zero, a larger one is its raw text through `parseInt`
///   when that is all digits, else the float converted with Rust's saturating `as` (Go's
///   `int64(f)` is implementation-defined there: arm64 saturates likewise, amd64 gives
///   `i64::MIN`); `false`, `null`, objects and arrays are 0.
/// - [`go_f64`]: gjson `Result.Float()`: `true` is 1; a string or number is Go
///   `strconv.ParseFloat` ([`go_parse_float`](gadk_model::rest::go_parse_float): nothing trimmed, hexadecimal and underscores
///   accepted, the value kept on a range error); anything else is 0. One divergence: a
///   non-finite result (`1e400`, `"inf"`, `"nan"`) is 0 where Go keeps ±Inf/NaN, because it
///   could not be persisted (serde writes `null`; Go's `json.Marshal` fails).
pub(crate) mod de {
    use gadk_model::rest::{GoParseFloatError, go_parse_float};
    use serde::Deserialize;
    use serde::de::{DeserializeOwned, Deserializer};
    use serde_json::value::RawValue;

    fn raw<'de, D: Deserializer<'de>>(d: D) -> Result<Box<RawValue>, D::Error> {
        Box::<RawValue>::deserialize(d)
    }

    /// Structs, slices and `Option`s: the wrong shape is the zero value.
    pub(crate) fn lenient<'de, D, T>(d: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: DeserializeOwned + Default,
    {
        let raw = raw(d)?;
        Ok(serde_json::from_str(raw.get()).unwrap_or_default())
    }

    /// A string field.
    pub(crate) fn go_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let raw = raw(d)?;
        let text = raw.get();
        if text.starts_with('"') {
            return Ok(serde_json::from_str(text).unwrap_or_default());
        }
        if text == "null" {
            return Ok(String::new());
        }
        Ok(text.to_string())
    }

    /// A raw JSON value as the gjson `Result` types `Int()`/`Float()` tell apart.
    enum Gjson<'a> {
        /// `String`, unescaped (`Result.Str`).
        Str(String),
        /// `Number`, its raw text (`Result.Raw`).
        Num(&'a str),
        /// `True`.
        True,
        /// `False`, `Null` and `JSON` (an object or array): `Int()`/`Float()` are 0.
        Other,
    }

    fn gjson(text: &str) -> Gjson<'_> {
        match text.as_bytes().first() {
            Some(b'"') => Gjson::Str(serde_json::from_str(text).unwrap_or_default()),
            Some(b't') => Gjson::True,
            Some(b'-' | b'0'..=b'9') => Gjson::Num(text),
            _ => Gjson::Other,
        }
    }

    /// Go `strconv.ParseFloat(s, 64)`'s value whatever its error, as gjson keeps it (`Num`,
    /// and `Float()` of a string): ±Inf on a range error, 0 on a syntax error.
    fn parse_float_value(s: &str) -> f64 {
        match go_parse_float(s) {
            Ok(v) | Err(GoParseFloatError::Range { value: v }) => v,
            Err(GoParseFloatError::Syntax) => 0.0,
        }
    }

    /// gjson `parseInt`: an optional `-` and at least one ASCII digit, nothing else; the
    /// arithmetic wraps as Go's `int64` does.
    fn gjson_parse_int(s: &str) -> Option<i64> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        if digits.is_empty() {
            return None;
        }
        let mut n: i64 = 0;
        for c in digits.bytes() {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n.wrapping_mul(10).wrapping_add(i64::from(c - b'0'));
        }
        Some(if neg { n.wrapping_neg() } else { n })
    }

    /// gjson `safeInt`'s bound: JavaScript's `Number.MAX_SAFE_INTEGER`.
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

    /// An integer field (gjson `Result.Int()`).
    pub(crate) fn go_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        let raw = raw(d)?;
        Ok(match gjson(raw.get()) {
            Gjson::True => 1,
            Gjson::Str(s) => gjson_parse_int(&s).unwrap_or(0),
            Gjson::Num(text) => {
                let f = parse_float_value(text);
                if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&f) {
                    f as i64
                } else if let Some(i) = gjson_parse_int(text) {
                    i
                } else {
                    // `as` saturates (Go arm64's `int64(f)`; see the module docs).
                    f as i64
                }
            }
            Gjson::Other => 0,
        })
    }

    /// A float field (gjson `Result.Float()`); a non-finite value is 0 (see the module docs).
    pub(crate) fn go_f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let raw = raw(d)?;
        let f = match gjson(raw.get()) {
            Gjson::True => 1.0,
            Gjson::Str(s) => parse_float_value(&s),
            Gjson::Num(text) => parse_float_value(text),
            Gjson::Other => 0.0,
        };
        Ok(if f.is_finite() { f } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use gadk_model::to_go_json;
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde_json::json;

    fn go(v: &impl Serialize) -> String {
        String::from_utf8(to_go_json(v).unwrap()).unwrap()
    }

    #[test]
    fn shared_params_serialize_in_openai_go_order() {
        assert_eq!(go(&ReasoningParam::default()), "{}");
        assert!(ReasoningParam::default().is_zero());
        assert_eq!(
            go(&ReasoningParam {
                effort: Some(ReasoningEffort::Xhigh),
                summary: Some(ReasoningSummary::Auto),
            }),
            r#"{"effort":"xhigh","summary":"auto"}"#
        );
        for (e, want) in [
            (ReasoningEffort::None, "none"),
            (ReasoningEffort::Minimal, "minimal"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Xhigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
        ] {
            assert_eq!(go(&e), format!("\"{want}\""));
        }
        let params = json!({"type": "object", "properties": {}, "b": 1, "a": 2});
        let Value::Object(params) = params else {
            panic!("object")
        };
        assert_eq!(
            go(&FunctionDefinitionParam {
                name: "f".into(),
                strict: Some(false),
                description: Some("d".into()),
                parameters: Some(params.clone()),
            }),
            r#"{"name":"f","strict":false,"description":"d","parameters":{"a":2,"b":1,"properties":{},"type":"object"}}"#
        );
        assert_eq!(
            go(&FunctionDefinitionParam {
                name: "f".into(),
                parameters: Some(Map::new()),
                ..Default::default()
            }),
            r#"{"name":"f","parameters":{}}"#
        );
        assert_eq!(
            go(&ResponseFormatJsonObjectParam::default()),
            r#"{"type":"json_object"}"#
        );
        assert_eq!(
            go(&ResponseFormatTextParam::default()),
            r#"{"type":"text"}"#
        );
        assert_eq!(
            go(&ResponseFormatJsonSchemaParam {
                json_schema: ResponseFormatJsonSchemaJsonSchemaParam {
                    name: "adk_response".into(),
                    strict: Some(true),
                    description: None,
                    schema: Some(params),
                },
                kind: JsonSchemaTag,
            }),
            r#"{"json_schema":{"name":"adk_response","strict":true,"schema":{"a":2,"b":1,"properties":{},"type":"object"}},"type":"json_schema"}"#
        );
        assert_eq!(JsonSchemaTag::VALUE, "json_schema");
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Probe {
        #[serde(default, deserialize_with = "de::go_string")]
        s: String,
        #[serde(default, deserialize_with = "de::go_i64")]
        i: i64,
        #[serde(default, deserialize_with = "de::go_f64")]
        f: f64,
        #[serde(default, deserialize_with = "de::lenient")]
        o: Option<Inner>,
        #[serde(default, deserialize_with = "de::lenient")]
        v: Vec<Inner>,
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Inner {
        #[serde(default, deserialize_with = "de::go_i64")]
        n: i64,
    }

    fn probe(s: &str) -> Probe {
        serde_json::from_str(s).unwrap()
    }

    /// The coercions openai-go v3.91.1's `apijson` applies (gjson v1.19.0 `Int()`/`Float()`),
    /// recorded from Go decoding `openai.CompletionUsage.PromptTokens` (int64) and
    /// `openai.ChatCompletionTokenLogprob.Logprob` (float64) on arm64. The one divergence
    /// pinned here: Go keeps a non-finite float (`1e400` → +Inf, `"inf"` → +Inf); gadk has 0.
    #[test]
    fn decoding_follows_apijson_leniency() {
        assert_eq!(probe("{}"), Probe::default());
        assert_eq!(
            probe(r#"{"s":5,"i":"7","f":"2.5","o":{},"v":[{"n":1}]}"#),
            Probe {
                s: "5".into(),
                i: 7,
                f: 2.5,
                o: Some(Inner { n: 0 }),
                v: vec![Inner { n: 1 }],
            }
        );
        assert_eq!(probe(r#"{"s":{"a":1}}"#).s, r#"{"a":1}"#);
        assert_eq!(probe(r#"{"s":true}"#).s, "true");
        assert_eq!(probe(r#"{"s":null}"#).s, "");
        let int = |v: &str| probe(&format!(r#"{{"i":{v}}}"#)).i;
        for (v, want) in [
            ("true", 1),
            ("false", 0),
            ("null", 0),
            ("{}", 0),
            ("[1]", 0),
            (r#""7""#, 7),
            (r#"" 7""#, 0),
            (r#""+7""#, 0),
            (r#""1.5""#, 0),
            (r#""-""#, 0),
            (r#""""#, 0),
            (r#""-12""#, -12),
            (r#""99999999999999999999""#, 7_766_279_631_452_241_919),
            ("1.5", 1),
            ("2.0", 2),
            ("-1.9", -1),
            ("9007199254740993", 9_007_199_254_740_993),
            ("99999999999999999999", 7_766_279_631_452_241_919),
            ("1e19", i64::MAX),
            ("1e400", i64::MAX),
            ("-1e400", i64::MIN),
            ("1.5e20", i64::MAX),
            ("-0", 0),
        ] {
            assert_eq!(int(v), want, "int {v}");
        }
        let float = |v: &str| probe(&format!(r#"{{"f":{v}}}"#)).f;
        for (v, want) in [
            ("true", 1.0),
            ("false", 0.0),
            (r#""2.5""#, 2.5),
            (r#"" 2.5""#, 0.0),
            (r#""+2.5""#, 2.5),
            (r#"".5""#, 0.5),
            (r#""1.""#, 1.0),
            (r#""0x1p-2""#, 0.25),
            (r#""1_0""#, 10.0),
            (r#""abc""#, 0.0),
            ("{}", 0.0),
            // Divergences: Go keeps +Inf, +Inf, +Inf and -Inf.
            (r#""inf""#, 0.0),
            (r#""1e400""#, 0.0),
            ("1e400", 0.0),
            ("-1e400", 0.0),
        ] {
            assert_eq!(float(v), want, "float {v}");
        }
        assert_eq!(probe(r#"{"o":null}"#).o, None);
        assert_eq!(probe(r#"{"o":"x"}"#).o, None);
        assert_eq!(probe(r#"{"v":[1,{"n":0}]}"#).v, vec![]);
        assert_eq!(probe(r#"{"v":{"n":0}}"#).v, vec![]);
        assert_eq!(probe(r#"{"unknown":1e400,"x":[1,2]}"#), Probe::default());
    }
}
