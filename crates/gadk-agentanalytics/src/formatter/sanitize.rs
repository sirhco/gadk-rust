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

//! The sanitizer behind [`smart_truncate`]: [`smart_truncate`] / [`sanitize_value`] turn a value
//! into sanitized, truncated JSON, with [`truncate_string`] cutting text.
//!
//! The value drives a sanitizing [`serde::Serializer`] that builds the sanitized tree ([`San`]) as
//! it goes and stops where the walk stops (depth limit, node budget), so nothing is converted
//! twice. The tree is then written as the sanitized JSON text an ADK plugin writes (sorted keys,
//! HTML-escaped strings, the shortest float format).
//!
//! serde mapping: `serialize_some` is a present optional value (depth check, no node),
//! `serialize_none` / `serialize_unit` an absent one, `serialize_struct` a struct (fields by their
//! serde names; `skip_field` an omitted field), `serialize_map` a map, `serialize_seq` /
//! `serialize_tuple` a list, `serialize_bytes` a byte string, a unit variant an enum or string-like
//! type, serde_json's `RawValue` raw JSON text, and a newtype named [`JSON_NUMBER_TOKEN`] a number
//! kept as its literal text. Types that serialize to JSON by themselves (such as timestamps) are
//! walked as the value they produce. Such a value costs one node of the budget here; an ADK server
//! takes a node for the value, then walks the decoded JSON it produced (another node), so the two
//! differ only when the node budget runs out on exactly that value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::ser::{
    self, Impossible, Serialize, SerializeMap, SerializeSeq, SerializeStruct,
    SerializeStructVariant, SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
    Serializer,
};
use serde_json::{Map, Number, Value};

use super::{
    BINARY_DATA_PLACEHOLDER, COLLIDING_KEY_PLACEHOLDER, MAX_DEPTH_PLACEHOLDER, MAX_SANITIZE_DEPTH,
    NodeBudget, SANITIZE_BUDGET_PLACEHOLDER, TRUNCATION_SUFFIX, UNSUPPORTED_KEY_PLACEHOLDER,
    UNSUPPORTED_OBJ_PLACEHOLDER,
};
use crate::error::Error;
use crate::redaction::{
    DecodeFail, JNum, JVal, REDACTED_VALUE, decode_std_base64, is_sensitive_key,
    redact_json_text_with_budget, scrub_error_text, unmarshal_json_strict_with_budget,
};

/// serde_json's `RawValue` struct token (`serde_json::value::RawValue` serializes as a struct of
/// this name with one field of the same name holding the JSON text): raw JSON text.
const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

/// The newtype-struct name under which a value serializes the text of a number kept as its literal
/// (written as is when it is a valid JSON number).
pub(crate) const JSON_NUMBER_TOKEN: &str = "$gadk_agentanalytics::json.Number";

/// The gadk-genai fields that hold bytes but serialize as base64 text (gadk-genai's
/// `serde_util::base64_bytes`): `(struct name, serde field name)`. The sanitizer decodes these
/// fields back to bytes, so they show as `[BINARY_DATA]` or the UTF-8 text.
const GENAI_BYTES_FIELDS: [(&str, &str); 10] = [
    ("Blob", "data"),
    ("FunctionResponseBlob", "data"),
    ("Part", "thoughtSignature"),
    ("ReplicatedVoiceConfig", "voiceSampleAudio"),
    ("ReplicatedVoiceConfig", "consentAudio"),
    ("GenerateContentConfig", "continuationToken"),
    ("Candidate", "continuationToken"),
    ("CustomizedAvatar", "imageData"),
    ("Image", "imageBytes"),
    ("SearchEntryPoint", "sdkBlob"),
];

/// A sanitized value: the JSON-native tree the sanitizer builds (null, boolean, integers, 32- and
/// 64-bit floats, number text, string, list, map).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum San {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    F32(f32),
    /// A number kept as text (or an integer wider than 64 bits): JSON number text written as is.
    Number(String),
    Str(String),
    List(Vec<San>),
    Map(BTreeMap<String, San>),
}

impl San {
    fn placeholder(text: &str) -> (San, bool) {
        (San::Str(text.to_owned()), true)
    }

    fn is_budget_placeholder(&self) -> bool {
        matches!(self, San::Str(s) if s == SANITIZE_BUDGET_PLACEHOLDER)
    }

