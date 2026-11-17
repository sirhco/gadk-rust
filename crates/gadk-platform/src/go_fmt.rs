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

//! Text helpers shared by the gadk crates, with exact output formats: double-quoted strings
//! (`%q`), shortest round-trip float formatting (`g` style, 32 or 64 bits), simple one-rune
//! lower/upper casing, query-component escaping and `%v`-style rendering of a JSON value.

use std::sync::LazyLock;

use serde_json::Value;

/// True for printable characters: letters, marks, numbers, punctuation, symbols (categories L,
/// M, N, P, S) and the ASCII space U+0020. Other spaces, Cc, Cf, Cs, Co and Cn are not
/// printable. The categories come from the `regex` crate's Unicode tables, whose Unicode
/// version may differ slightly for newly assigned code points.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static GO_PRINTABLE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[\p{L}\p{M}\p{N}\p{P}\p{S}]$").expect("valid printable-class regex")
});

/// True when `c` is printable (see [`GO_PRINTABLE`]).
fn go_is_print(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    let mut buf = [0u8; 4];
    GO_PRINTABLE.is_match(c.encode_utf8(&mut buf))
}

/// Quotes `s` as a double-quoted string: `"` and `\` are backslash-escaped, printable
/// characters stay raw, `\a \b \f \n \r \t \v` use their short escapes, and every other
/// character becomes `\xNN` (below U+0020 and U+007F), `\uNNNN` or `\UNNNNNNNN`, in lowercase
/// hex. A `&str` cannot hold invalid UTF-8, so the invalid-byte case does not arise
/// ([`go_quote_bytes`] handles it).
#[doc(hidden)]
pub fn go_quote(s: &str) -> String {
    go_quote_bytes(s.as_bytes())
}

/// Quotes a byte string that may hold invalid UTF-8: as [`go_quote`], and each byte that is
/// not part of a valid UTF-8 sequence becomes `\xNN`.
#[doc(hidden)]
pub fn go_quote_bytes(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for chunk in s.utf8_chunks() {
        for c in chunk.valid().chars() {
            push_quoted_rune(&mut out, c);
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out.push('"');
    out
}

/// Appends one valid character as [`go_quote`] writes it.
fn push_quoted_rune(out: &mut String, c: char) {
    match c {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        c if go_is_print(c) => out.push(c),
        '\u{7}' => out.push_str("\\a"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{b}' => out.push_str("\\v"),
        c if (c as u32) < 0x20 || c == '\u{7f}' => {
            out.push_str(&format!("\\x{:02x}", c as u32));
        }
        c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
        c => out.push_str(&format!("\\U{:08x}", c as u32)),
    }
}

/// The simple (one-character) lowercase mapping. Rust's `char::to_lowercase` is the full
/// mapping, which differs only for U+0130 (here `i`, Rust `i` + U+0307).
fn go_lower_rune(c: char) -> char {
    if c == '\u{130}' {
        return 'i';
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// The simple (one-character) uppercase mapping. Where Rust's full mapping expands to several
/// characters (`ß` → `SS`, ligatures, Greek with ypogegrammeni) the character is kept, except
/// the lowercase Greek letters with ypogegrammeni, whose simple uppercase is the matching
/// prosgegrammeni capital.
fn go_upper_rune(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => {
            let shifted = match c as u32 {
                cp @ (0x1F80..=0x1F87 | 0x1F90..=0x1F97 | 0x1FA0..=0x1FA7) => cp + 8,
                cp @ (0x1FB3 | 0x1FC3 | 0x1FF3) => cp + 9,
                _ => return c,
            };
            char::from_u32(shifted).unwrap_or(c)
        }
    }
}

/// Lowercases `s` character by character with `go_lower_rune`, with no final-sigma context.
/// Rust's `str::to_lowercase` differs for U+0130 and a final `Σ` (here `σ`, Rust `ς`).
pub fn go_to_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars().map(go_lower_rune).collect()
}

/// Uppercases `s` character by character with `go_upper_rune`. Rust's `str::to_uppercase`
/// differs wherever the full mapping has several characters (`ß` → `SS`).
pub fn go_to_upper(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    s.chars().map(go_upper_rune).collect()
}

/// Escapes `s` as a URL query component: every byte except ASCII letters, digits, `-`, `.`,
/// `_` and `~` becomes `%XX` (upper-case hex, per UTF-8 byte), and a space becomes `+`.
pub fn go_query_escape(s: &str) -> String {
    const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(UPPER_HEX[usize::from(b >> 4)]));
                out.push(char::from(UPPER_HEX[usize::from(b & 15)]));
            }
        }
    }
    out
}

