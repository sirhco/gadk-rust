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

//! Go-compatible JSON for the toolauth models (port of ADK-Go `tool/toolauth/model_json.go` and
//! `digest`'s tree).
//!
//! The credential types follow adk-python's pydantic models, which write each field under its
//! camelCase alias, read it under the alias or under the snake_case Python field name, and keep
//! the keys that match no field. The scheme types follow fastapi's OpenAPI models, which read
//! each field under its exact name only. [`decode_object`] and [`ObjectWriter`] give every model
//! that behaviour, with Go's field order and Go's `json.Marshal` bytes.
//!
//! JSON is read straight into raw text, never through `serde_json::Value`, so every number keeps
//! the text it arrived with, as Go's `UseNumber` decode does. [`GoJson`] is Go's `any` after such
//! a decode, and its [`GoJson::encode`] is Go's encoder.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use super::Extra;

/// Go's `any` after a `UseNumber` decode: numbers keep their literal text; object keys sort
/// bytewise (Go map order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoJson {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<GoJson>),
    Object(BTreeMap<String, GoJson>),
}

impl GoJson {
    /// Parses `text`, which must be one JSON value. Syntax is checked by serde_json; the tree is
    /// then built by a Go-faithful scanner (a lone UTF-16 surrogate escape decodes to U+FFFD, as
    /// `encoding/json` decodes it; serde_json would reject it).
    // Task 7's `digest` is the first caller outside the tests.
    #[allow(dead_code)]
    pub(crate) fn parse(text: &str) -> Result<GoJson, serde_json::Error> {
        serde_json::from_str::<serde::de::IgnoredAny>(text)?;
        Ok(Scanner::new(text).value())
    }

    /// Infallible: `RawValue` holds valid JSON.
    pub(crate) fn from_raw(raw: &RawValue) -> GoJson {
        Scanner::new(raw.get()).value()
    }

    /// Compact, sorted keys, no trailing newline. `escape_html = true` is `json.Marshal`
    /// (`<` `>` `&` as six-character `\u00xx` escapes); `false` is an Encoder with
    /// `SetEscapeHTML(false)`. U+2028/U+2029 are always escaped; other non-ASCII is raw.
    pub(crate) fn encode(&self, escape_html: bool) -> String {
        let mut out = String::new();
        self.write(&mut out, escape_html);
        out
    }

    fn write(&self, out: &mut String, escape_html: bool) {
        match self {
            GoJson::Null => out.push_str("null"),
            GoJson::Bool(true) => out.push_str("true"),
            GoJson::Bool(false) => out.push_str("false"),
            GoJson::Number(text) => out.push_str(text),
            GoJson::String(s) => write_string(out, s, escape_html),
            GoJson::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out, escape_html);
                }
                out.push(']');
            }
            GoJson::Object(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, key, escape_html);
                    out.push(':');
                    value.write(out, escape_html);
                }
                out.push('}');
            }
        }
    }
}

/// Go `encodeState.string` (Go 1.22+): `"` `\` and the five short escapes `\b \f \n \r \t`, other
/// control characters as `\u00xx` (lowercase hex), `<` `>` `&` as `\u00xx` when `escape_html`,
/// U+2028 and U+2029 always escaped, everything else written raw.
fn write_string(out: &mut String, s: &str, escape_html: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' if escape_html => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A scanner over text that serde_json has already accepted as one JSON value. It never panics
/// and always terminates on other input too: it stops at the first byte that does not fit.
struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Scanner { text, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> GoJson {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => GoJson::Object(
                self.entries()
                    .into_iter()
                    .map(|(key, value, _)| (key, value))
                    .collect(),
            ),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(b',') => self.pos += 1,
                        Some(_) => {
                            let before = self.pos;
                            items.push(self.value());
                            if self.pos == before {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                GoJson::Array(items)
            }
            Some(b'"') => GoJson::String(self.string()),
            Some(b't') => {
                self.pos += 4;
                GoJson::Bool(true)
            }
            Some(b'f') => {
                self.pos += 5;
                GoJson::Bool(false)
            }
            Some(b'n') => {
                self.pos += 4;
                GoJson::Null
            }
            Some(_) => {
                let start = self.pos;
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.pos += 1;
                }
                GoJson::Number(self.text.get(start..self.pos).unwrap_or("").to_owned())
            }
            None => GoJson::Null,
        }
    }

    /// The members of the object at `pos`, in input order: each key (Go-unquoted), its value and
    /// the byte span of the value's text. Leaves `pos` after the closing brace. Each value is
    /// scanned once, so the cost is linear in the text.
    fn entries(&mut self) -> Vec<(String, GoJson, (usize, usize))> {
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() != Some(b'{') {
            return out;
        }
        self.pos += 1;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'"') => {
                    let key = self.string();
                    self.skip_ws();
                    if self.peek() != Some(b':') {
                        break;
                    }
                    self.pos += 1;
                    self.skip_ws();
                    let start = self.pos;
                    let value = self.value();
                    out.push((key, value, (start, self.pos)));
                }
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        out
    }

    /// The string literal at `pos`, unquoted as Go's `unquote` does. Leaves `pos` after the
    /// closing quote.
    fn string(&mut self) -> String {
        let bytes = self.text.as_bytes();
        self.pos += 1;
        let start = self.pos;
        while let Some(&b) = bytes.get(self.pos) {
            match b {
                b'"' => break,
                b'\\' => self.pos += 2,
                _ => self.pos += 1,
            }
        }
        let end = self.pos.min(bytes.len());
        self.pos = (self.pos + 1).min(bytes.len());
        unquote(self.text.get(start..end).unwrap_or(""))
    }
}