    /// The tree as compact JSON text: sorted keys, HTML-escaped strings, shortest floats.
    pub(crate) fn to_go_json(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            San::Null => out.extend_from_slice(b"null"),
            San::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            San::I64(n) => out.extend_from_slice(n.to_string().as_bytes()),
            San::U64(n) => out.extend_from_slice(n.to_string().as_bytes()),
            San::F64(f) => out.extend_from_slice(go_json_float64(*f).as_bytes()),
            San::F32(f) => out.extend_from_slice(go_json_float32(*f).as_bytes()),
            San::Number(n) => out.extend_from_slice(n.as_bytes()),
            San::Str(s) => write_go_string(s, out),
            San::List(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    item.write(out);
                }
                out.push(b']');
            }
            San::Map(m) => {
                out.push(b'{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write_go_string(k, out);
                    out.push(b':');
                    v.write(out);
                }
                out.push(b'}');
            }
        }
    }

    /// The tree as a `serde_json::Value`; a number kept as text or a 32-bit float becomes the
    /// `Number` its text parses to.
    pub(crate) fn into_value(self) -> Value {
        match self {
            San::Null => Value::Null,
            San::Bool(b) => Value::Bool(b),
            San::I64(n) => Value::from(n),
            San::U64(n) => Value::from(n),
            San::F64(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
            San::F32(f) => go_json_float32(f)
                .parse::<Number>()
                .map_or(Value::Null, Value::Number),
            // serde_json (no `arbitrary_precision`) holds an integer outside `i64` / `u64` (an
            // `i128` / `u128`, or such number text) as the nearest `f64`, so this `Value` form
            // loses its low digits. `to_go_json` writes the text unchanged.
            San::Number(n) => n.parse::<Number>().map_or(Value::String(n), Value::Number),
            San::Str(s) => Value::String(s),
            San::List(items) => Value::Array(items.into_iter().map(San::into_value).collect()),
            San::Map(m) => Value::Object(
                m.into_iter()
                    .map(|(k, v)| (k, v.into_value()))
                    .collect::<Map<String, Value>>(),
            ),
        }
    }
}

/// A JSON string (`gadk_model::to_go_json`: `<`, `>`, `&`, U+2028 and U+2029 escaped).
fn write_go_string(s: &str, out: &mut Vec<u8>) {
    if let Ok(b) = gadk_model::to_go_json(s) {
        out.extend_from_slice(&b);
    }
}

/// Rewrites Rust's `{:e}` float text into the JSON exponent form: a negative exponent loses its
/// leading zero (`1e-7`) and a positive one is written `e+NN` (`1e+21`).
fn go_exponent_form(e: &str) -> String {
    match e.split_once('e') {
        Some((mantissa, exp)) => match exp.strip_prefix('-') {
            Some(neg) => format!("{mantissa}e-{neg}"),
            None => format!("{mantissa}e+{exp:0>2}"),
        },
        None => e.to_owned(),
    }
}

/// The JSON text of a finite `f64`: plain decimal from 1e-6 up to 1e21, else exponent form.
fn go_json_float64(f: f64) -> String {
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        return go_exponent_form(&format!("{f:e}"));
    }
    format!("{f}")
}

/// The JSON text of a finite `f32` (shortest form at 32 bits, the exponent thresholds compared as
/// `float32`).
fn go_json_float32(f: f32) -> String {
    let abs = f.abs();
    if abs != 0.0 && !(1e-6_f32..1e21_f32).contains(&abs) {
        return go_exponent_form(&format!("{f:e}"));
    }
    format!("{f}")
}

/// The shortest `%g`-style text of a float map key, given the shortest `{:e}` text (`d.ddde±x`) and
/// the plain text of the value: exponent form (`1e+21`, `1.5e-07`) when the decimal exponent is
/// below -4 or at least 6, else the plain decimal form.
fn go_format_g(exp_text: &str, plain: &str) -> String {
    let Some((mantissa, exp)) = exp_text.split_once('e') else {
        return plain.to_owned();
    };
    let Ok(x) = exp.parse::<i32>() else {
        return plain.to_owned();
    };
    if !(-4..6).contains(&x) {
        let sign = if x < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", x.unsigned_abs());
    }
    plain.to_owned()
}

fn go_format_g64(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    go_format_g(&format!("{f:e}"), &format!("{f}"))
}

fn go_format_g32(f: f32) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    go_format_g(&format!("{f:e}"), &format!("{f}"))
}

/// Whether `n` is exactly one JSON number with no surrounding space.
pub(crate) fn is_json_number(n: &str) -> bool {
    let b = n.as_bytes();
    let digits = |mut i: usize| {
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        (i, i > start)
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => i = digits(i).0,
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        let (j, ok) = digits(i + 1);
        if !ok {
            return false;
        }
        i = j;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let (j, ok) = digits(i);
        if !ok {
            return false;
        }
        i = j;
    }
    i == b.len()
}

