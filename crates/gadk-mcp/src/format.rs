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

//! MCP result content as model-facing text (port of ADK-Go `tool/mcptoolset/tool.go`
//! `formatMCPContent` and its helpers).

use rmcp::model::ContentBlock;

/// Go `formatMCPContent`: text as is; embedded resources, resource links, images and audio as
/// `[MCP …]` labels (with a decoded text body where Go shows one); blocks joined by Go's
/// newline rule.
#[allow(unused_variables, clippy::todo)]
pub fn format_mcp_content(contents: &[ContentBlock]) -> String {
    todo!("Task 3")
}

/// Go `decodeTextBlob`: `blob` as text when `mime_type` is a text type with an acceptable
/// charset and the bytes are valid UTF-8.
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn decode_text_blob(blob: &[u8], mime_type: &str) -> Option<String> {
    todo!("Task 3")
}