/// Go `encoding/json` `unquote` of a string literal's body. A `\u` escape of a UTF-16 surrogate
/// that is not the first half of a valid pair becomes U+FFFD.
fn unquote(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        out.push_str(body.get(copied..i).unwrap_or(""));
        let escape = bytes.get(i + 1).copied();
        i += 2;
        match escape {
            Some(b'"') => out.push('"'),
            Some(b'\\') => out.push('\\'),
            Some(b'/') => out.push('/'),
            Some(b'b') => out.push('\u{8}'),
            Some(b'f') => out.push('\u{c}'),
            Some(b'n') => out.push('\n'),
            Some(b'r') => out.push('\r'),
            Some(b't') => out.push('\t'),
            Some(b'u') => {
                let Some(first) = hex4(body, i) else {
                    copied = i.min(bytes.len());
                    continue;
                };
                i += 4;
                let decoded = if (0xD800..0xDC00).contains(&first)
                    && bytes.get(i) == Some(&b'\\')
                    && bytes.get(i + 1) == Some(&b'u')
                {
                    match hex4(body, i + 2) {
                        Some(second) if (0xDC00..0xE000).contains(&second) => {
                            i += 6;
                            char::from_u32(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00))
                        }
                        _ => None,
                    }
                } else {
                    char::from_u32(first)
                };
                out.push(decoded.unwrap_or('\u{FFFD}'));
            }
            _ => {}
        }
        copied = i.min(bytes.len());
    }
    out.push_str(body.get(copied..).unwrap_or(""));
    out
}

fn hex4(body: &str, at: usize) -> Option<u32> {
    let digits = body.get(at..at + 4)?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// The JSON kind of `text` as Go's `UnmarshalTypeError.Value` names it, with the literal cut
/// off: "object", "array", "string", "bool", "null" or "number".
fn json_kind(text: &str) -> &'static str {
    match text.trim_start().as_bytes().first() {
        Some(b'{') => "object",
        Some(b'[') => "array",
        Some(b'"') => "string",
        Some(b't' | b'f') => "bool",
        Some(b'n') => "null",
        _ => "number",
    }
}

/// Go `cmp` equality of two `Extra` maps (each value compared as `GoJson`).
pub(crate) fn extra_eq(a: &Extra, b: &Extra) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && GoJson::from_raw(va) == GoJson::from_raw(vb))
}

/// Go `json.Marshal` of one string (HTML-escaped).
pub(crate) fn go_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_string(&mut out, s, true);
    out
}

/// Go `json.Marshal` of a `[]string` that is not nil.
pub(crate) fn go_string_list(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(&mut out, item, true);
    }
    out.push(']');
    out
}

/// Go `json.Marshal` of a `map[string]string` that is not nil: keys sorted bytewise.
pub(crate) fn go_string_map(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (i, (key, value)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(&mut out, key, true);
        out.push(':');
        write_string(&mut out, value, true);
    }
    out.push('}');
    out
}