/// `s` cut to `max_length` chars followed by [`TRUNCATION_SUFFIX`]; a negative `max_length` means
/// no limit.
pub(crate) fn truncate_string(s: &str, max_length: i64) -> (String, bool) {
    let Ok(max) = usize::try_from(max_length) else {
        return (s.to_owned(), false);
    };
    match s.char_indices().nth(max) {
        None => (s.to_owned(), false),
        Some((cut, _)) => (format!("{}{TRUNCATION_SUFFIX}", &s[..cut]), true),
    }
}

/// `obj` as sanitized JSON text, with redaction optional, and whether anything was truncated,
/// redacted away or replaced.
pub(crate) fn smart_truncate_bytes<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> (Vec<u8>, bool) {
    let (v, truncated) = sanitize(obj, max_length, redact);
    (v.to_go_json(), truncated)
}

/// `obj` as the sanitized tree.
pub(crate) fn sanitize<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> (San, bool) {
    let mut st = State {
        max_length,
        redact,
        budget: NodeBudget::new(),
    };
    walk(&mut st, 0, obj)
}

/// `obj` as sanitized, truncated JSON (redaction on) and whether anything was truncated or
/// replaced. The `Err` case cannot happen: the sanitized tree is always encodable.
pub fn smart_truncate<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
) -> Result<(Vec<u8>, bool), Error> {
    smart_truncate_with(obj, max_length, true)
}

/// [`smart_truncate`] with redaction optional.
pub(crate) fn smart_truncate_with<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> Result<(Vec<u8>, bool), Error> {
    Ok(smart_truncate_bytes(obj, max_length, redact))
}

/// The sanitized, truncated value (depth limit [`MAX_SANITIZE_DEPTH`], node budget
/// `MAX_SANITIZE_NODES`) and whether anything was lost.
pub(crate) fn sanitize_value<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> (Value, bool) {
    let (v, truncated) = sanitize(obj, max_length, redact);
    (v.into_value(), truncated)
}

/// The options and node budget of one sanitization.
struct State {
    max_length: i64,
    redact: bool,
    budget: NodeBudget,
}

impl State {
    /// A string at `depth`: redacted (when enabled), then truncated to the maximum length.
    fn string_value(&mut self, s: &str, depth: usize) -> (San, bool) {
        let mut text = s.to_owned();
        let mut content_lost = false;
        if self.redact {
            let (redacted, _, lost) = redact_json_text_with_budget(&text, depth, &mut self.budget);
            content_lost = lost;
            let scrubbed = scrub_error_text(&redacted);
            if scrubbed != redacted {
                text = scrubbed;
                content_lost = true;
            } else {
                text = redacted;
            }
        }
        let (shown, length_truncated) = truncate_string(&text, self.max_length);
        (San::Str(shown), content_lost || length_truncated)
    }

    /// Bytes at `depth`: UTF-8 text as a string, else `[BINARY_DATA]`.
    fn bytes(&mut self, b: &[u8], depth: usize) -> (San, bool) {
        match std::str::from_utf8(b) {
            Ok(s) => self.string_value(s, depth),
            Err(_) => San::placeholder(BINARY_DATA_PLACEHOLDER),
        }
    }

    /// Number text: kept as a number when it is a valid JSON number, else truncated as a string.
    fn number(&self, n: &str) -> (San, bool) {
        if is_json_number(n) {
            return (San::Number(n.to_owned()), false);
        }
        let (s, t) = truncate_string(n, self.max_length);
        (San::Str(s), t)
    }
}

/// Sanitizes `v` at `depth`; a value whose `Serialize` impl fails is `[UNSUPPORTED_OBJECT]`.
fn walk<T: Serialize + ?Sized>(st: &mut State, depth: usize, v: &T) -> (San, bool) {
    v.serialize(Walker {
        st,
        depth,
        base64_bytes: false,
    })
    .unwrap_or_else(|_| San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER))
}

/// A `Serialize` impl failed (it returned an error).
#[derive(Debug)]
pub(crate) struct SanitizeError(String);

impl fmt::Display for SanitizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SanitizeError {}

impl ser::Error for SanitizeError {
    fn custom<M: fmt::Display>(msg: M) -> Self {
        SanitizeError(msg.to_string())
    }
}

/// The sanitizing serializer of one value at `depth`.
struct Walker<'s> {
    st: &'s mut State,
    depth: usize,
    /// The value is a gadk-genai bytes field written as base64 text.
    base64_bytes: bool,
}

type Out = (San, bool);

