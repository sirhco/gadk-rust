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

//! Server-chosen text in error messages (port of ADK-Go `internal/shared/servertext.go`).

/// The mark a clipped string ends with (Go's `"\u2026"`, HORIZONTAL ELLIPSIS).
const TRUNCATION_MARK: char = '\u{2026}';

/// Bounds any server-chosen string an error quotes (Go `MaxServerTextRunes`).
pub(crate) const MAX_SERVER_TEXT_RUNES: usize = 256;

/// Go `ClipServerText`: trims `s` and caps it at [`MAX_SERVER_TEXT_RUNES`] characters,
/// marking a cut with `…` (after trimming the kept prefix again). Callers render the result
/// with [`gadk_platform::go_quote`].
pub(crate) fn clip_server_text(s: &str) -> String {
    let s = s.trim();
    // `nth` walks the characters rather than collecting them, so a megabyte of message costs
    // nothing beyond the kept prefix, and the byte offset it yields is a character boundary.
    match s.char_indices().nth(MAX_SERVER_TEXT_RUNES) {
        Some((cut, _)) => format!("{}{TRUNCATION_MARK}", s[..cut].trim_end()),
        None => s.to_string(),
    }
}