/// Go `pythonName` (model_json.go): "type" → "type_", camelCase → snake_case.
pub(crate) fn python_name(alias: &str) -> String {
    if alias == "type" {
        return "type_".to_owned();
    }
    let mut out = String::with_capacity(alias.len() + 4);
    for c in alias.chars() {
        if c.is_uppercase() {
            out.push('_');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Go `decode` (model_json.go): one JSON object split into known fields (by Go alias, the
/// camelCase spelling winning over its Python name) and extras. `Err` is Go's message text.
pub(crate) struct Decoded {
    pub(crate) known: BTreeMap<String, Box<RawValue>>,
    pub(crate) extra: Extra,
}

/// Go `decode`: `aliases` are the model's json tag names in struct order; with `python_names`
/// (Go `decodeModel`) a key that is a field's Python name is read as that field unless the
/// object also has the alias, in which case it is dropped; without it (Go `decodeOpenAPIModel`)
/// such a key is an extra. Each name in `required` must be present and not null. A JSON `null`
/// reads as an empty object, as Go's `json.Unmarshal` into a map leaves it.
pub(crate) fn decode_object(
    raw: &RawValue,
    type_name: &str,
    aliases: &[&str],
    python_names: bool,
    required: &[&str],
) -> Result<Decoded, String> {
    let text = raw.get();
    let mut members: BTreeMap<String, &str> = BTreeMap::new();
    match json_kind(text) {
        "object" => {
            let mut scanner = Scanner::new(text);
            for (key, _, (start, end)) in scanner.entries() {
                members.insert(key, text.get(start..end).unwrap_or("null"));
            }
        }
        "null" => {}
        kind => {
            return Err(format!(
                "toolauth: {type_name}: want a JSON object, got {kind}"
            ));
        }
    }
    let mut known = BTreeMap::new();
    let mut extra = Extra::new();
    for (key, value) in &members {
        let value = RawValue::from_string((*value).to_owned())
            .map_err(|_| format!("toolauth: {type_name}: malformed JSON"))?;
        if aliases.contains(&key.as_str()) {
            known.insert(key.clone(), value);
            continue;
        }
        let field = if python_names {
            aliases
                .iter()
                .find(|alias| python_name(alias) == *key && **alias != key.as_str())
        } else {
            None
        };
        match field {
            Some(alias) if members.contains_key(*alias) => {}
            Some(alias) => {
                known.insert((*alias).to_owned(), value);
            }
            None => {
                extra.insert(key.clone(), value);
            }
        }
    }
    for name in required {
        match known.get(*name) {
            Some(value) if value.get().trim() != "null" => {}
            _ => {
                return Err(format!(
                    "toolauth: {type_name}: missing required field {name}"
                ));
            }
        }
    }
    Ok(Decoded { known, extra })
}

impl Decoded {
    /// Decode one known field; a type mismatch is Go's
    /// "toolauth: <type_name>: field <alias>: cannot use a JSON <kind> as <go_type>".
    ///
    /// An absent field and a JSON `null` are both `Ok(None)`, as Go leaves the field at its zero
    /// value. A nested model's own error (any text starting `toolauth: `) is passed through, as
    /// Go's `encoding/json` passes an `UnmarshalJSON` error through.
    pub(crate) fn take<T: DeserializeOwned>(
        &mut self,
        alias: &str,
        type_name: &str,
        go_type: &str,
    ) -> Result<Option<T>, String> {
        let Some(raw) = self.known.remove(alias) else {
            return Ok(None);
        };
        let kind = json_kind(raw.get());
        if kind == "null" {
            return Ok(None);
        }
        // serde_json rejects a lone surrogate escape that Go decodes to U+FFFD, so text with a
        // surrogate escape is first re-encoded through the Go-faithful tree.
        let normalized;
        let text = if raw.get().contains("\\ud") || raw.get().contains("\\uD") {
            normalized = GoJson::from_raw(&raw).encode(false);
            normalized.as_str()
        } else {
            raw.get()
        };
        serde_json::from_str::<T>(text).map(Some).map_err(|e| {
            let msg = e.to_string();
            let suffix = format!(" at line {} column {}", e.line(), e.column());
            let msg = msg.strip_suffix(&suffix).unwrap_or(&msg);
            if msg.starts_with("toolauth: ") {
                msg.to_owned()
            } else {
                format!(
                    "toolauth: {type_name}: field {alias}: cannot use a JSON {kind} as {go_type}"
                )
            }
        })
    }
}

/// Go `encode` (model_json.go): known fields in Go struct order, then extras sorted, skipping an
/// extra whose key is a field alias (or, with `python_names`, a field's Python name).
pub(crate) struct ObjectWriter {
    buf: String,
}

impl ObjectWriter {
    pub(crate) fn new() -> Self {
        ObjectWriter {
            buf: String::from("{"),
        }
    }

    /// Append `"alias":<json>`; `json` is already Go-encoded.
    pub(crate) fn field(&mut self, alias: &str, json: &str) {
        if self.buf.len() > 1 {
            self.buf.push(',');
        }
        write_string(&mut self.buf, alias, true);
        self.buf.push(':');
        self.buf.push_str(json);
    }

    /// Appends each extra, re-encoded as Go's `json.Marshal` writes the decoded value, unless its
    /// key names a field, and closes the object.
    pub(crate) fn finish(mut self, extra: &Extra, aliases: &[&str], python_names: bool) -> String {
        for (key, value) in extra {
            let names_field = aliases.contains(&key.as_str())
                || (python_names && aliases.iter().any(|alias| python_name(alias) == *key));
            if names_field {
                continue;
            }
            let json = GoJson::from_raw(value).encode(true);
            self.field(key, &json);
        }
        self.buf.push('}');
        self.buf
    }
}

/// Every model's Go `MarshalJSON` bytes (HTML-escaped, Go field order, extras last).
pub(crate) trait GoMarshal {
    fn go_marshal(&self) -> String;
}

/// `Serialize` body shared by every model: emits `go_marshal()` as a `RawValue`, so a
/// serde_json serializer writes Go's bytes verbatim.
pub(crate) fn serialize_go<T: GoMarshal, S: serde::Serializer>(
    v: &T,
    s: S,
) -> Result<S::Ok, S::Error> {
    let raw = RawValue::from_string(v.go_marshal()).map_err(serde::ser::Error::custom)?;
    serde::Serialize::serialize(&raw, s)
}

/// Every model's Go `UnmarshalJSON`: decode one JSON value, `Err` being Go's message text.
pub(crate) trait GoUnmarshal: Sized {
    fn go_unmarshal(raw: &RawValue) -> Result<Self, String>;
}

/// `Deserialize` body shared by every model: reads the value as raw text (no
/// `serde_json::Value` step, so numbers keep their text) and decodes it with
/// [`GoUnmarshal::go_unmarshal`]. Works with serde_json deserializers, which support `RawValue`.
pub(crate) fn deserialize_go<'de, T: GoUnmarshal, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<T, D::Error> {
    let raw = <Box<RawValue> as serde::Deserialize>::deserialize(d)?;
    T::go_unmarshal(&raw).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).unwrap()
    }

    #[test]
    fn encode_escapes_html_only_when_asked() {
        let v = GoJson::String("<a href=\"x\">&amp;</a>".to_owned());
        assert_eq!(
            v.encode(true),
            "\"\\u003ca href=\\\"x\\\"\\u003e\\u0026amp;\\u003c/a\\u003e\""
        );
        assert_eq!(v.encode(false), r#""<a href=\"x\">&amp;</a>""#);
    }

    #[test]
    fn encode_writes_go_string_escapes() {
        let v = GoJson::String("\u{8}\u{c}\n\r\t\u{1}\u{1f}\u{7f}é\u{2028}\u{2029}\\/".to_owned());
        let want = "\"\\b\\f\\n\\r\\t\\u0001\\u001f\u{7f}é\\u2028\\u2029\\\\/\"";
        assert_eq!(v.encode(true), want);
        assert_eq!(v.encode(false), want);
    }

    #[test]
    fn parse_keeps_number_text_and_sorts_keys() {
        let v = GoJson::parse(
            r#" { "b" : [1.0, 1e5, -0, 12345678901234567890123], "a":{"z":null,"y":true} } "#,
        )
        .unwrap();
        assert_eq!(
            v.encode(true),
            r#"{"a":{"y":true,"z":null},"b":[1.0,1e5,-0,12345678901234567890123]}"#
        );
    }

    #[test]
    fn parse_keeps_the_last_duplicate_key() {
        assert_eq!(
            GoJson::parse(r#"{"a":1,"a":2}"#).unwrap().encode(true),
            r#"{"a":2}"#
        );
    }

    #[test]
    fn parse_unescapes_as_go_does() {
        let v = GoJson::parse("\"\\u0041\\u00e9\\ud83d\\ude00\\ud800x\\udc00\\ud800\\u0041\\/\"")
            .unwrap();
        assert_eq!(
            v,
            GoJson::String("Aé😀\u{FFFD}x\u{FFFD}\u{FFFD}A/".to_owned())
        );
    }

    #[test]
    fn parse_rejects_invalid_json() {
        assert!(GoJson::parse("{").is_err());
        assert!(GoJson::parse("1 2").is_err());
    }

    #[test]
    fn python_name_matches_go() {
        for (alias, want) in [
            ("type", "type_"),
            ("clientId", "client_id"),
            ("authProviderX509CertUrl", "auth_provider_x509_cert_url"),
            ("useIdToken", "use_id_token"),
            ("issuer_url", "issuer_url"),
            ("scopes", "scopes"),
        ] {
            assert_eq!(python_name(alias), want, "{alias}");
        }
    }

    #[test]
    fn decode_object_splits_known_and_extra() {
        let r = raw(r#"{"clientId":"c","client_secret":"s","client_id":"dropped","x":1}"#);
        let d = decode_object(&r, "T", &["clientId", "clientSecret"], true, &[]).unwrap();
        let known: Vec<(&str, &str)> = d.known.iter().map(|(k, v)| (k.as_str(), v.get())).collect();
        assert_eq!(known, [("clientId", "\"c\""), ("clientSecret", "\"s\"")]);
        let extra: Vec<(&str, &str)> = d.extra.iter().map(|(k, v)| (k.as_str(), v.get())).collect();
        assert_eq!(extra, [("x", "1")]);
    }

    #[test]
    fn decode_object_without_python_names_keeps_snake_case_as_extra() {
        let r = raw(r#"{"token_url":"t"}"#);
        let d = decode_object(&r, "T", &["tokenUrl"], false, &[]).unwrap();
        assert!(d.known.is_empty());
        assert_eq!(d.extra["token_url"].get(), "\"t\"");
    }

    #[test]
    fn decode_object_reads_null_as_empty_and_rejects_other_kinds() {
        let d = decode_object(&raw("null"), "T", &["a"], true, &[]).unwrap();
        assert!(d.known.is_empty() && d.extra.is_empty());
        assert_eq!(
            decode_object(&raw("null"), "T", &["a"], true, &["a"])
                .err()
                .unwrap(),
            "toolauth: T: missing required field a"
        );
        for (text, kind) in [
            ("\"s\"", "string"),
            ("[1]", "array"),
            ("12", "number"),
            ("true", "bool"),
        ] {
            assert_eq!(
                decode_object(&raw(text), "T", &[], true, &[])
                    .err()
                    .unwrap(),
                format!("toolauth: T: want a JSON object, got {kind}")
            );
        }
    }

    #[test]
    fn take_reports_go_type_errors_without_the_value() {
        let r = raw(r#"{"n":98765432109876543210,"s":["secret"],"z":null}"#);
        let mut d = decode_object(&r, "T", &["n", "s", "z"], true, &[]).unwrap();
        assert_eq!(
            d.take::<i64>("n", "T", "int64").err().unwrap(),
            "toolauth: T: field n: cannot use a JSON number as int64"
        );
        assert_eq!(
            d.take::<String>("s", "T", "string").err().unwrap(),
            "toolauth: T: field s: cannot use a JSON array as string"
        );
        assert_eq!(d.take::<String>("z", "T", "string").unwrap(), None);
        assert_eq!(d.take::<String>("missing", "T", "string").unwrap(), None);
    }

    #[test]
    fn take_decodes_a_lone_surrogate_as_go_does() {
        let r = raw("{\"s\":\"a\\ud800b\"}");
        let mut d = decode_object(&r, "T", &["s"], true, &[]).unwrap();
        assert_eq!(
            d.take::<String>("s", "T", "string").unwrap().unwrap(),
            "a\u{FFFD}b"
        );
    }

    #[test]
    fn writer_skips_extras_that_name_fields() {
        let mut extra = Extra::new();
        extra.insert("clientId".to_owned(), raw("\"x\""));
        extra.insert("client_id".to_owned(), raw("\"y\""));
        extra.insert("z".to_owned(), raw(r#"{"b":"<","a":1.50}"#));
        let mut w = ObjectWriter::new();
        w.field("clientId", &go_string("c"));
        assert_eq!(
            w.finish(&extra, &["clientId"], true),
            "{\"clientId\":\"c\",\"z\":{\"a\":1.50,\"b\":\"\\u003c\"}}"
        );
        let mut w = ObjectWriter::new();
        w.field("clientId", &go_string("c"));
        assert_eq!(
            w.finish(&extra, &["clientId"], false),
            "{\"clientId\":\"c\",\"client_id\":\"y\",\"z\":{\"a\":1.50,\"b\":\"\\u003c\"}}"
        );
        assert_eq!(ObjectWriter::new().finish(&Extra::new(), &[], true), "{}");
    }
}
