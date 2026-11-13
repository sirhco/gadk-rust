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

//! Port of ADK-Go `plugin/agentanalytics/json_formatter.go`'s sanitizer: `SmartTruncate`,
//! `sanitizeValue`, the `sanitizer` reflection walk (`walk`, `walkMap`, `walkList`,
//! `walkStruct`, `mapKeyString`, `number`, `floatValue`, `stringValue`, `bytes`,
//! `marshaled`) and `truncateString`.
//!
//! Go walks any value by reflection; here the value drives a sanitizing [`serde::Serializer`]
//! that builds the sanitized tree ([`San`]) as it goes and stops where Go's walk stops (depth
//! limit, node budget), so nothing is converted twice. The tree is then written as Go's
//! `json.Marshal` writes it (sorted keys, HTML-escaped strings, Go float format).
//!
//! Go → serde mapping: a Go pointer is `serialize_some` (depth check, no node), a nil pointer or
//! interface `serialize_none` / `serialize_unit`, a struct `serialize_struct` (fields by their
//! serde names; `skip_field` is Go's omitted field), a map `serialize_map`, a slice or array
//! `serialize_seq` / `serialize_tuple`, `[]byte` `serialize_bytes`, a named string type a unit
//! variant, `json.RawMessage` serde_json's `RawValue`, `json.Number` a newtype named
//! [`JSON_NUMBER_TOKEN`]. Types with a custom `MarshalJSON` in Go serialize to that JSON in
//! gadk already (e.g. `time.Time` ↔ chrono), so they are walked as the value they produce.
//! Divergence (ledger 7b-1): such a value costs one node of the budget here; Go takes a node for
//! the value, then walks the decoded `MarshalJSON` result (another node), so the two differ only
//! when the node budget runs out on exactly that value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::ser::{
    self, Impossible, Serialize, SerializeMap, SerializeSeq, SerializeStruct,
    SerializeStructVariant, SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
    Serializer,
};
use serde_json::{Map, Number, Value};

use super::{
    BINARY_DATA_PLACEHOLDER, MAX_DEPTH_PLACEHOLDER, MAX_SANITIZE_DEPTH, NodeBudget,
    SANITIZE_BUDGET_PLACEHOLDER, TRUNCATION_SUFFIX, UNSUPPORTED_KEY_PLACEHOLDER,
    UNSUPPORTED_OBJ_PLACEHOLDER,
};
use crate::error::Error;
use crate::redaction::{
    DecodeFail, JNum, JVal, REDACTED_VALUE, decode_std_base64, is_sensitive_key,
    redact_json_text_with_budget, scrub_error_text, unmarshal_json_strict_with_budget,
};

/// serde_json's `RawValue` struct token (`serde_json::value::RawValue` serializes as a struct of
/// this name with one field of the same name holding the JSON text): Go's `json.RawMessage`.
const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

/// The newtype-struct name under which a value serializes the text of a Go `json.Number`
/// (written as is when it is a valid JSON number).
pub(crate) const JSON_NUMBER_TOKEN: &str = "$gadk_agentanalytics::json.Number";

/// The gadk-genai fields that are Go `[]byte` but serialize as base64 text (gadk-genai's
/// `serde_util::base64_bytes`): `(struct name, serde field name)`. Go's walk sees bytes there
/// (`[BINARY_DATA]` or the UTF-8 text), so the sanitizer decodes these fields back to bytes.
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

/// A sanitized value: the JSON-native tree Go's `sanitizeValue` builds (`nil`, `bool`, the
/// integer kinds, `float32` / `float64`, `json.Number`, `string`, `[]any`, `map[string]any`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum San {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    F32(f32),
    /// A `json.Number` (or an integer wider than 64 bits): JSON number text written as is.
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

    /// Go `json.Marshal` of the tree.
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

    /// The tree as a `serde_json::Value` (Go `sanitizeValue`'s `any`); a `json.Number` or
    /// `float32` becomes the `Number` its text parses to.
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

/// Go's encoding of a JSON string (`gadk_model::to_go_json`: `<`, `>`, `&`, U+2028 and U+2029
/// escaped).
fn write_go_string(s: &str, out: &mut Vec<u8>) {
    if let Ok(b) = gadk_model::to_go_json(s) {
        out.extend_from_slice(&b);
    }
}

/// Go `encoding/json`'s `floatEncoder` exponent form: `'e'` output with a negative two-digit
/// exponent's leading zero dropped and a positive exponent written `e+NN`.
fn go_exponent_form(e: &str) -> String {
    match e.split_once('e') {
        Some((mantissa, exp)) => match exp.strip_prefix('-') {
            Some(neg) => format!("{mantissa}e-{neg}"),
            None => format!("{mantissa}e+{exp:0>2}"),
        },
        None => e.to_owned(),
    }
}

