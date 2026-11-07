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

//! Service-text redaction (port of ADK-Go `auth/gcp/redact.go`). Task 1 placeholder: Task 5
//! replaces every `todo!` body.

#![allow(dead_code, unused_variables, clippy::todo)]

pub(crate) const MAX_ERROR_BODY: usize = 1024; // redact.go:29
pub(crate) const WITHHELD_TEXT: &str =
    "[withheld: could not be shown free of the request's own identifiers]"; // redact.go:33
pub(crate) const REDACTED_MARKER: &str = "[redacted]"; // redact.go:36
pub(crate) const MAX_SCRUBBABLE_SECRET: usize = 4096; // redact.go:62
pub(crate) const MAX_STRADDLE_WINDOW: usize = MAX_ERROR_BODY + 8 * MAX_SCRUBBABLE_SECRET; // redact.go:148
pub(crate) const MAX_DECODE_PASSES: usize = 64; // redact.go:317

/// redact.go:107.
pub(crate) fn redacted_for_error(s: &str, secrets: &[&str]) -> String {
    todo!("Task 5")
}
/// redact.go:161.
pub(crate) fn showable(s: &str, secrets: &[&str]) -> Option<String> {
    todo!("Task 5")
}
/// redact.go:200.
pub(crate) fn recoverable(x: &str, secrets: &[&str]) -> bool {
    todo!("Task 5")
}
/// redact.go:342.
pub(crate) fn decode_fully(s: &str) -> (String, bool) {
    todo!("Task 5")
}
/// redact.go:379.
pub(crate) fn unescape_json(s: &str) -> String {
    todo!("Task 5")
}
/// redact.go:454.
pub(crate) fn visible_limit(s: &str) -> (usize, bool) {
    todo!("Task 5")
}
/// redact.go:482.
pub(crate) fn truncate_for_error(s: &str) -> (String, bool) {
    todo!("Task 5")
}
/// redact.go:520.
pub(crate) fn redact(s: &str, values: &[&str]) -> String {
    todo!("Task 5")
}
/// redact.go:558.
pub(crate) fn redact_within_limit(s: &str, values: &[&str]) -> (String, bool) {
    todo!("Task 5")
}
/// redact.go:588.
pub(crate) fn lowered_values(values: &[&str]) -> Vec<String> {
    todo!("Task 5")
}
/// redact.go:607.
pub(crate) fn redact_lowered(ls: &str, limit: usize, lowered: &[String]) -> (String, bool) {
    todo!("Task 5")
}
/// Go `strings.ToLower` on valid UTF-8: per char `unicode.ToLower`, i.e. Rust's
/// `char::to_lowercase` except U+0130 → 'i' (Rust gives "i\u{307}"); no final-sigma context.
/// Every lowering above goes through this.
pub(crate) fn go_to_lower(s: &str) -> String {
    todo!("Task 5")
}
