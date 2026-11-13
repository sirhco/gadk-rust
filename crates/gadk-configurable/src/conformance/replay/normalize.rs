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

//! Go `internal/configurable/conformance/replayplugin/yaml_utils.go`: `normalizeYAMLNode`
//! with `toIgnore` / `toIgnoreButRecurse`, over the gadk-goyaml node arena.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_goyaml::{Document, NodeId};

/// Go `toIgnore`: freeform keys whose values are neither renamed nor recursed into.
pub const TO_IGNORE: [&str; 4] = ["thought_signature", "http_options", "args", "response"];

/// Go `toIgnoreButRecurse`: snake_case structural keys kept as they are, values recursed into.
pub const TO_IGNORE_BUT_RECURSE: [&str; 8] = [
    "user_message_index",
    "agent_name",
    "llm_recording",
    "llm_request",
    "llm_responses",
    "tool_recording",
    "tool_call",
    "tool_response",
];

/// Go `normalizeYAMLNode`: a scalar `system_instruction` becomes a `user` content with one
/// text part, a mapping `llm_response` becomes the one-element `llm_responses` sequence, and
/// every other key loses its underscores (outside [`TO_IGNORE`] / [`TO_IGNORE_BUT_RECURSE`]).
pub fn normalize_yaml_node(doc: &mut Document, node: NodeId) {
    // Go: yaml_utils.go normalizeYAMLNode
    todo!("Task 6")
}