impl<'s> Walker<'s> {
    /// The common head of the walk for a value that is not an optional: take a node, then check the
    /// depth.
    fn enter(&mut self) -> Option<Out> {
        if !self.st.budget.take() {
            return Some(San::placeholder(SANITIZE_BUDGET_PLACEHOLDER));
        }
        if self.depth >= MAX_SANITIZE_DEPTH {
            return Some(San::placeholder(MAX_DEPTH_PLACEHOLDER));
        }
        None
    }

    /// An absent optional or unit value: a node, no depth check.
    fn nil(self) -> Out {
        if !self.st.budget.take() {
            return San::placeholder(SANITIZE_BUDGET_PLACEHOLDER);
        }
        (San::Null, false)
    }

    fn scalar(mut self, v: San) -> Result<Out, SanitizeError> {
        Ok(self.enter().unwrap_or((v, false)))
    }

    fn compound(mut self, kind: Kind) -> Compound<'s> {
        let done = self.enter();
        Compound {
            child_depth: self.depth + 1,
            st: self.st,
            kind,
            done,
            stopped: false,
            truncated: false,
            items: Vec::new(),
            map: BTreeMap::new(),
            unsupported_keys: 0,
            pending_key: None,
            pending_redacted: false,
            seen: BTreeSet::new(),
            raw: None,
        }
    }
}

impl<'s> Serializer for Walker<'s> {
    type Ok = Out;
    type Error = SanitizeError;
    type SerializeSeq = Compound<'s>;
    type SerializeTuple = Compound<'s>;
    type SerializeTupleStruct = Compound<'s>;
    type SerializeTupleVariant = Compound<'s>;
    type SerializeMap = Compound<'s>;
    type SerializeStruct = Compound<'s>;
    type SerializeStructVariant = Compound<'s>;

