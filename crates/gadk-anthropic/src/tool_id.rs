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

//! Tool-use ID sanitizing.

use std::collections::HashMap;

/// The pattern of a valid tool-use ID: `^[a-zA-Z0-9_-]+$`.
fn is_valid_tool_use_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Maps invalid or empty `tool_use` IDs to deterministic fallbacks (`toolu_fallback_<n>`),
/// pairing function calls and function responses across one conversation in the same order as
/// adk-python. One sanitizer serves one request's contents.
#[derive(Debug, Default)]
pub(crate) struct ToolUseIdSanitizer {
    mapping: HashMap<String, String>,
    unpaired_calls_by_name: HashMap<String, Vec<String>>,
    unpaired_calls: Vec<String>,
    next_fallback: u64,
}

impl ToolUseIdSanitizer {
    /// A new sanitizer with no waiting calls.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn next_id(&mut self) -> String {
        let id = format!("toolu_fallback_{}", self.next_fallback);
        self.next_fallback += 1;
        id
    }

    /// The ID to send for a function call
    /// (`is_call`) or a function response with ID `tool_id` and name `tool_name`.
    /// - A valid ID is kept.
    /// - An invalid non-empty ID gets one fallback, reused for every later occurrence.
    /// - A call with an empty ID gets a new fallback and waits for its response.
    /// - A response with an empty ID takes the oldest waiting call of the same name, else the
    ///   oldest waiting call, else a new fallback.
    pub(crate) fn sanitize(&mut self, tool_id: &str, tool_name: &str, is_call: bool) -> String {
        if is_valid_tool_use_id(tool_id) {
            return tool_id.to_string();
        }
        if !tool_id.is_empty()
            && let Some(mapped) = self.mapping.get(tool_id)
        {
            return mapped.clone();
        }

        if is_call || !tool_id.is_empty() {
            let assigned = self.next_id();
            if !tool_id.is_empty() {
                self.mapping.insert(tool_id.to_string(), assigned.clone());
            } else {
                if !tool_name.is_empty() {
                    self.unpaired_calls_by_name
                        .entry(tool_name.to_string())
                        .or_default()
                        .push(assigned.clone());
                }
                self.unpaired_calls.push(assigned.clone());
            }
            return assigned;
        }

        if !tool_name.is_empty()
            && let Some(ids) = self.unpaired_calls_by_name.get_mut(tool_name)
            && !ids.is_empty()
        {
            let assigned = ids.remove(0);
            if let Some(idx) = self.unpaired_calls.iter().position(|id| *id == assigned) {
                self.unpaired_calls.remove(idx);
            }
            return assigned;
        }
        if !self.unpaired_calls.is_empty() {
            let assigned = self.unpaired_calls.remove(0);
            // A fallback ID is in at most one name's list, so the first hit is the only one.
            for ids in self.unpaired_calls_by_name.values_mut() {
                if let Some(idx) = ids.iter().position(|id| *id == assigned) {
                    ids.remove(idx);
                    break;
                }
            }
            return assigned;
        }

        self.next_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn valid_ids_are_kept() {
        let mut s = ToolUseIdSanitizer::new();
        assert_eq!(s.sanitize("toolu_01-AbC", "f", true), "toolu_01-AbC");
        assert_eq!(s.sanitize("toolu_01-AbC", "f", false), "toolu_01-AbC");
        assert_eq!(s.sanitize("x", "", false), "x");
    }

    #[test]
    fn invalid_ids_map_to_one_fallback_each() {
        let mut s = ToolUseIdSanitizer::new();
        assert_eq!(s.sanitize("call.1", "f", true), "toolu_fallback_0");
        assert_eq!(s.sanitize("call 2", "g", true), "toolu_fallback_1");
        assert_eq!(s.sanitize("call.1", "f", false), "toolu_fallback_0");
        assert_eq!(s.sanitize("call 2", "g", false), "toolu_fallback_1");
        // An invalid ID first seen on a response also gets (and keeps) a fallback.
        assert_eq!(s.sanitize("résumé", "h", false), "toolu_fallback_2");
        assert_eq!(s.sanitize("résumé", "h", true), "toolu_fallback_2");
    }

    #[test]
    fn empty_response_ids_pair_with_the_oldest_call_of_the_same_name() {
        let mut s = ToolUseIdSanitizer::new();
        assert_eq!(s.sanitize("", "a", true), "toolu_fallback_0");
        assert_eq!(s.sanitize("", "b", true), "toolu_fallback_1");
        assert_eq!(s.sanitize("", "a", true), "toolu_fallback_2");
        assert_eq!(s.sanitize("", "b", false), "toolu_fallback_1");
        assert_eq!(s.sanitize("", "a", false), "toolu_fallback_0");
        assert_eq!(s.sanitize("", "a", false), "toolu_fallback_2");
        // Nothing left to pair with: a new fallback.
        assert_eq!(s.sanitize("", "a", false), "toolu_fallback_3");
    }

    #[test]
    fn empty_response_ids_fall_back_to_the_oldest_call_overall() {
        let mut s = ToolUseIdSanitizer::new();
        assert_eq!(s.sanitize("", "a", true), "toolu_fallback_0");
        assert_eq!(s.sanitize("", "", true), "toolu_fallback_1");
        // Unknown name: the oldest pending call overall, which leaves "a"'s list too.
        assert_eq!(s.sanitize("", "zzz", false), "toolu_fallback_0");
        // Nameless response: the next oldest.
        assert_eq!(s.sanitize("", "", false), "toolu_fallback_1");
        // "a"'s pending call was consumed above, so this is a fresh fallback.
        assert_eq!(s.sanitize("", "a", false), "toolu_fallback_2");
    }
}
