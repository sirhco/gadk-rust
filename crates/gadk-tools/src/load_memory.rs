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

//! The `load_memory` tool (port of Go `tool/loadmemorytool`), the Go-default JSON rendering of
//! memory entries, and the Go error-shape helpers shared by this crate's tools.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Context, RunnableTool, Tool, ToolError, ToolOriginKind};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use gadk_memory::MemoryEntry;
use gadk_model::LlmRequest;
use gadk_platform::go_type_name;
use serde_json::{Map, Value};

use crate::go_fmt::wrap_err;

const NAME: &str = "load_memory";
const DESCRIPTION: &str = "Loads the memory for the current user.";

/// Go `memoryInstructions` (tool.go:33), a raw string with one embedded newline.
const MEMORY_INSTRUCTIONS: &str = "You have memory. You can use it to answer questions. If any questions need\nyou to look up the memory, you should call load_memory function with a query.";

/// Go's zero `time.Time` as `MarshalJSON` writes it.
const GO_ZERO_TIME_JSON: &str = "0001-01-01T00:00:00Z";

/// Port of Go `loadmemorytool.New` (tool.go:42).
pub fn new() -> Arc<dyn Tool> {
    Arc::new(LoadMemoryTool)
}

/// Port of Go `loadMemoryTool` (tool.go:36).
struct LoadMemoryTool;

#[async_trait]
impl Tool for LoadMemoryTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    /// Port of Go `loadMemoryTool.Declaration` (tool.go:65).
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: NAME.into(),
            description: DESCRIPTION.into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: BTreeMap::from([(
                    "query".to_string(),
                    Schema {
                        r#type: Some(Type::String),
                        description: "The query to search memory for.".into(),
                        ..Default::default()
                    },
                )]),
                required: vec!["query".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    /// Port of Go `loadMemoryTool.ProcessRequest` (tool.go:112). The flow has already packed the
    /// tool (Go-parity note 8), so this only appends the memory instructions.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        append_instructions(req, &[MEMORY_INSTRUCTIONS]);
        Ok(())
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    /// Go classifies a `tool/loadmemorytool` tool as `LOCAL`.
    fn origin_kind(&self) -> ToolOriginKind {
        ToolOriginKind::Local
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for LoadMemoryTool {
    /// Port of Go `loadMemoryTool.Run` (tool.go:83). It returns `{"memories": [..]}`, with each
    /// entry rendered as Go's default JSON would render a `memory.Entry`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let Value::Object(m) = &args else {
            return Err(ToolError::InvalidArgument(format!(
                "unexpected args type, got: {}",
                go_type_name(&args)
            )));
        };
        let Some(raw) = m.get("query") else {
            return Err(ToolError::InvalidArgument(
                "missing required parameter: query".into(),
            ));
        };
        let Value::String(query) = raw else {
            return Err(ToolError::InvalidArgument(format!(
                "query must be a string, got: {}",
                go_type_name(raw)
            )));
        };
        let resp = ctx
            .search_memory(query)
            .await
            .map_err(|e| wrap_err("failed to search memory", e))?;
        let memories = resp.memories.iter().map(go_json_entry).collect();
        let mut result = Map::new();
        result.insert("memories".into(), Value::Array(memories));
        Ok(result)
    }
}

/// Go's default `encoding/json` rendering of a `memory.Entry` (memory/service.go:54). The keys
/// are the Go field names. `Content` is genai JSON or `null`, `Timestamp` is
/// `time.Time.MarshalJSON` (`None` is Go's zero time), and `CustomMetadata` is `null` when empty
/// (Go-parity note 4).
pub(crate) fn go_json_entry(e: &MemoryEntry) -> Value {
    let mut m = Map::new();
    m.insert("ID".into(), Value::String(e.id.clone()));
    m.insert(
        "Content".into(),
        e.content.as_ref().map_or(Value::Null, |c| {
            // Infallible: every `Content` field serializes to JSON without error (string-keyed
            // maps, `serde_json::Value`s, bytes as base64 strings), so the fallback is never
            // taken.
            serde_json::to_value(c).unwrap_or(Value::Null)
        }),
    );
    m.insert("Author".into(), Value::String(e.author.clone()));
    m.insert(
        "Timestamp".into(),
        Value::String(e.timestamp.as_ref().map_or_else(
            || GO_ZERO_TIME_JSON.to_string(),
            gadk_platform::go_format_rfc3339_nano,
        )),
    );
    m.insert(
        "CustomMetadata".into(),
        if e.custom_metadata.is_empty() {
            Value::Null
        } else {
            Value::Object(e.custom_metadata.clone())
        },
    );
    Value::Object(m)
}

/// A [`ToolError::Execution`] carrying Go's `errors.New(msg)` text.
pub(crate) fn go_error(msg: impl Into<String>) -> ToolError {
    let msg: String = msg.into();
    ToolError::Execution(msg.into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike, Utc};
    use gadk_genai::Content;
    use serde_json::json;

    #[test]
    fn load_memory_tool_is_a_local_tool() {
        let t = new();
        assert_eq!(t.origin_kind(), ToolOriginKind::Local);
        assert!(t.origin_agent().is_none());
    }

    #[test]
    fn go_json_entry_zero_value_matches_go() {
        assert_eq!(
            go_json_entry(&MemoryEntry::default()),
            json!({
                "ID": "",
                "Content": null,
                "Author": "",
                "Timestamp": "0001-01-01T00:00:00Z",
                "CustomMetadata": null
            })
        );
    }

    #[test]
    fn go_json_entry_renders_content_metadata_and_rfc3339_nano() {
        let base = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        let mut md = Map::new();
        md.insert("k".into(), json!("v"));
        let e = MemoryEntry {
            id: "m1".into(),
            content: Some(Content::from_text("hi", "user")),
            author: "user".into(),
            timestamp: Some(base.with_nanosecond(123_456_789).unwrap()),
            custom_metadata: md,
        };
        assert_eq!(
            go_json_entry(&e),
            json!({
                "ID": "m1",
                "Content": {"parts": [{"text": "hi"}], "role": "user"},
                "Author": "user",
                "Timestamp": "2025-01-02T03:04:05.123456789Z",
                "CustomMetadata": {"k": "v"}
            })
        );
    }
}