    fn serialize_bool(self, v: bool) -> Result<Out, SanitizeError> {
        self.scalar(San::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Out, SanitizeError> {
        self.scalar(San::I64(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<Out, SanitizeError> {
        self.scalar(San::I64(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<Out, SanitizeError> {
        self.scalar(San::I64(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<Out, SanitizeError> {
        self.scalar(San::I64(v))
    }
    fn serialize_i128(self, v: i128) -> Result<Out, SanitizeError> {
        self.scalar(San::Number(v.to_string()))
    }
    fn serialize_u8(self, v: u8) -> Result<Out, SanitizeError> {
        self.scalar(San::U64(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<Out, SanitizeError> {
        self.scalar(San::U64(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<Out, SanitizeError> {
        self.scalar(San::U64(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Out, SanitizeError> {
        self.scalar(San::U64(v))
    }
    fn serialize_u128(self, v: u128) -> Result<Out, SanitizeError> {
        self.scalar(San::Number(v.to_string()))
    }

    /// A 32-bit float: NaN and the infinities as `"NaN"`, `"+Inf"`, `"-Inf"`.
    fn serialize_f32(self, v: f32) -> Result<Out, SanitizeError> {
        self.scalar(if v.is_finite() {
            San::F32(v)
        } else {
            San::Str(go_format_g32(v))
        })
    }

    /// A 64-bit float: NaN and the infinities as `"NaN"`, `"+Inf"`, `"-Inf"`.
    fn serialize_f64(self, v: f64) -> Result<Out, SanitizeError> {
        self.scalar(if v.is_finite() {
            San::F64(v)
        } else {
            San::Str(go_format_g64(v))
        })
    }

    /// A `char` is sanitized as the one-character string serde_json writes.
    fn serialize_char(self, v: char) -> Result<Out, SanitizeError> {
        self.serialize_str(v.encode_utf8(&mut [0; 4]))
    }

    fn serialize_str(mut self, v: &str) -> Result<Out, SanitizeError> {
        if let Some(out) = self.enter() {
            return Ok(out);
        }
        if self.base64_bytes {
            let decoded = if v.is_empty() {
                Some(Vec::new())
            } else {
                decode_std_base64(v)
            };
            if let Some(b) = decoded {
                return Ok(self.st.bytes(&b, self.depth));
            }
        }
        Ok(self.st.string_value(v, self.depth))
    }

    fn serialize_bytes(mut self, v: &[u8]) -> Result<Out, SanitizeError> {
        if let Some(out) = self.enter() {
            return Ok(out);
        }
        Ok(self.st.bytes(v, self.depth))
    }

    fn serialize_none(self) -> Result<Out, SanitizeError> {
        Ok(self.nil())
    }

    /// A present optional value: the depth check without taking a node, then the value.
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Out, SanitizeError> {
        if self.depth >= MAX_SANITIZE_DEPTH {
            return Ok(San::placeholder(MAX_DEPTH_PLACEHOLDER));
        }
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Out, SanitizeError> {
        Ok(self.nil())
    }

    /// A unit struct: `{}`.
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Out, SanitizeError> {
        self.scalar(San::Map(BTreeMap::new()))
    }

    /// A unit enum variant (such as genai's enums): its name as a string.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Out, SanitizeError> {
        self.serialize_str(variant)
    }

    /// A newtype named for a credential (`Token`, `APIKey`, `Credential`, …; see
    /// `is_sensitive_key`) is written `[REDACTED]` under redaction, without reading its value; any
    /// other newtype is its value.
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        mut self,
        name: &'static str,
        value: &T,
    ) -> Result<Out, SanitizeError> {
        if self.st.redact && name != JSON_NUMBER_TOKEN && is_sensitive_key(name) {
            if let Some(out) = self.enter() {
                return Ok(out);
            }
            return Ok((San::Str(REDACTED_VALUE.to_owned()), true));
        }
        if name != JSON_NUMBER_TOKEN {
            return value.serialize(self);
        }
        if let Some(out) = self.enter() {
            return Ok(out);
        }
        match value.serialize(KeySer) {
            Ok(n) => Ok(self.st.number(&n)),
            Err(_) => Ok(San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER)),
        }
    }

    /// Newtype, tuple and struct enum variants are sanitized as serde_json writes them,
    /// `{"variant": value}`.
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Out, SanitizeError> {
        let mut c = self.compound(Kind::Map);
        c.entry(Ok(variant.to_owned()), value);
        Ok(c.finish())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::Seq))
    }

    fn serialize_tuple(self, _len: usize) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::Seq))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::Seq))
    }

    /// Sanitized as `{"variant": [..]}` (serde_json's form; see `serialize_newtype_variant`).
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::SeqVariant(variant)))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::Map))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Compound<'s>, SanitizeError> {
        if name == RAW_VALUE_TOKEN {
            return Ok(self.compound(Kind::Raw));
        }
        Ok(self.compound(Kind::Struct(name)))
    }

    /// Sanitized as `{"variant": {..}}` (serde_json's form; see `serialize_newtype_variant`).
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'s>, SanitizeError> {
        Ok(self.compound(Kind::StructVariant(variant)))
    }
}

/// What a [`Compound`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A list.
    Seq,
    /// A map.
    Map,
    /// A struct (the serde name of the struct).
    Struct(&'static str),
    /// serde_json's `RawValue`: the raw JSON text is decoded, then walked.
    Raw,
    /// An enum tuple variant: a list inside `{"variant": ..}`.
    SeqVariant(&'static str),
    /// An enum struct variant: an object inside `{"variant": ..}`.
    StructVariant(&'static str),
}

/// A container being sanitized (a list, map or struct).
struct Compound<'s> {
    st: &'s mut State,
    child_depth: usize,
    kind: Kind,
    /// The container was replaced by a placeholder before its elements were walked.
    done: Option<Out>,
    /// The node budget ran out: later elements are not walked.
    stopped: bool,
    truncated: bool,
    items: Vec<San>,
    map: BTreeMap<String, San>,
    unsupported_keys: usize,
    /// The key of the map entry whose value comes next (`Err`: not a supported key).
    pending_key: Option<Result<String, ()>>,
    /// The key of the map entry whose value comes next is redacted whole (see
    /// `map_key_redacts_whole`).
    pending_redacted: bool,
    /// Struct field names already written or omitted.
    seen: BTreeSet<String>,
    /// The JSON text of a `RawValue`.
    raw: Option<String>,
}

impl Compound<'_> {
    fn idle(&self) -> bool {
        self.done.is_some() || self.stopped
    }

    fn stop_with_budget_key(&mut self) {
        self.map.insert(
            SANITIZE_BUDGET_PLACEHOLDER.to_owned(),
            San::Str(SANITIZE_BUDGET_PLACEHOLDER.to_owned()),
        );
        self.truncated = true;
        self.stopped = true;
    }

    /// One list element.
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) {
        if self.idle() {
            return;
        }
        if self.st.budget.remaining <= 0 {
            self.items
                .push(San::Str(SANITIZE_BUDGET_PLACEHOLDER.to_owned()));
            self.truncated = true;
            self.stopped = true;
            return;
        }
        let (val, t) = walk(self.st, self.child_depth, value);
        let stop = val.is_budget_placeholder() && self.st.budget.remaining <= 0;
        self.items.push(val);
        self.truncated |= t;
        self.stopped = stop;
    }

    /// A map entry whose key is redacted whole (see `map_key_redacts_whole`): one node for the key
    /// and one for the skipped value, written `{"[REDACTED]":"[REDACTED]"}`.
    fn redacted_entry(&mut self) {
        if self.idle() {
            return;
        }
        if self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        self.st.budget.take();
        if !self.st.budget.take() {
            self.stop_with_budget_key();
            return;
        }
        self.map.insert(
            REDACTED_VALUE.to_owned(),
            San::Str(REDACTED_VALUE.to_owned()),
        );
        self.truncated = true;
    }

    /// One map entry (`key` `Err`: the key is not a supported key type).
    fn entry<T: Serialize + ?Sized>(&mut self, key: Result<String, ()>, value: &T) {
        if self.idle() {
            return;
        }
        if self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        match key {
            Ok(k) => self.named_value(k, value, true),
            Err(()) => {
                self.unsupported_keys += 1;
                self.truncated = true;
                let k = if self.unsupported_keys > 1 {
                    format!("[UNSUPPORTED_KEY_{}]", self.unsupported_keys)
                } else {
                    UNSUPPORTED_KEY_PLACEHOLDER.to_owned()
                };
                self.named_value(k, value, false);
            }
        }
    }

    /// The part of the map and struct handling after the key is known: redaction of a sensitive key
    /// (`check_sensitive`), else the walk of the value.
    fn named_value<T: Serialize + ?Sized>(
        &mut self,
        key: String,
        value: &T,
        check_sensitive: bool,
    ) {
        if check_sensitive && self.st.redact && is_sensitive_key(&key) {
            if !self.st.budget.take() {
                self.stop_with_budget_key();
                return;
            }
            self.map.insert(key, San::Str(REDACTED_VALUE.to_owned()));
            return;
        }
        let (val, t) = walk(self.st, self.child_depth, value);
        if val.is_budget_placeholder() && self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        // Keys written alike (such as `"1"` and 1) keep their value only if they agree on it,
        // so that which one is written does not depend on the order of the map.
        match self.map.get(&key) {
            Some(existing) if *existing != val => {
                let placeholder = if self.st.redact {
                    REDACTED_VALUE
                } else {
                    COLLIDING_KEY_PLACEHOLDER
                };
                self.map.insert(key, San::Str(placeholder.to_owned()));
                self.truncated = true;
            }
            Some(_) => {}
            None => {
                self.map.insert(key, val);
            }
        }
        self.truncated |= t;
    }

    /// One struct field.
    fn field<T: Serialize + ?Sized>(&mut self, name: &str, key: &'static str, value: &T) {
        if self.kind == Kind::Raw {
            if let Ok(text) = value.serialize(KeySer) {
                self.raw = Some(text);
            }
            return;
        }
        if self.idle() {
            return;
        }
        if self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        if !self.seen.insert(key.to_owned()) {
            return;
        }
        // A field whose type is redacted whole is written `[REDACTED]` (and marks the row
        // truncated) before its name is looked at.
        if self.st.redact && value_redacts_whole(value) {
            if !self.st.budget.take() {
                self.stop_with_budget_key();
                return;
            }
            self.map
                .insert(key.to_owned(), San::Str(REDACTED_VALUE.to_owned()));
            self.truncated = true;
            return;
        }
        if GENAI_BYTES_FIELDS.contains(&(name, key)) {
            return self.base64_field(key, value);
        }
        self.named_value(key.to_owned(), value, true);
    }

    /// A gadk-genai bytes field (see [`GENAI_BYTES_FIELDS`]).
    fn base64_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) {
        if self.st.redact && is_sensitive_key(key) {
            return self.named_value(key.to_owned(), value, true);
        }
        let (val, t) = value
            .serialize(Walker {
                st: self.st,
                depth: self.child_depth,
                base64_bytes: true,
            })
            .unwrap_or_else(|_| San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER));
        if val.is_budget_placeholder() && self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        self.map.insert(key.to_owned(), val);
        self.truncated |= t;
    }

    /// A field omitted by `skip_serializing_if` (it still meets the budget check at the top of the
    /// loop and claims its name).
    fn skipped(&mut self, key: &'static str) {
        if self.kind == Kind::Raw || self.idle() {
            return;
        }
        if self.st.budget.remaining <= 0 {
            self.stop_with_budget_key();
            return;
        }
        self.seen.insert(key.to_owned());
    }

    fn finish(mut self) -> Out {
        if let Some(out) = self.done.take() {
            return out;
        }
        match self.kind {
            Kind::Seq => (San::List(self.items), self.truncated),
            Kind::Map | Kind::Struct(_) => (San::Map(self.map), self.truncated),
            Kind::Raw => self.finish_raw(),
            Kind::SeqVariant(variant) => {
                let mut m = BTreeMap::new();
                m.insert(variant.to_owned(), San::List(self.items));
                (San::Map(m), self.truncated)
            }
            Kind::StructVariant(variant) => {
                let mut m = BTreeMap::new();
                m.insert(variant.to_owned(), San::Map(self.map));
                (San::Map(m), self.truncated)
            }
        }
    }

    /// A `RawValue`: the text is decoded (numbers kept as text), then walked at the same depth.
    ///
    /// The text is decoded by `redaction`'s strict decoder, which stops past 100 levels, so a
    /// `RawValue` nested deeper is `[UNSUPPORTED_OBJECT]`; an ADK server decodes up to 10000
    /// levels, and its walk then writes `[MAX_DEPTH_EXCEEDED]`.
    fn finish_raw(self) -> Out {
        let Some(text) = self.raw else {
            return San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER);
        };
        let mut unlimited = NodeBudget {
            remaining: i64::MAX,
        };
        match unmarshal_json_strict_with_budget(&text, &mut unlimited) {
            Ok((decoded, _)) => walk(self.st, self.child_depth - 1, &JValSer(&decoded)),
            Err(DecodeFail::Budget | DecodeFail::Syntax) => {
                San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER)
            }
        }
    }
}

