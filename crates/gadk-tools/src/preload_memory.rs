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

//! The `preload_memory` tool.
//!
//! The model never calls this tool, unlike `load_memory`. On every model request it searches
//! memory with the user's current query and puts the matching past conversations into the system
//! instruction. It has no declaration, so the flow never packs it.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Context, Tool, ToolError};
use gadk_memory::MemoryEntry;
use gadk_model::LlmRequest;

use crate::go_fmt::wrap_err;

/// The timestamp `0001-01-01T00:00:00Z` as Unix seconds: the "unset" time.
const GO_ZERO_UNIX_SECONDS: i64 = -62_135_596_800;

/// A `preload_memory` tool.
pub fn new() -> Arc<dyn Tool> {
    Arc::new(PreloadMemoryTool)
}

/// The `preload_memory` tool.
struct PreloadMemoryTool;

#[async_trait]
impl Tool for PreloadMemoryTool {
    fn name(&self) -> &str {
        "preload_memory"
    }

    fn description(&self) -> &str {
        "Preloads relevant memory for the current user."
    }

    /// Searches memory and adds the matching past conversations to the system instruction. The
    /// query is every non-empty text part of the user content joined with spaces (thought parts
    /// included). A missing user content or an empty query is a no-op.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let Some(content) = ctx.user_content() else {
            return Ok(());
        };
        let query = content
            .parts
            .iter()
            .filter(|p| !p.text.is_empty())
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if query.is_empty() {
            return Ok(());
        }
        let resp = ctx
            .search_memory(&query)
            .await
            .map_err(|e| wrap_err("preload memory search failed", e))?;
        if resp.memories.is_empty() {
            return Ok(());
        }
        let memory_text = format_memories(&resp.memories);
        if memory_text.is_empty() {
            return Ok(());
        }
        // No trailing newline after the closing tag.
        let instructions = format!(
            "The following content is from your previous conversations with the user.\nThey may be useful for answering the user's current query.\n<PAST_CONVERSATIONS>\n{memory_text}\n</PAST_CONVERSATIONS>"
        );
        append_instructions(req, &[&instructions]);
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Formats the memories for the instruction. Entries with no text are skipped. A `Time:` line
/// (RFC 3339, no fraction) precedes the text when the timestamp is set, and the text is
/// `<author>: <text>` when there is an author.
fn format_memories(memories: &[MemoryEntry]) -> String {
    let mut lines = Vec::new();
    for mem in memories {
        let text = extract_text(mem);
        if text.is_empty() {
            continue;
        }
        if let Some(ts) = mem.timestamp.as_ref().filter(|t| !is_go_zero(t)) {
            lines.push(format!("Time: {}", ts.format("%Y-%m-%dT%H:%M:%SZ")));
        }
        if mem.author.is_empty() {
            lines.push(text);
        } else {
            lines.push(format!("{}: {text}", mem.author));
        }
    }
    lines.join("\n")
}

/// Whether the time is the "unset" time (`0001-01-01T00:00:00Z`).
fn is_go_zero(t: &DateTime<Utc>) -> bool {
    t.timestamp() == GO_ZERO_UNIX_SECONDS && t.timestamp_subsec_nanos() == 0
}

/// The non-empty, non-thought part texts joined with one space.
fn extract_text(mem: &MemoryEntry) -> String {
    let Some(content) = &mem.content else {
        return String::new();
    };
    content
        .parts
        .iter()
        .filter(|p| !p.text.is_empty() && !p.thought)
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
