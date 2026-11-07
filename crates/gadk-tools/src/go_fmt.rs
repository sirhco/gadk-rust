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

//! Go standard-library formatting and path helpers the tools reproduce byte-for-byte: `net/url`
//! `QueryEscape`, `html` `EscapeString`, `path` `Clean`, `io/fs` `ValidPath`, `encoding/json` of a
//! Go `string(bytes)`, and `fmt` `%v` of a JSON-decoded value; plus the crate's one
//! `fmt.Errorf("<msg>: %w", err)` wrapper, [`wrap_err`]. Private to the crate (the engine's and
//! gadk-session's copies are private to those crates).

use gadk_agent::ToolError;
use serde_json::Value;

/// Go `url.QueryEscape` (net/url/url.go `escape(s, encodeQueryComponent)`): every byte except
/// ASCII letters, digits, `-`, `.`, `_` and `~` becomes `%XX` (upper-case hex, per UTF-8 byte), and
/// a space becomes `+`.
pub(crate) fn url_query_escape(s: &str) -> String {
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

/// Go `html.EscapeString`: `&`, `'`, `<`, `>` and `"` become `&amp;`, `&#39;`, `&lt;`, `&gt;` and
/// `&#34;`; every other character is kept.
pub(crate) fn html_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            other => out.push(other),
        }
    }
    out
}

/// Go `path.Clean` (lexical, slash-separated): drops empty and `.` elements, resolves `..`
/// against the preceding element, keeps leading `..` elements of a relative path, drops `..`
/// at the root of a rooted path, and returns `.` for an empty result (`/` when rooted).
pub(crate) fn path_clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_owned();
    }
    let rooted = p.starts_with('/');
    let mut elems: Vec<&str> = Vec::new();
    for elem in p.split('/') {
        match elem {
            "" | "." => {}
            ".." => {
                if elems.last().is_some_and(|last| *last != "..") {
                    elems.pop();
                } else if !rooted {
                    elems.push("..");
                }
            }
            real => elems.push(real),
        }
    }
    let joined = elems.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// Go `fs.ValidPath`: an unrooted, slash-separated path whose elements are all non-empty and
/// neither `.` nor `..`; the single path `.` is also valid. Go also rejects invalid UTF-8, which
/// a Rust `&str` cannot hold.
pub(crate) fn valid_path(p: &str) -> bool {
    p == "." || p.split('/').all(|e| !matches!(e, "" | "." | ".."))
}

/// Go `fmt.Errorf("<msg>: %w", source)`: Display `"<msg>: <source>"`, `source()` = the wrapped
/// error.
#[derive(Debug)]
struct GoWrapped {
    msg: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for GoWrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl std::error::Error for GoWrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// A [`ToolError::Execution`] wrapping `source` as Go's `fmt.Errorf("<msg>: %w", source)` does.
pub(crate) fn wrap_err(
    msg: impl Into<String>,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ToolError {
    ToolError::Execution(Box::new(GoWrapped {
        msg: msg.into(),
        source: source.into(),
    }))
}

/// Go `string(bytes)` as `encoding/json` encodes it: valid UTF-8 is copied, and each byte that
/// `utf8.DecodeRune` rejects (size 1) becomes one U+FFFD. Unlike [`String::from_utf8_lossy`],
/// which emits one U+FFFD per maximal invalid subpart, this emits one per invalid byte.
pub(crate) fn go_lossy_utf8(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                out.push_str(valid);
                return out;
            }
            Err(e) => {
                let (valid, rest) = bytes.split_at(e.valid_up_to());
                // `valid` is valid UTF-8 by `valid_up_to`'s contract.
                out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                // `None`: a truncated sequence at the end; every remaining byte is invalid.
                let invalid = e.error_len().unwrap_or(rest.len());
                out.extend(std::iter::repeat_n('\u{FFFD}', invalid));
                bytes = &rest[invalid..];
            }
        }
    }
}