impl SerializeSeq for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
        self.element(value);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeTuple for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
        self.element(value);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeTupleStruct for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
        self.element(value);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeTupleVariant for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
        self.element(value);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeMap for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), SanitizeError> {
        self.pending_redacted = self.st.redact && map_key_redacts_whole(key);
        self.pending_key = if self.pending_redacted {
            None
        } else {
            Some(key.serialize(KeySer).map_err(|_| ()))
        };
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
        if std::mem::take(&mut self.pending_redacted) {
            self.redacted_entry();
            return Ok(());
        }
        let key = self.pending_key.take().unwrap_or(Err(()));
        self.entry(key, value);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeStruct for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SanitizeError> {
        let name = match self.kind {
            Kind::Struct(name) => name,
            _ => "",
        };
        self.field(name, key, value);
        Ok(())
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), SanitizeError> {
        self.skipped(key);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

impl SerializeStructVariant for Compound<'_> {
    type Ok = Out;
    type Error = SanitizeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SanitizeError> {
        self.field("", key, value);
        Ok(())
    }
    fn skip_field(&mut self, key: &'static str) -> Result<(), SanitizeError> {
        self.skipped(key);
        Ok(())
    }
    fn end(self) -> Result<Out, SanitizeError> {
        Ok(self.finish())
    }
}

/// Whether the map key is redacted whole from its type alone — a newtype, tuple struct or struct
/// named for a credential (even a plain credential-named struct, which cannot be an object key),
/// through `Some`.
fn map_key_redacts_whole<T: Serialize + ?Sized>(key: &T) -> bool {
    matches!(
        key.serialize(KeyTypeProbe { plain_struct: true }),
        Err(KeyProbe::Sensitive)
    )
}

/// Whether a value is redacted whole: a newtype named for a credential, through `Some` (a
/// credential-named struct is written by its fields).
fn value_redacts_whole<T: Serialize + ?Sized>(value: &T) -> bool {
    matches!(
        value.serialize(KeyTypeProbe {
            plain_struct: false
        }),
        Err(KeyProbe::Sensitive)
    )
}

/// The outcome of [`KeyTypeProbe`]: `Sensitive` stops at a credential-named type; `Other`
/// stops anywhere else.
#[derive(Debug)]
enum KeyProbe {
    Sensitive,
    Other,
}

impl fmt::Display for KeyProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyProbe::Sensitive => "credential-named key",
            KeyProbe::Other => "other key",
        })
    }
}