/// Renders a JSON value as `%v` would print it after decoding into a dynamic value:
/// - `null` → `<nil>`; a bool → `true`/`false`; a string → itself;
/// - every number is printed as a 64-bit float (see [`go_format_float_v`]), so `1000000` →
///   `1e+06`;
/// - an array → `[a b c]`; an object → `map[k1:v1 k2:v2]` with the keys sorted.
pub fn go_sprint_value(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => go_format_float_v(n.as_f64().unwrap_or_default()),
        Value::String(s) => s.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(go_sprint_value)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Value::Object(m) => {
            let mut entries: Vec<(&String, &Value)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            format!(
                "map[{}]",
                entries
                    .iter()
                    .map(|(k, v)| format!("{k}:{}", go_sprint_value(v)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    }
}

/// The type name of a JSON value as a dynamically typed value prints it: `<nil>` for `null`,
/// `bool`, `float64` for every number, `string`, `[]interface {}` for an array and
/// `map[string]interface {}` for an object.
pub fn go_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "<nil>",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float64",
        Value::String(_) => "string",
        Value::Array(_) => "[]interface {}",
        Value::Object(_) => "map[string]interface {}",
    }
}

/// Formats a 64-bit float in `%v` style, the shortest round-trip form (see
/// [`go_format_float_g`]): 1e6 → `1e+06`, 123456 → `123456`, 0.00001 → `1e-05`, -0.0 → `-0`.
pub fn go_format_float_v(f: f64) -> String {
    go_format_float_g(f, 64)
}

/// Formats a float in the shortest round-trip `g` style for `bits` 32 or 64 (any other value
/// is treated as 64; an `f32` is passed widened): `NaN`, `+Inf`, `-Inf`; otherwise the
/// shortest decimal digits that round-trip at that bit size (for 32, of `f as f32`), in `%e`
/// form (`d.ddde±XX`, at least two exponent digits) when the decimal exponent is below -4 or
/// at least 6, otherwise plain decimal.
///
/// Rust's shortest formatter rounds an exact decimal tie up where this function picks the
/// even digit (141016749166520.625 → `1.4101674916652062e+14`), so the digit count comes from
/// `{:e}` and the digits from Rust's exact formatting at that count (ties to even), unless
/// those digits do not round-trip (an asymmetric interval at a power of two), when the
/// shortest ones stand.
pub fn go_format_float_g(f: f64, bits: u32) -> String {
    // With 32 bits the value is first rounded to an f32, so a finite f64 past f32::MAX
    // becomes ±Inf before the NaN/Inf checks.
    let r = if bits == 32 { f64::from(f as f32) } else { f };
    if r.is_nan() {
        return "NaN".to_owned();
    }
    if r.is_infinite() {
        return if r > 0.0 { "+Inf" } else { "-Inf" }.to_owned();
    }
    let sci = if bits == 32 {
        shortest_sci(f as f32)
    } else {
        shortest_sci(f)
    };
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let (neg, mant) = match mant.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mant),
    };
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::with_capacity(digits.len() + 8);
    if neg {
        out.push('-');
    }
    // Exponent form when `exp < -4 || exp >= 6`.
    if !(-4..6).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.unsigned_abs()));
        return out;
    }
    // Plain decimal: the decimal point sits after `dp` digits (`dp` may be ≤ 0 or past the end).
    let dp = exp + 1;
    let nd = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let zeros = |n: i64| "0".repeat(usize::try_from(n).unwrap_or(0));
    if dp <= 0 {
        out.push_str("0.");
        out.push_str(&zeros(-dp));
        out.push_str(&digits);
    } else if dp >= nd {
        out.push_str(&digits);
        out.push_str(&zeros(dp - nd));
    } else {
        let split = usize::try_from(dp).unwrap_or(0);
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    }
    out
}