/// Go `fmt.Sprintf("%v", v)` of a value Go decoded from JSON (`any`):
/// - `null` → `<nil>`; a bool → `true`/`false`; a string → itself;
/// - every number is a Go `float64`, printed as `%v` does (see [`go_format_float_v`]), so
///   `1000000` → `1e+06`;
/// - an array → `[a b c]`; an object → `map[k1:v1 k2:v2]` with the keys sorted (Go `fmt` sorts
///   map keys).
pub(crate) fn go_sprint_value(v: &Value) -> String {
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    /// Go net/url/url_test.go `queryEscapeTests`, plus non-ASCII and the unreserved marks.
    #[test]
    fn url_query_escape_matches_go() {
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
            assert_eq!(url_query_escape(input), want, "QueryEscape({input:?})");
        }
    }

    #[test]
    fn wrap_err_keeps_go_text_and_source() {
        let err = wrap_err("failed to search memory", std::io::Error::other("boom"));
        assert_eq!(err.to_string(), "failed to search memory: boom");
        let src = std::error::Error::source(&err).expect("wrapped source");
        assert_eq!(src.to_string(), "boom");
    }

    /// A wrapped `ToolError` stays reachable as itself through `source()` (Go `%w`).
    #[test]
    fn wrap_err_of_tool_error_sources_the_tool_error() {
        let err = wrap_err(
            "toolsearch: list base tools",
            ToolError::Panic("boom".into()),
        );
        assert_eq!(err.to_string(), "toolsearch: list base tools: boom");
        let src = std::error::Error::source(&err).expect("wrapped source");
        assert!(matches!(
            src.downcast_ref::<ToolError>(),
            Some(ToolError::Panic(m)) if m == "boom"
        ));
    }

    /// Go `string(bytes)` through `encoding/json`: one U+FFFD per byte `utf8.DecodeRune`
    /// rejects.
    #[test]
    fn go_lossy_utf8_replaces_each_invalid_byte() {
        for (input, want) in [
            (&b""[..], ""),
            (b"plain \xE2\x82\xAC", "plain \u{20AC}"),
            (b"\xE2\x82A", "\u{FFFD}\u{FFFD}A"),
            (b"\xF0\x9F\x98", "\u{FFFD}\u{FFFD}\u{FFFD}"),
            (b"a\xFFb\xC0\xAF", "a\u{FFFD}b\u{FFFD}\u{FFFD}"),
        ] {
            assert_eq!(go_lossy_utf8(input), want, "{input:?}");
        }
    }

    /// Go `html.EscapeString` outputs (go1.27 run of the same inputs).
    #[test]
    fn html_escape_string_matches_go() {
        for (input, want) in [
            ("", ""),
            ("plain", "plain"),
            ("a&b<c>d'e\"f", "a&amp;b&lt;c&gt;d&#39;e&#34;f"),
            ("&amp;", "&amp;amp;"),
            ("<skill>", "&lt;skill&gt;"),
            ("héllo ☺", "héllo ☺"),
        ] {
            assert_eq!(html_escape_string(input), want, "EscapeString({input:?})");
        }
    }

    /// Go path/path_test.go `cleantests`.
    #[test]
    fn path_clean_matches_go() {
        for (input, want) in [
            // Already clean
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            (".", "."),
            ("..", ".."),
            ("../..", "../.."),
            ("../../abc", "../../abc"),
            ("/abc", "/abc"),
            ("/", "/"),
            // Remove trailing slash
            ("abc/", "abc"),
            ("abc/def/", "abc/def"),
            ("a/b/c/", "a/b/c"),
            ("./", "."),
            ("../", ".."),
            ("../../", "../.."),
            ("/abc/", "/abc"),
            // Remove doubled slash
            ("abc//def//ghi", "abc/def/ghi"),
            ("//abc", "/abc"),
            ("///abc", "/abc"),
            ("//abc//", "/abc"),
            ("abc//", "abc"),
            // Remove . elements
            ("abc/./def", "abc/def"),
            ("/./abc/def", "/abc/def"),
            ("abc/.", "abc"),
            // Remove .. elements
            ("abc/def/ghi/../jkl", "abc/def/jkl"),
            ("abc/def/../ghi/../jkl", "abc/jkl"),
            ("abc/def/..", "abc"),
            ("abc/def/../..", "."),
            ("/abc/def/../..", "/"),
            ("abc/def/../../..", ".."),
            ("/abc/def/../../..", "/"),
            ("abc/def/../../../ghi/jkl/../../../mno", "../../mno"),
            // Combinations
            ("abc/./../def", "def"),
            ("abc//./../def", "def"),
            ("abc/../../././../def", "../../def"),
        ] {
            assert_eq!(path_clean(input), want, "Clean({input:?})");
        }
    }

    /// Go io/fs/fs_test.go `isValidPathTests`.
    #[test]
    fn valid_path_matches_go() {
        for (input, want) in [
            (".", true),
            ("x", true),
            ("x/y", true),
            ("", false),
            ("..", false),
            ("/", false),
            ("x/", false),
            ("/x", false),
            ("x/y/", false),
            ("/x/y", false),
            ("./", false),
            ("./x", false),
            ("x/.", false),
            ("x/./y", false),
            ("../", false),
            ("../x", false),
            ("x/..", false),
            ("x/../y", false),
            ("x//y", false),
            ("x\\", true),
            ("x\\y", true),
            ("x:y", true),
            ("\\x", true),
        ] {
            assert_eq!(valid_path(input), want, "ValidPath({input:?})");
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
