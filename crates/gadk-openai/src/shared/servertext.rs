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

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

/// Bounds any server-chosen string an error quotes (Go `MaxServerTextRunes`).
pub(crate) const MAX_SERVER_TEXT_RUNES: usize = 256;

/// Go `ClipServerText`: trims `s` and caps it at [`MAX_SERVER_TEXT_RUNES`] characters,
/// marking a cut with `…` (after trimming the kept prefix again). Callers render the result
/// with [`gadk_platform::go_quote`].
pub(crate) fn clip_server_text(s: &str) -> String {
    todo!("Task 3")
}