impl std::error::Error for KeyProbe {}

impl ser::Error for KeyProbe {
    fn custom<M: fmt::Display>(_msg: M) -> Self {
        KeyProbe::Other
    }
}

/// Reads the serde type names of a map key or value (never its scalar values) for
/// [`map_key_redacts_whole`] and [`value_redacts_whole`]; with `plain_struct`, a struct named
/// for a credential matches too.
struct KeyTypeProbe {
    plain_struct: bool,
}

impl KeyTypeProbe {
    fn named(&self, name: &str) -> Result<ser::Impossible<(), KeyProbe>, KeyProbe> {
        Err(if self.plain_struct && is_sensitive_key(name) {
            KeyProbe::Sensitive
        } else {
            KeyProbe::Other
        })
    }
}

impl Serializer for KeyTypeProbe {
    type Ok = ();
    type Error = KeyProbe;
    type SerializeSeq = ser::Impossible<(), KeyProbe>;
    type SerializeTuple = ser::Impossible<(), KeyProbe>;
    type SerializeTupleStruct = ser::Impossible<(), KeyProbe>;
    type SerializeTupleVariant = ser::Impossible<(), KeyProbe>;
    type SerializeMap = ser::Impossible<(), KeyProbe>;
    type SerializeStruct = ser::Impossible<(), KeyProbe>;
    type SerializeStructVariant = ser::Impossible<(), KeyProbe>;

