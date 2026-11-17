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

//! Text helpers for the generated Dockerfile and the console banners.
//!
//! The checks take bytes (`&str`, `String` and `&[u8]` all qualify) because a byte string can
//! hold invalid UTF-8, which both checks reject; gadk's flag values are `String`s (clap rejects
//! a non-UTF-8 argument), so that branch is reached only by direct callers.

use gadk_platform::go_quote_bytes;

/// `s` padded with spaces to width `w`, the extra space on the right; `w`
/// counts bytes.
pub fn center_string(s: &str, w: usize) -> String {
    let sw = w as isize - s.len() as isize;
    let lw = sw / 2;
    let rw = sw - lw;
    // A string wider than `w` gets no padding (the count is clamped at 0). No caller passes
    // such a string.
    " ".repeat(lw.max(0) as usize) + s + &" ".repeat(rw.max(0) as usize)
}

/// Rejects `"`, `` ` ``, `\`, every C0 control byte and invalid
/// UTF-8, so a value embedded in a generated Dockerfile's exec-form `CMD` keeps the line a
/// valid JSON array and reaches the container unchanged. The weaker of the two checks; a value
/// that reaches a `RUN` line needs [`validate_shell_arg_safe`].
pub fn validate_dockerfile_safe(s: impl AsRef<[u8]>, label: &str) -> Result<(), String> {
    let s = s.as_ref();
    // A byte loop: the property is about the bytes written to the file.
    if s.iter()
        .any(|&c| c == b'"' || c == b'`' || c == b'\\' || c < 0x20)
    {
        return Err(format!(
            "{label} {} contains a character (quote, backtick, backslash, or a control character) that is not allowed in a value embedded in generated Dockerfile content",
            go_quote_bytes(s)
        ));
    }
    if std::str::from_utf8(s).is_err() {
        return Err(format!(
            "{label} {} is not valid UTF-8; the invalid bytes would reach the container as U+FFFD rather than as written",
            go_quote_bytes(s)
        ));
    }
    Ok(())
}

/// Admits only letters, digits, `.`, `/`, `-`, `_` and non-ASCII
/// runes (valid UTF-8), the check for a value reaching a shell-form `RUN` line or a `COPY`
/// operand.
pub fn validate_shell_arg_safe(s: impl AsRef<[u8]>, label: &str) -> Result<(), String> {
    let s = s.as_ref();
    // A byte loop: every byte of a multi-byte rune is >= 0x80, so the class applies
    // per byte and the UTF-8 check below rules out a stray high byte.
    let allowed =
        |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'/' | b'-') || c >= 0x80;
    if !s.iter().all(|&c| allowed(c)) {
        return Err(format!(
            "{label} {} contains a character not allowed in a value embedded in generated Dockerfile content; whitespace splits COPY operands and shell metacharacters are live in a RUN line, so only letters, digits, '.', '/', '-', '_', and non-ASCII characters are permitted",
            go_quote_bytes(s)
        ));
    }
    if std::str::from_utf8(s).is_err() {
        return Err(format!(
            "{label} {} is not valid UTF-8; a COPY operand containing an invalid byte does not match the file it names",
            go_quote_bytes(s)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    /// Go `CenterString` outputs (`runGcloudProxy`'s banner lines at width 80).
    #[test]
    fn center_string_pads_like_go() {
        assert_eq!(center_string("", 4), "    ");
        assert_eq!(center_string("ab", 5), " ab  ");
        assert_eq!(center_string("abc", 6), " abc  ");
        assert_eq!(center_string("abcd", 4), "abcd");
        assert_eq!(center_string("Press Ctrl-C to stop", 80).len(), 80);
        assert_eq!(
            center_string("Press Ctrl-C to stop", 80),
            format!("{}Press Ctrl-C to stop{}", " ".repeat(30), " ".repeat(30))
        );
        // Non-ASCII counts bytes, as Go's `len`.
        assert_eq!(center_string("é", 4), " é ");
        assert_eq!(center_string("toolong", 3), "toolong");
    }

    #[test]
    fn rejection_texts_are_go_verbatim() {
        assert_eq!(
            validate_dockerfile_safe("x\"y", "--a2a_agent_url").unwrap_err(),
            r#"--a2a_agent_url "x\"y" contains a character (quote, backtick, backslash, or a control character) that is not allowed in a value embedded in generated Dockerfile content"#
        );
        assert_eq!(
            validate_dockerfile_safe(b"http://x\xffy", "--a2a_agent_url").unwrap_err(),
            r#"--a2a_agent_url "http://x\xffy" is not valid UTF-8; the invalid bytes would reach the container as U+FFFD rather than as written"#
        );
        assert_eq!(
            validate_shell_arg_safe("main go", "label:").unwrap_err(),
            r#"label: "main go" contains a character not allowed in a value embedded in generated Dockerfile content; whitespace splits COPY operands and shell metacharacters are live in a RUN line, so only letters, digits, '.', '/', '-', '_', and non-ASCII characters are permitted"#
        );
        assert_eq!(
            validate_shell_arg_safe(b"agent\xff", "--source_dir").unwrap_err(),
            r#"--source_dir "agent\xff" is not valid UTF-8; a COPY operand containing an invalid byte does not match the file it names"#
        );
        assert_eq!(
            validate_dockerfile_safe("http://127.0.0.1:8081", "x"),
            Ok(())
        );
        assert_eq!(validate_shell_arg_safe("agentes/café/main", "x"), Ok(()));
    }
}
