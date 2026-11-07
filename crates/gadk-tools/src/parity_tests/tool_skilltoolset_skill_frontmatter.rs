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

//! Port of ADK-Go tool/skilltoolset/skill/frontmatter_test.go.

use std::collections::BTreeMap;
use std::error::Error as _;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, BufReader, ReadBuf};

use crate::skills::skill::{Frontmatter, build, parse, parse_bytes, validate};

fn fm(name: &str, description: &str) -> Frontmatter {
    Frontmatter {
        name: name.into(),
        description: description.into(),
        ..Default::default()
    }
}

// parity: adapted: Go's "nil frontmatter" case cannot be expressed (validate takes &Frontmatter); the other 14 cases are ported.
#[test]
fn test_validate() {
    let compat = |c: String| Frontmatter {
        compatibility: c,
        ..fm("valid-name", "Valid.")
    };
    let tests: Vec<(&str, Frontmatter, bool)> = vec![
        (
            "valid frontmatter",
            fm("valid-name", "A valid description."),
            false,
        ),
        ("name too short", fm("", "Valid description."), true),
        (
            "name too long",
            fm(&"a".repeat(65), "Valid description."),
            true,
        ),
        (
            "name starts with hyphen",
            fm("-name", "Valid description."),
            true,
        ),
        (
            "name ends with hyphen",
            fm("name-", "Valid description."),
            true,
        ),
        (
            "name has consecutive hyphens",
            fm("na--me", "Valid description."),
            true,
        ),
        (
            "name contains uppercase letters",
            fm("Invalid-Name", "Valid description."),
            true,
        ),
        ("description too short", fm("valid-name", ""), true),
        (
            "description too long",
            fm("valid-name", &"a".repeat(1025)),
            true,
        ),
        (
            "description at unicode character limit",
            fm("valid-name", &"技".repeat(1024)),
            false,
        ),
        (
            "description exceeds unicode character limit",
            fm("valid-name", &"技".repeat(1025)),
            true,
        ),
        ("compatibility too long", compat("a".repeat(501)), true),
        (
            "compatibility at unicode character limit",
            compat("é".repeat(500)),
            false,
        ),
        (
            "compatibility exceeds unicode character limit",
            compat("é".repeat(501)),
            true,
        ),
    ];
    for (name, frontmatter, want_err) in tests {
        let err = validate(&frontmatter);
        assert_eq!(
            err.is_err(),
            want_err,
            "{name}: expected error {want_err}, got {err:?}"
        );
    }
}

#[tokio::test]
async fn test_parse_valid() {
    let tests: Vec<(&str, &str, Frontmatter, &str)> = vec![
        (
            "valid minimal frontmatter",
            "---\nname: skill\ndescription: skill\n---\nMarkdown Body",
            fm("skill", "skill"),
            "Markdown Body",
        ),
        (
            "valid minimal frontmatter with windows line endings",
            "---\r\nname: skill\r\ndescription: skill\r\n---\r\nMarkdown Body",
            fm("skill", "skill"),
            "Markdown Body",
        ),
        (
            "allowed tools as specification scalar",
            "---\nname: skill\ndescription: skill\nallowed-tools: Bash(git:*) Bash(jq:*) Read\n---\nMarkdown Body",
            Frontmatter {
                allowed_tools: vec!["Bash(git:*)".into(), "Bash(jq:*)".into(), "Read".into()],
                ..fm("skill", "skill")
            },
            "Markdown Body",
        ),
        (
            "allowed tools comma-separated with spaces inside parentheses",
            "---\nname: skill\ndescription: skill\nallowed-tools: Read, Bash(kubectl get:*)\n---\nMarkdown Body",
            Frontmatter {
                allowed_tools: vec!["Read".into(), "Bash(kubectl get:*)".into()],
                ..fm("skill", "skill")
            },
            "Markdown Body",
        ),
        (
            "valid full frontmatter",
            "---
name: my-cool-skill
description: A cool skill.
metadata:
  author: \"Cool Author\"
  version: \"1.0.0\"
allowed-tools:
  - tool1
  - tool2
compatibility: \"compatible indeed\"
license: \"yes\"
---

# Long

Multi-line
Markdown
Body

",
            Frontmatter {
                name: "my-cool-skill".into(),
                description: "A cool skill.".into(),
                metadata: BTreeMap::from([
                    ("author".to_string(), "Cool Author".to_string()),
                    ("version".to_string(), "1.0.0".to_string()),
                ]),
                allowed_tools: vec!["tool1".into(), "tool2".into()],
                compatibility: "compatible indeed".into(),
                license: "yes".into(),
            },
            "\n# Long\n\nMulti-line\nMarkdown\nBody\n\n",
        ),
    ];
    for (name, input, want, want_instruction) in tests {
        let mut reader = BufReader::new(input.as_bytes());
        let got = parse(&mut reader)
            .await
            .unwrap_or_else(|e| panic!("{name}: Parse: expected no error, got: {e}"));
        assert_eq!(got, want, "{name}: Parse");
        let mut rest = String::new();
        reader.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, want_instruction, "{name}: unread instruction");
    }
}

