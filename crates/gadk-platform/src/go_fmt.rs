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

//! Go `fmt` verb and `strings` ports shared by the gadk crates: `strconv.Quote` (`%q`),
//! `strings.ToLower` and `strings.ToUpper`.

use std::sync::LazyLock;

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

#[cfg(test)]
mod tests {
    use super::{go_quote, go_to_lower, go_to_upper};

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
}