    fn serialize_bool(self, _v: bool) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_i8(self, _v: i8) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_i16(self, _v: i16) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_i32(self, _v: i32) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_i64(self, _v: i64) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_u8(self, _v: u8) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_u16(self, _v: u16) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_u32(self, _v: u32) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_u64(self, _v: u64) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_f32(self, _v: f32) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_f64(self, _v: f64) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_char(self, _v: char) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_str(self, _v: &str) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), KeyProbe> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), KeyProbe> {
        self.named(name).map(|_| ())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> Result<(), KeyProbe> {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), KeyProbe> {
        if name != JSON_NUMBER_TOKEN && is_sensitive_key(name) {
            return Err(KeyProbe::Sensitive);
        }
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), KeyProbe> {
        Err(KeyProbe::Other)
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, KeyProbe> {
        Err(KeyProbe::Other)
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, KeyProbe> {
        Err(KeyProbe::Other)
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, KeyProbe> {
        self.named(name)
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, KeyProbe> {
        Err(KeyProbe::Other)
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, KeyProbe> {
        Err(KeyProbe::Other)
    }
    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, KeyProbe> {
        self.named(name)
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, KeyProbe> {
        Err(KeyProbe::Other)
    }
}

/// The JSON object key of a map key — strings, integers, booleans and floats (shortest `%g`-style
/// text), a unit or `None` as `"null"`; `Err` for any other key (a sequence, map, struct, bytes,
/// ...). Also captures the text of a `RawValue` or number-text newtype.
struct KeySer;

impl Serializer for KeySer {
    type Ok = String;
    type Error = SanitizeError;
    type SerializeSeq = Impossible<String, SanitizeError>;
    type SerializeTuple = Impossible<String, SanitizeError>;
    type SerializeTupleStruct = Impossible<String, SanitizeError>;
    type SerializeTupleVariant = Impossible<String, SanitizeError>;
    type SerializeMap = Impossible<String, SanitizeError>;
    type SerializeStruct = Impossible<String, SanitizeError>;
    type SerializeStructVariant = Impossible<String, SanitizeError>;

    fn serialize_bool(self, v: bool) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_i8(self, v: i8) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_i16(self, v: i16) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_i32(self, v: i32) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_i64(self, v: i64) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_i128(self, v: i128) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_u8(self, v: u8) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_u16(self, v: u16) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_u32(self, v: u32) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_u64(self, v: u64) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_u128(self, v: u128) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_f32(self, v: f32) -> Result<String, SanitizeError> {
        Ok(go_format_g32(v))
    }
    fn serialize_f64(self, v: f64) -> Result<String, SanitizeError> {
        Ok(go_format_g64(v))
    }
    fn serialize_char(self, v: char) -> Result<String, SanitizeError> {
        Ok(v.to_string())
    }
    fn serialize_str(self, v: &str) -> Result<String, SanitizeError> {
        Ok(v.to_owned())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<String, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_none(self) -> Result<String, SanitizeError> {
        Ok("null".to_owned())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String, SanitizeError> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<String, SanitizeError> {
        Ok("null".to_owned())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<String, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String, SanitizeError> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String, SanitizeError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, SanitizeError> {
        Err(ser::Error::custom("unsupported key"))
    }
}

/// A decoded [`JVal`] walked as the value a JSON decoder builds (null, boolean, number text,
/// string, list, map).
struct JValSer<'a>(&'a JVal);

impl Serialize for JValSer<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            JVal::Null => s.serialize_unit(),
            JVal::Bool(b) => s.serialize_bool(*b),
            JVal::Num(JNum::Text(t)) => s.serialize_newtype_struct(JSON_NUMBER_TOKEN, t),
            JVal::Num(JNum::Value(n)) => n.serialize(s),
            JVal::Str(t) => s.serialize_str(t),
            JVal::Arr(items) => s.collect_seq(items.iter().map(JValSer)),
            JVal::Obj(m) => s.collect_map(m.iter().map(|(k, v)| (k, JValSer(v)))),
        }
    }
}