#[tokio::test]
async fn test_parse_invalid_format() {
    let tests: Vec<(&str, &str, &str)> = vec![
        (
            "missing opening separator",
            "name: skill\ndescription: skill\n---\n# Markdown\nBody",
            "",
        ),
        (
            "missing newline after opening separator",
            "---name: skill\ndescription: skill\n---\n# Markdown\nBody",
            "",
        ),
        (
            "whitespace after opening separator",
            "--- \nname: skill\ndescription: skill\n---\n# Markdown\nBody",
            "",
        ),
        (
            "missing closing separator",
            "---\nname: skill\ndescription: skill\n",
            "",
        ),
        (
            "missing newline after closing separator",
            "---\nname: skill\ndescription: skill\n---# Markdown\nBody",
            "",
        ),
        (
            "whitespace after closing separator",
            "---\nname: skill\ndescription: skill\n--- \n# Markdown\nBody",
            "",
        ),
        (
            "invalid yaml",
            "---\n_ : invalid : _\n---\n# Markdown\nBody",
            "",
        ),
        (
            "fails frontmatter validation",
            "---\nname: INVALID_NAME\ndescription: test\n---\n# Markdown\nBody",
            "",
        ),
        (
            "unknown fields",
            "---\nname: skill\ndescription: skill\nunknown-field: field\n---\n# Markdown\nBody",
            "",
        ),
        (
            "allowed tools with unexpected closing parenthesis",
            "---\nname: skill\ndescription: skill\nallowed-tools: Read) Bash\n---\n# Markdown\nBody",
            "allowed-tools must be a whitespace- or comma-separated list of Tool or Tool(well-balanced expression): unexpected ')'",
        ),
        (
            "allowed tools with unclosed opening parenthesis",
            "---\nname: skill\ndescription: skill\nallowed-tools: Bash(git:* Read\n---\n# Markdown\nBody",
            "allowed-tools must be a whitespace- or comma-separated list of Tool or Tool(well-balanced expression): unclosed '('",
        ),
        (
            "allowed tools with closing parenthesis before opening parenthesis",
            "---\nname: skill\ndescription: skill\nallowed-tools: Bash)(git:*)\n---\n# Markdown\nBody",
            "allowed-tools must be a whitespace- or comma-separated list of Tool or Tool(well-balanced expression): unexpected ')'",
        ),
    ];
    for (name, input, want_err) in tests {
        let mut reader = BufReader::new(input.as_bytes());
        let err = match parse(&mut reader).await {
            Ok(got) => panic!("{name}: expected error, got {got:?}"),
            Err(err) => err,
        };
        assert!(
            want_err.is_empty() || err.to_string().contains(want_err),
            "{name}: error {:?} does not contain {want_err:?}",
            err.to_string()
        );
    }
}

/// Go `iotest.ErrReader(err)`: every read fails with `err`.
struct ErrReader;

impl AsyncRead for ErrReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("immediate error")))
    }
}