/// Go `encoding/json` text of a finite `float64`.
fn go_json_float64(f: f64) -> String {
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        return go_exponent_form(&format!("{f:e}"));
    }
    format!("{f}")
}

/// Go `encoding/json` text of a finite `float32` (`bits == 32`: shortest form at 32 bits, the
/// exponent thresholds compared as `float32`).
fn go_json_float32(f: f32) -> String {
    let abs = f.abs();
    if abs != 0.0 && !(1e-6_f32..1e21_f32).contains(&abs) {
        return go_exponent_form(&format!("{f:e}"));
    }
    format!("{f}")
}

/// Go `strconv.FormatFloat(f, 'g', -1, bits)` for a map key, given the shortest `{:e}` text
/// (`d.ddde±x`) of the value: `%e` form (`1e+21`, `1.5e-07`) when the decimal exponent is
/// below -4 or at least 6 (Go's `eprec` for the shortest form), else `%f` form.
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

/// Go `isJSONNumber(n)`: `n` is one JSON number with no surrounding space.
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

/// Go `truncateString(s, maxLength)`: `s` cut to `max_length` chars followed by
/// [`TRUNCATION_SUFFIX`]; a negative `max_length` means no limit.
pub(crate) fn truncate_string(s: &str, max_length: i64) -> (String, bool) {
    let Ok(max) = usize::try_from(max_length) else {
        return (s.to_owned(), false);
    };
    match s.char_indices().nth(max) {
        None => (s.to_owned(), false),
        Some((cut, _)) => (format!("{}{TRUNCATION_SUFFIX}", &s[..cut]), true),
    }
}

/// Go `SmartTruncate(obj, maxLength)` with redaction optional (Go `smartTruncate`): `obj` as
/// Go's sanitized JSON text and whether anything was truncated, redacted away or replaced.
pub(crate) fn smart_truncate_bytes<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> (Vec<u8>, bool) {
    let (v, truncated) = sanitize(obj, max_length, redact);
    (v.to_go_json(), truncated)
}

/// Go `sanitizeValue(obj, maxLength, redact)` as the sanitized tree.
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

/// Go `SmartTruncate(obj, maxLength)`: `obj` as sanitized, truncated JSON (redaction on) and
/// whether anything was truncated or replaced. The `Err` case of Go's `json.Marshal` cannot
/// happen: the sanitized tree is always encodable.
pub fn smart_truncate<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
) -> Result<(Vec<u8>, bool), Error> {
    smart_truncate_with(obj, max_length, true)
}

/// Go `smartTruncate(obj, maxLength, redact)`.
pub(crate) fn smart_truncate_with<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> Result<(Vec<u8>, bool), Error> {
    Ok(smart_truncate_bytes(obj, max_length, redact))
}

/// Go `sanitizeValue(obj, maxLength, redact)`: the sanitized, truncated value (depth limit
/// [`MAX_SANITIZE_DEPTH`], node budget `MAX_SANITIZE_NODES`) and whether anything was lost.
pub(crate) fn sanitize_value<T: Serialize + ?Sized>(
    obj: &T,
    max_length: i64,
    redact: bool,
) -> (Value, bool) {
    let (v, truncated) = sanitize(obj, max_length, redact);
    (v.into_value(), truncated)
}

/// Go `sanitizer`: the options and node budget of one sanitization.
struct State {
    max_length: i64,
    redact: bool,
    budget: NodeBudget,
}

impl State {
    /// Go `stringValue(str, depth)`.
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

    /// Go `bytes(b, depth)`.
    fn bytes(&mut self, b: &[u8], depth: usize) -> (San, bool) {
        match std::str::from_utf8(b) {
            Ok(s) => self.string_value(s, depth),
            Err(_) => San::placeholder(BINARY_DATA_PLACEHOLDER),
        }
    }

    /// Go `number(n)`.
    fn number(&self, n: &str) -> (San, bool) {
        if is_json_number(n) {
            return (San::Number(n.to_owned()), false);
        }
        let (s, t) = truncate_string(n, self.max_length);
        (San::Str(s), t)
    }
}

/// Go `s.walk(v, depth)`: a value that fails to serialize is Go's failing `MarshalJSON`.
fn walk<T: Serialize + ?Sized>(st: &mut State, depth: usize, v: &T) -> (San, bool) {
    v.serialize(Walker {
        st,
        depth,
        base64_bytes: false,
    })
    .unwrap_or_else(|_| San::placeholder(UNSUPPORTED_OBJ_PLACEHOLDER))
}

/// A `Serialize` impl failed (Go: `MarshalJSON` returned an error or panicked).
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

/// The sanitizing serializer of one value at `depth` (Go `walk(v, depth)`).
struct Walker<'s> {
    st: &'s mut State,
    depth: usize,
    /// The value is a gadk-genai `[]byte` field written as base64 text.
    base64_bytes: bool,
}

type Out = (San, bool);

