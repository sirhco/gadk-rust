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

//! The `preload_memory` tool (port of Go `tool/preloadmemorytool`).
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

/// Go's zero `time.Time` as Unix seconds (`0001-01-01T00:00:00Z`).
const GO_ZERO_UNIX_SECONDS: i64 = -62_135_596_800;

/// Port of Go `preloadmemorytool.New` (tool.go:49).
pub fn new() -> Arc<dyn Tool> {
    Arc::new(PreloadMemoryTool)
}

/// Port of Go `preloadMemoryTool` (tool.go:43).
struct PreloadMemoryTool;

#[async_trait]
impl Tool for PreloadMemoryTool {
    fn name(&self) -> &str {
        "preload_memory"
    }

    fn description(&self) -> &str {
        "Preloads relevant memory for the current user."
    }

    /// Port of Go `preloadMemoryTool.ProcessRequest` (tool.go:73). Only the first user part is
    /// used as the query. A missing user content, missing parts or empty text is a no-op.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        let Some(query) = ctx
            .user_content()
            .and_then(|c| c.parts.first())
            .map(|p| p.text.clone())
            .filter(|t| !t.is_empty())
        else {
            return Ok(());
        };
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
        // Go preloadInstructions (tool.go:34): no trailing newline after the closing tag.
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

/// Port of Go `formatMemories` (tool.go:99). Entries with no text are skipped. A `Time:` line
/// (Go `time.RFC3339`, no fraction) precedes the text when the timestamp is set, and the text is
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

/// Go `Timestamp.IsZero()` on a UTC time.
fn is_go_zero(t: &DateTime<Utc>) -> bool {
    t.timestamp() == GO_ZERO_UNIX_SECONDS && t.timestamp_subsec_nanos() == 0
}

/// Port of Go `extractText` (tool.go:118): the non-empty, non-thought part texts joined with one
/// space.
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