/// Go `iotest.TimeoutReader(r)`: the second read fails with `ErrTimeout` ("timeout") and
/// returns no data; the other reads go to `r`.
struct TimeoutReader {
    inner: &'static [u8],
    reads: usize,
}

impl AsyncRead for TimeoutReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reads += 1;
        if self.reads == 2 {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "timeout")));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// Go `errors.Is(err, target)` for an I/O target: some link of the chain is a
/// `std::io::Error` with the target's kind and text.
fn chain_has_io_error(err: &crate::skills::skill::SkillError, want: &io::Error) -> bool {
    let mut next: Option<&(dyn std::error::Error + 'static)> = err.source();
    while let Some(e) = next {
        if let Some(io_err) = e.downcast_ref::<io::Error>()
            && io_err.kind() == want.kind()
            && io_err.to_string() == want.to_string()
        {
            return true;
        }
        next = e.source();
    }
    false
}

// parity: adapted: Go's testing/iotest failing readers become AsyncRead stubs behind tokio BufReader; errors.Is is a source-chain search for the io::Error.
#[tokio::test]
async fn test_parse_failing_reader() {
    let immediate = io::Error::other("immediate error");
    let mut reader = BufReader::new(ErrReader);
    let err = parse(&mut reader).await.unwrap_err();
    assert!(
        chain_has_io_error(&err, &immediate),
        "immediate error: got {err}"
    );
    assert_eq!(err.to_string(), "reading first line: immediate error");

    let timeout = io::Error::new(io::ErrorKind::TimedOut, "timeout");
    let mut reader = BufReader::new(TimeoutReader {
        inner: b"---",
        reads: 0,
    });
    let err = parse(&mut reader).await.unwrap_err();
    assert!(
        chain_has_io_error(&err, &timeout),
        "timeout error: got {err}"
    );
    assert_eq!(err.to_string(), "reading first line: timeout");
}

#[test]
fn test_parse_bytes() {
    let input = b"---\nname: my-skill\ndescription: A cool skill\n---\n# Markdown Content\nHere.";
    let (got_frontmatter, got_instruction) = parse_bytes(input).unwrap();
    assert_eq!(got_frontmatter, fm("my-skill", "A cool skill"));
    assert_eq!(got_instruction, "# Markdown Content\nHere.");
}

#[test]
fn test_parse_bytes_error() {
    let input = b"---\ninvalid-frontmatter without closing";
    assert!(
        parse_bytes(input).is_err(),
        "ParseBytes: expected error for invalid frontmatter bytes, got nil"
    );
}

#[test]
fn test_build() {
    let frontmatter = Frontmatter {
        name: "my-skill".into(),
        description: "A test skill.".into(),
        metadata: BTreeMap::from([("author".to_string(), "Cool Author".to_string())]),
        allowed_tools: vec!["tool1".into(), "tool2".into()],
        ..Default::default()
    };
    let instruction = "# Instruction\nDo something cool.";

    let out_bytes = build(&frontmatter, instruction).unwrap();

    let out_str = String::from_utf8(out_bytes).unwrap();
    assert!(
        out_str.starts_with("---\n"),
        "Build: output must start with frontmatter separator"
    );
    assert!(
        out_str.contains("\n---\n"),
        "Build: output missing proper closing separator"
    );
    for field in ["name", "description", "metadata", "allowed-tools"] {
        assert!(
            out_str.contains(field),
            "Build: output missing {field} field"
        );
    }
    for field in ["compatibility", "license"] {
        assert!(
            !out_str.contains(field),
            "Build: output should not contain {field} field"
        );
    }
    let (parsed_frontmatter, parsed_instruction) = parse_bytes(out_str.as_bytes())
        .unwrap_or_else(|e| panic!("ParseBytes: failed to parse frontmatter after Build: {e}"));
    assert_eq!(parsed_frontmatter, frontmatter);
    assert_eq!(parsed_instruction, instruction);
}

#[test]
fn test_build_validation_error() {
    let frontmatter = fm("INVALID_NAME!!!", "A test skill.");
    assert!(
        build(&frontmatter, "Some markdown").is_err(),
        "Build: expected validation error, got nil"
    );
}