impl<'s> Walker<'s> {
    /// The common head of Go's walk for a non-pointer value: take a node, then check the depth.
    fn enter(&mut self) -> Option<Out> {
        if !self.st.budget.take() {
            return Some(San::placeholder(SANITIZE_BUDGET_PLACEHOLDER));
        }
        if self.depth >= MAX_SANITIZE_DEPTH {
            return Some(San::placeholder(MAX_DEPTH_PLACEHOLDER));
        }
        None
    }

    /// A nil pointer or interface: a node, no depth check.
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

    /// Go `floatValue` for a `float32`.
    fn serialize_f32(self, v: f32) -> Result<Out, SanitizeError> {
        self.scalar(if v.is_finite() {
            San::F32(v)
        } else {
            San::Str(go_format_g32(v))
        })
    }

    /// Go `floatValue` for a `float64`: NaN and the infinities as `"NaN"`, `"+Inf"`, `"-Inf"`.
    fn serialize_f64(self, v: f64) -> Result<Out, SanitizeError> {
        self.scalar(if v.is_finite() {
            San::F64(v)
        } else {
            San::Str(go_format_g64(v))
        })
    }

    /// Divergence (ledger 7b-1): Go has no `char` (a rune is an `int32`); a Rust `char` is
    /// sanitized as the one-character string serde_json writes.
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

    /// A non-nil Go pointer: the depth check without taking a node, then the pointee.
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Out, SanitizeError> {
        if self.depth >= MAX_SANITIZE_DEPTH {
            return Ok(San::placeholder(MAX_DEPTH_PLACEHOLDER));
        }
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Out, SanitizeError> {
        Ok(self.nil())
    }

    /// Go `struct{}{}`: `{}`.
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Out, SanitizeError> {
        self.scalar(San::Map(BTreeMap::new()))
    }

    /// A Go named string type (genai's enums): its string value.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Out, SanitizeError> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        mut self,
        name: &'static str,
        value: &T,
    ) -> Result<Out, SanitizeError> {
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

    /// Divergence (ledger 7b-1): Go has no enum variants with data; newtype, tuple and struct
    /// variants are sanitized as serde_json writes them, `{"variant": value}`.
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
    /// Go `walkList`.
    Seq,
    /// Go `walkMap`.
    Map,
    /// Go `walkStruct` (the serde name of the struct).
    Struct(&'static str),
    /// `json.RawMessage`: Go's `marshaled` path.
    Raw,
    /// An enum tuple variant: a list inside `{"variant": ..}`.
    SeqVariant(&'static str),
    /// An enum struct variant: an object inside `{"variant": ..}`.
    StructVariant(&'static str),
}

/// A container being sanitized (Go `walkList` / `walkMap` / `walkStruct`).
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
    /// Go `seenNames`: struct field names already written or omitted.
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

    /// One element of Go's `walkList` loop.
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

    /// One entry of Go's `walkMap` loop (`key` `Err`: `mapKeyString` failed).
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

    /// The part of Go's `walkMap` / `walkStruct` loops after the key is known: redaction of a
    /// sensitive key (`check_sensitive`), else the walk of the value.
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
        self.map.insert(key, val);
        self.truncated |= t;
    }

    /// One field of Go's `walkStruct` loop.
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
        if GENAI_BYTES_FIELDS.contains(&(name, key)) {
            return self.base64_field(key, value);
        }
        self.named_value(key.to_owned(), value, true);
    }

    /// A gadk-genai `[]byte` field (see [`GENAI_BYTES_FIELDS`]).
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

    /// A field omitted by `skip_serializing_if` (Go: an `omitempty` field, which still meets the
    /// budget check at the top of the loop and claims its name).
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

    /// Go `marshaled` for a `json.RawMessage`: the text decoded with `UseNumber`, then walked at
    /// the same depth.
    ///
    /// Divergence (ledger 7b-1): the text is decoded by `redaction`'s Go `decodeJSONValue` port,
    /// which stops past 100 levels, so a `RawValue` nested deeper is `[UNSUPPORTED_OBJECT]`; Go's
    /// `json.Decoder` accepts up to 10000 levels and the walk then writes `[MAX_DEPTH_EXCEEDED]`.
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
        self.pending_key = Some(key.serialize(KeySer).map_err(|_| ()));
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SanitizeError> {
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

/// Go `mapKeyString(k)`: the JSON object key of a map key — strings, integers, booleans and
/// floats (`'g'` format) as Go writes them, a nil interface as `"null"`; `Err` for any other
/// key (a sequence, map, struct, bytes, ...). Also captures the text of a `RawValue` /
/// `json.Number`.
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

/// A decoded [`JVal`] walked as the Go value `json.Decoder.Decode` with `UseNumber` builds
/// (`nil`, `bool`, `json.Number`, `string`, `[]any`, `map[string]any`).
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
