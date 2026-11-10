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

//! Go `fmt` verb, `strings` and `net/url` ports shared by the gadk crates: `strconv.Quote`
//! (`%q`), `strings.ToLower`, `strings.ToUpper`, `url.QueryEscape` and `fmt` `%v` of a
//! JSON-decoded value.

use std::sync::LazyLock;

use serde_json::Value;

/// Go `unicode.IsPrint` (= `strconv.IsPrint`): letters, marks, numbers, punctuation, symbols
/// (categories L, M, N, P, S) and the ASCII space U+0020. Other spaces, Cc, Cf, Cs, Co and Cn
/// are not printable. The categories come from the `regex` crate's Unicode tables, whose Unicode
/// version may differ slightly from Go's for newly assigned code points.
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static GO_PRINTABLE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[\p{L}\p{M}\p{N}\p{P}\p{S}]$").expect("valid printable-class regex")
});

/// Port of Go `unicode.IsPrint`.
fn go_is_print(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    let mut buf = [0u8; 4];
    GO_PRINTABLE.is_match(c.encode_utf8(&mut buf))
}

/// Port of Go `strconv.Quote` (Go `%q` on a string): `"` and `\` are backslash-escaped,
/// printable runes (Go `unicode.IsPrint`) stay raw, `\a \b \f \n \r \t \v` use their short
/// escapes, and every other rune becomes `\xNN` (below U+0020 and U+007F), `\uNNNN` or
/// `\UNNNNNNNN`, in lowercase hex. A `&str` cannot hold invalid UTF-8, so Go's invalid-byte
/// branch does not arise.
#[doc(hidden)]
pub fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
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
    out.push('"');
    out
}

/// Go `unicode.ToLower`: the simple (one-rune) lowercase mapping. Rust's `char::to_lowercase`
/// is the full mapping, which differs only for U+0130 (Go `i`, Rust `i` + U+0307).
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

/// Go `unicode.ToUpper`: the simple (one-rune) uppercase mapping. Where Rust's full mapping
/// expands to several runes (`ß` → `SS`, ligatures, Greek with ypogegrammeni) Go keeps the rune,
/// except the lowercase Greek letters with ypogegrammeni, whose simple uppercase is the
/// matching prosgegrammeni capital.
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

/// Port of Go `strings.ToLower` on valid UTF-8: `unicode.ToLower` rune by rune,
/// with no final-sigma context. Rust's `str::to_lowercase` differs for U+0130 and a final `Σ`
/// (Go `σ`, Rust `ς`).
pub fn go_to_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars().map(go_lower_rune).collect()
}

/// Port of Go `strings.ToUpper` on valid UTF-8: `unicode.ToUpper` rune by rune.
/// Rust's `str::to_uppercase` differs wherever the full mapping has several runes (`ß` → `SS`).
pub fn go_to_upper(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    s.chars().map(go_upper_rune).collect()
}

/// Port of Go `url.QueryEscape` (net/url/url.go `escape(s, encodeQueryComponent)`): every byte
/// except ASCII letters, digits, `-`, `.`, `_` and `~` becomes `%XX` (upper-case hex, per UTF-8
/// byte), and a space becomes `+`.
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

/// Go `fmt.Sprintf("%v", v)` of a value Go decoded from JSON (`any`):
/// - `null` → `<nil>`; a bool → `true`/`false`; a string → itself;
/// - every number is a Go `float64`, printed as `%v` does (see `go_format_float_v`), so
///   `1000000` → `1e+06`;
/// - an array → `[a b c]`; an object → `map[k1:v1 k2:v2]` with the keys sorted (Go `fmt` sorts
///   map keys).
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

/// Go `%v` of a float64 = `strconv.FormatFloat(f, 'g', -1, 64)`: the shortest round-trip digits,
/// in `%e` form (`d.ddde±XX`, at least two exponent digits) when the decimal exponent is below -4
/// or at least 6 (strconv ftoa.go: `if shortest { eprec = 6 }`), otherwise plain decimal. So
/// 1e6 → `1e+06`, 123456 → `123456`, 0.00001 → `1e-05`, -0.0 → `-0`.
///
/// Rust's shortest formatters round an exact decimal tie up, while Go rounds it half to even
/// (141016749166520.625 → Go `1.4101674916652062e+14`). So only the shortest digit count is taken
/// from `{f:e}`; the digits themselves come from Rust's exact-precision formatting at that count,
/// which rounds ties to even as Go does.
fn go_format_float_v(f: f64) -> String {
    let shortest = format!("{f:e}"); // e.g. "1.234567e6", "-0e0"
    let shortest_mant = shortest
        .split_once('e')
        .map_or(shortest.as_str(), |(m, _)| m);
    let digits = shortest_mant.bytes().filter(u8::is_ascii_digit).count();
    let prec = digits.saturating_sub(1);
    let sci = format!("{f:.prec$e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        // Plain decimal with the same significant digits: `prec - exp` fraction digits.
        let frac =
            usize::try_from(i64::try_from(prec).unwrap_or(i64::MAX) - i64::from(exp)).unwrap_or(0);
        format!("{f:.frac$}")
    } else {
        format!(
            "{mant}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{go_query_escape, go_quote, go_sprint_value, go_to_lower, go_to_upper};

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
}
