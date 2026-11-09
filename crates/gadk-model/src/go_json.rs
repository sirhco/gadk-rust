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

//! Go `encoding/json` output for serde values (moved from gadk-anthropic's `wire.rs` in 3b so
//! every REST provider shares it): [`to_go_json`].

use std::io;

use serde::Serialize;

/// A `serde_json` formatter that writes what Go's `encoding/json` writes for the same value:
/// `<`, `>`, `&`, U+2028 and U+2029 in strings are escape sequences (`\u003c`, `\u003e`,
/// `\u0026`, `\u2028`, `\u2029`); floats use Go's format (`0` not `0.0`, `1e-7`, `1e+21`).
struct GoFormatter;

/// Go `encoding/json` `floatEncoder`: `'f'` format with the shortest representation, `'e'`
/// below 1e-6 or from 1e21 on, with a two-digit negative exponent's leading zero removed.
fn go_float(value: f64, shortest: impl Fn() -> String, exponent: impl Fn() -> String) -> String {
    let abs = value.abs();
    if abs == 0.0 || (1e-6..1e21).contains(&abs) {
        return shortest();
    }
    let e = exponent();
    match e.split_once('e') {
        Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp:0>2}"),
        _ => e,
    }
}

impl serde_json::ser::Formatter for GoFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        let s = go_float(value, || format!("{value}"), || format!("{value:e}"));
        writer.write_all(s.as_bytes())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        let s = go_float(
            f64::from(value),
            || format!("{value}"),
            || format!("{value:e}"),
        );
        writer.write_all(s.as_bytes())
    }

    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut start = 0;
        for (i, c) in fragment.char_indices() {
            let esc = match c {
                '<' => "\\u003c",
                '>' => "\\u003e",
                '&' => "\\u0026",
                '\u{2028}' => "\\u2028",
                '\u{2029}' => "\\u2029",
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..i])?;
            writer.write_all(esc.as_bytes())?;
            start = i + c.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }
}

/// Serializes `value` as Go's `json.Marshal` would: compact, HTML-escaped strings, Go float
/// formatting, and map keys sorted (as `serde_json::Map` is without `preserve_order`).
pub fn to_go_json<T: ?Sized + Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, GoFormatter);
    value.serialize(&mut ser)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// Go `encoding/json` float output (`strconv.AppendFloat(…, 'f' or 'e', -1, 64)` plus the
    /// exponent clean-up) and f32 values widened as Go's `float64(*cfg.Temperature)` does.
    #[test]
    fn floats_use_gos_format() {
        let cases: [(f64, &str); 12] = [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (-2.5e-10, "-2.5e-10"),
            (123456.789, "123456.789"),
            (f64::from(0.7_f32), "0.699999988079071"),
        ];
        for (v, want) in cases {
            assert_eq!(
                String::from_utf8(to_go_json(&v).unwrap()).unwrap(),
                want,
                "{v:e}"
            );
        }
        assert_eq!(
            String::from_utf8(to_go_json(&0.1_f32).unwrap()).unwrap(),
            "0.1"
        );
        assert_eq!(
            String::from_utf8(to_go_json(&json!({"n": 2.0, "i": 2})).unwrap()).unwrap(),
            r#"{"i":2,"n":2}"#
        );
    }

    /// Go's `json.Marshal` escapes HTML-significant characters and the JavaScript line
    /// separators; control characters use the same escapes as serde_json.
    #[test]
    fn strings_use_gos_escaping() {
        let s = "a<b>&c\u{2028}\u{2029}\"\\\n\t\u{1}\u{8}\u{c}é";
        assert_eq!(
            String::from_utf8(to_go_json(s).unwrap()).unwrap(),
            "\"a\\u003cb\\u003e\\u0026c\\u2028\\u2029\\\"\\\\\\n\\t\\u0001\\b\\fé\""
        );
        assert_eq!(
            String::from_utf8(to_go_json(&json!({"<k>": "&"})).unwrap()).unwrap(),
            r#"{"\u003ck\u003e":"\u0026"}"#
        );
    }
}
