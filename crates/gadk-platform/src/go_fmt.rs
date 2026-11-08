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

//! Go `fmt` verb ports shared by the gadk crates: `strconv.Quote` (`%q`).

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

#[cfg(test)]
mod tests {
    use super::go_quote;

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
