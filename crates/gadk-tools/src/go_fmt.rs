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

//! Text and path helpers whose output the tools keep identical to ADK's: [`html_escape_string`],
//! [`valid_path`] and [`go_lossy_utf8`]. Query escaping is [`gadk_platform::go_query_escape`],
//! lexical path cleaning [`gadk_platform::go_path_clean`] and the plain-text rendering of a JSON
//! value [`gadk_platform::go_sprint_value`]. The module also holds the crate's one error wrapper,
//! [`wrap_err`]. Private to the crate (the engine's and gadk-session's copies are private to those
//! crates).

use gadk_agent::ToolError;

/// Escapes HTML text: `&`, `'`, `<`, `>` and `"` become `&amp;`, `&#39;`, `&lt;`, `&gt;` and
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

/// Whether `p` is a valid slash-separated path: unrooted, with elements that are all non-empty and
/// neither `.` nor `..`; the single path `.` is also valid. (A Rust `&str` cannot hold the invalid
/// UTF-8 that would also make a path invalid.)
pub(crate) fn valid_path(p: &str) -> bool {
    p == "." || p.split('/').all(|e| !matches!(e, "" | "." | ".."))
}

/// A [`ToolError::Execution`] whose text is `"<msg>: <source>"` and whose source is `source`.
pub(crate) fn wrap_err(
    msg: impl Into<String>,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ToolError {
    ToolError::Execution(Box::new(gadk_platform::GoWrapped::new(msg, source)))
}

/// Converts bytes to a string: valid UTF-8 is copied, and each invalid byte becomes one U+FFFD.
/// Unlike [`String::from_utf8_lossy`], which emits one U+FFFD per maximal invalid subpart, this
/// emits one per invalid byte.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

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
}
