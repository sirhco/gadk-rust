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

//! Port of ADK-Go `model/openaimodel/internal/shared/servertext_test.go`.

use pretty_assertions::assert_eq;

use crate::shared::servertext::{MAX_SERVER_TEXT_RUNES, clip_server_text};

/// Go's truncation marker (`…`).
const MARK: char = '\u{2026}';

/// Pins the cap's arithmetic, which counts characters while the strings it guards are bytes.
#[test]
fn test_clip_server_text() {
    // (name, in, wantRunes, wantMark)
    let tests: Vec<(&str, String, usize, bool)> = vec![
        ("short", "upstream exploded".to_string(), 17, false),
        ("blank", "  \n\t ".to_string(), 0, false),
        // Exactly at the cap: nothing was dropped, so nothing may claim it was.
        (
            "exactly the cap",
            "A".repeat(MAX_SERVER_TEXT_RUNES),
            MAX_SERVER_TEXT_RUNES,
            false,
        ),
        (
            "one past the cap",
            "A".repeat(MAX_SERVER_TEXT_RUNES + 1),
            MAX_SERVER_TEXT_RUNES + 1, // the cap plus the marker
            true,
        ),
        // Multi-byte, so slicing by bytes rather than characters would sever one.
        (
            "multi-byte past the cap",
            "\u{4e16}".repeat(MAX_SERVER_TEXT_RUNES + 10),
            MAX_SERVER_TEXT_RUNES + 1,
            true,
        ),
        (
            "astral past the cap",
            "\u{1f642}".repeat(MAX_SERVER_TEXT_RUNES + 10),
            MAX_SERVER_TEXT_RUNES + 1,
            true,
        ),
        // The cap lands inside a run of spaces, so the marker would otherwise be pushed out
        // behind them.
        (
            "cut inside whitespace",
            format!("{}{}{}", "A".repeat(250), " ".repeat(10), "B".repeat(10)),
            251, // 250 kept, the spaces dropped, plus the marker
            true,
        ),
    ];
    // Asserted absolutely, once: every other bound here is written against the constant.
    assert_eq!(
        MAX_SERVER_TEXT_RUNES, 256,
        "raising the cap is a deliberate change, so update this line and the bounds written against it"
    );
    for (name, input, want_runes, want_mark) in tests {
        let got = clip_server_text(&input);
        // A Rust `String` is valid UTF-8 by construction (Go's utf8.ValidString check).
        assert_eq!(got.chars().count(), want_runes, "{name}: rune count");
        assert_eq!(got.ends_with(MARK), want_mark, "{name}: truncation marked");
    }
}

// parity: adapted: Go's fuzz target runs here over its seed corpus only (no fuzzing engine); the invariants are Go's, and the valid-UTF-8 one holds by construction for a Rust `String`
#[test]
fn fuzz_clip_server_text() {
    let seeds: Vec<String> = vec![
        String::new(),
        "  ".to_string(),
        "upstream exploded".to_string(),
        "line\r\nforged".to_string(),
        "\u{1b}[2J".to_string(),
        "\u{2028}sep".to_string(),
        // An ellipsis the server itself sent, which is not a truncation marker.
        "rate limited, retrying \u{2026}".to_string(),
        "A".repeat(MAX_SERVER_TEXT_RUNES),
        "\u{4e16}".repeat(MAX_SERVER_TEXT_RUNES + 1),
        "\u{1f642}".repeat(MAX_SERVER_TEXT_RUNES + 1),
        // Clipped mid-whitespace, so the marker assertion has something to bite on.
        format!("{}{}{}", "A".repeat(250), " ".repeat(10), "B".repeat(10)),
    ];
    for input in seeds {
        let got = clip_server_text(&input);
        assert!(
            got.chars().count() <= MAX_SERVER_TEXT_RUNES + 1,
            "ClipServerText({input:?}) returned {} runes, want at most {}",
            got.chars().count(),
            MAX_SERVER_TEXT_RUNES + 1
        );
        assert_eq!(
            got.trim(),
            got,
            "ClipServerText({input:?}): want no leading or trailing space"
        );
        // Gated on the input having been clipped: a trailing marker the server sent is its own
        // text.
        if input.trim().chars().count() > MAX_SERVER_TEXT_RUNES
            && let Some(body) = got.strip_suffix(MARK)
        {
            assert_eq!(
                body.trim(),
                body,
                "ClipServerText({input:?}) = {got:?}, want no space before the truncation marker"
            );
        }
    }
}