/// The shortest round-trip digits of `v` as Rust's `{:e}` writes them (`-1.2345e-6`), with an
/// exact decimal tie between two shortest candidates resolved to the even digit.
fn shortest_sci<T>(v: T) -> String
where
    T: std::fmt::LowerExp + std::str::FromStr + PartialEq + Copy,
{
    let shortest = format!("{v:e}");
    let mant = shortest
        .split_once('e')
        .map_or(shortest.as_str(), |(m, _)| m);
    let prec = mant
        .bytes()
        .filter(u8::is_ascii_digit)
        .count()
        .saturating_sub(1);
    let exact = format!("{v:.prec$e}");
    if exact.parse::<T>().is_ok_and(|back| back == v) {
        exact
    } else {
        shortest
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        go_query_escape, go_quote, go_quote_bytes, go_sprint_value, go_to_lower, go_to_upper,
        go_type_name,
    };

    /// Go net/url/url_test.go `queryEscapeTests`, plus non-ASCII and the unreserved marks.
    #[test]
    fn go_query_escape_matches_go() {
        for (input, want) in [
            ("", ""),
            ("abc", "abc"),
            ("one two", "one+two"),
            ("10%", "10%25"),
            (
                " ?&=#+%!<>#\"{}|\\^[]`☺\t:/@$'()*,;",
                "+%3F%26%3D%23%2B%25%21%3C%3E%23%22%7B%7D%7C%5C%5E%5B%5D%60%E2%98%BA%09%3A%2F%40%24%27%28%29%2A%2C%3B",
            ),
            ("my_agent", "my_agent"),
            ("agent-1.v2~x", "agent-1.v2~x"),
            ("Ünï", "%C3%9Cn%C3%AF"),
        ] {
            assert_eq!(go_query_escape(input), want, "QueryEscape({input:?})");
        }
    }

    /// Expected values are Go `strings.ToLower` / `strings.ToUpper` output. (The rune mappings
    /// were checked against Go 1.27's `unicode.ToLower`/`ToUpper` for every code point.)
    #[test]
    fn go_case_mapping_matches_go_strings() {
        assert_eq!(go_to_lower("Alice@Example.TEST"), "alice@example.test");
        // U+0130: Go maps to one `i`; Rust's full mapping gives "i\u{307}".
        assert_eq!(go_to_lower("ASS\u{130}STANT"), "assistant");
        // Final sigma: Go has no context, so both become σ (Rust: "σς").
        assert_eq!(go_to_lower("\u{3a3}\u{3a3}"), "\u{3c3}\u{3c3}");
        assert_eq!(go_to_lower("\u{3a3}"), "\u{3c3}");
        assert_eq!(go_to_lower("\u{212a}"), "k");
        assert_eq!(go_to_upper("auto"), "AUTO");
        // ß has no simple uppercase (Rust: "SS"); ﬁ stays (Rust: "FI").
        assert_eq!(go_to_upper("stra\u{df}e"), "STRA\u{df}E");
        assert_eq!(go_to_upper("\u{fb01}"), "\u{fb01}");
        // Ypogegrammeni: Go maps to the prosgegrammeni capital (Rust: two runes).
        assert_eq!(go_to_upper("\u{1f80}\u{1fb3}"), "\u{1f88}\u{1fbc}");
        assert_eq!(go_to_upper("\u{1c5}"), "\u{1c4}");
        assert_eq!(go_to_upper("\u{131}"), "I");
    }

    /// Expected values are Go `strconv.Quote` output.
    #[test]
    fn go_quote_matches_strconv_quote() {
        let cases = [
            ("plain", "\"plain\""),
            ("q\"b\\", "\"q\\\"b\\\\\""),
            ("\u{7}\u{8}\u{c}\n\r\t\u{b}", "\"\\a\\b\\f\\n\\r\\t\\v\""),
            ("\u{0}\u{1b}\u{7f}", "\"\\x00\\x1b\\x7f\""),
            ("\u{85}", "\"\\u0085\""),
            ("soft\u{ad}hyphen", "\"soft\\u00adhyphen\""),
            ("zero\u{200b}width", "\"zero\\u200bwidth\""),
            ("line\u{2028}sep", "\"line\\u2028sep\""),
            ("nbsp\u{a0}", "\"nbsp\\u00a0\""),
            ("tag\u{e0001}", "\"tag\\U000e0001\""),
            // Printable non-ASCII stays raw: combining mark, accented letter, CJK, emoji.
            (
                "cafe\u{301} \u{e9} \u{4e16} \u{1f600}",
                "\"cafe\u{301} \u{e9} \u{4e16} \u{1f600}\"",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(go_quote(input), want, "go_quote({input:?})");
        }
    }

    /// Expected values are Go `strconv.Quote` output for strings holding invalid UTF-8: each
    /// stray byte is `\xNN`; a valid U+FFFD stays raw.
    #[test]
    fn go_quote_bytes_matches_strconv_quote_on_invalid_utf8() {
        let cases: [(&[u8], &str); 6] = [
            (b"http://x\xffy", "\"http://x\\xffy\""),
            (b"agent\x80", "\"agent\\x80\""),
            (b"\xc3", "\"\\xc3\""),
            (b"\xe2\x82", "\"\\xe2\\x82\""),
            ("caf\u{e9}\u{fffd}".as_bytes(), "\"caf\u{e9}\u{fffd}\""),
            (b"q\"\n\xff", "\"q\\\"\\n\\xff\""),
        ];
        for (input, want) in cases {
            assert_eq!(go_quote_bytes(input), want, "go_quote_bytes({input:?})");
        }
        assert_eq!(go_quote_bytes(b"plain"), go_quote("plain"));
    }

    /// `fmt.Sprintf("%v", v)` after Go `json.Unmarshal(input, &v)` (go1.27 run of the same
    /// inputs). The inputs are parsed by serde_json, as a JSON-decoded value is in Rust.
    #[test]
    fn go_sprint_value_matches_go_for_json_decoded_values() {
        for (input, want) in [
            ("null", "<nil>"),
            ("true", "true"),
            ("false", "false"),
            (r#""ok""#, "ok"),
            (r#""""#, ""),
            ("0", "0"),
            ("-0.0", "-0"),
            ("3", "3"),
            ("1.5", "1.5"),
            ("-2.25", "-2.25"),
            ("100000", "100000"),
            ("123456", "123456"),
            ("1000000", "1e+06"),
            ("1234567", "1.234567e+06"),
            ("0.0001", "0.0001"),
            ("0.00001", "1e-05"),
            ("0.000123", "0.000123"),
            ("1e21", "1e+21"),
            ("1e20", "1e+20"),
            ("123456789012345678", "1.2345678901234568e+17"),
            ("5e-324", "5e-324"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"),
            ("0.1", "0.1"),
            ("-1.5e7", "-1.5e+07"),
            // Exact decimal ties: Go rounds the shortest digits half to even.
            ("141016749166520.625", "1.4101674916652062e+14"),
            ("-783388490710125.25", "-7.833884907101252e+14"),
            ("[]", "[]"),
            (r#"[1,"a",null,true]"#, "[1 a <nil> true]"),
            (r#"[[1,2],{"k":"v"}]"#, "[[1 2] map[k:v]]"),
            ("{}", "map[]"),
            (r#"{"b":2,"a":true,"c":"x y"}"#, "map[a:true b:2 c:x y]"),
            (
                r#"{"z":{"y":[1.5,"s"]},"a":null}"#,
                "map[a:<nil> z:map[y:[1.5 s]]]",
            ),
            (r#"{"result":"ok"}"#, "map[result:ok]"),
        ] {
            let v: Value = serde_json::from_str(input).unwrap();
            assert_eq!(go_sprint_value(&v), want, "%v of {input}");
        }
    }

    #[test]
    fn go_type_name_matches_go_percent_t() {
        for (input, want) in [
            ("null", "<nil>"),
            ("true", "bool"),
            ("1", "float64"),
            (r#""s""#, "string"),
            ("[]", "[]interface {}"),
            ("{}", "map[string]interface {}"),
        ] {
            let v: Value = serde_json::from_str(input).unwrap();
            assert_eq!(go_type_name(&v), want, "%T of {input}");
        }
    }
}
