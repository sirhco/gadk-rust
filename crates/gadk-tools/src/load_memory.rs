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
use chrono::{DateTime, Utc};
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Context, RunnableTool, Tool, ToolError};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use gadk_memory::MemoryEntry;
use gadk_model::LlmRequest;
use serde_json::{Map, Value};

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
            serde_json::to_value(c).unwrap_or(Value::Null)
        }),
    );
    m.insert("Author".into(), Value::String(e.author.clone()));
    m.insert(
        "Timestamp".into(),
        Value::String(
            e.timestamp
                .as_ref()
                .map_or_else(|| GO_ZERO_TIME_JSON.to_string(), go_rfc3339_nano),
        ),
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

/// Go `time.Time.MarshalJSON` for a UTC time: `time.RFC3339Nano`
/// (`2006-01-02T15:04:05.999999999Z07:00`). The fraction drops its trailing zeros and is omitted
/// when the nanoseconds are zero. A chrono leap-second nanosecond value (>= 1e9) is folded back,
/// because Go has no leap seconds.
fn go_rfc3339_nano(t: &DateTime<Utc>) -> String {
    let nanos = t.timestamp_subsec_nanos() % 1_000_000_000;
    let mut s = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s.push('Z');
    s
}

/// Go `%T` of a JSON-decoded value (`any` from `encoding/json`).
pub(crate) fn go_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "<nil>",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float64",
        Value::String(_) => "string",
        Value::Array(_) => "[]interface {}",
        Value::Object(_) => "map[string]interface {}",
    }
}

/// Go `fmt.Errorf("<msg>: %w", source)`: Display `"<msg>: <source>"`, `source()` = the wrapped
/// error.
#[derive(Debug)]
struct GoWrapped {
    msg: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for GoWrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl std::error::Error for GoWrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// A [`ToolError::Execution`] wrapping `source` as Go's `fmt.Errorf("<msg>: %w", source)` does.
pub(crate) fn wrap_err(
    msg: impl Into<String>,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> ToolError {
    ToolError::Execution(Box::new(GoWrapped {
        msg: msg.into(),
        source: source.into(),
    }))
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
    use chrono::{TimeZone, Timelike};
    use gadk_genai::Content;
    use serde_json::json;

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

    #[test]
    fn go_rfc3339_nano_trims_trailing_zeros() {
        let base = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(go_rfc3339_nano(&base), "2025-01-02T03:04:05Z");
        assert_eq!(
            go_rfc3339_nano(&base.with_nanosecond(500_000_000).unwrap()),
            "2025-01-02T03:04:05.5Z"
        );
        assert_eq!(
            go_rfc3339_nano(&base.with_nanosecond(1_000).unwrap()),
            "2025-01-02T03:04:05.000001Z"
        );
    }

    #[test]
    fn go_type_name_matches_go_percent_t() {
        assert_eq!(go_type_name(&json!(null)), "<nil>");
        assert_eq!(go_type_name(&json!(true)), "bool");
        assert_eq!(go_type_name(&json!(1)), "float64");
        assert_eq!(go_type_name(&json!("s")), "string");
        assert_eq!(go_type_name(&json!([])), "[]interface {}");
        assert_eq!(go_type_name(&json!({})), "map[string]interface {}");
    }

    #[test]
    fn wrap_err_keeps_go_text_and_source() {
        let err = wrap_err("failed to search memory", std::io::Error::other("boom"));
        assert_eq!(err.to_string(), "failed to search memory: boom");
        let src = std::error::Error::source(&err).expect("wrapped source");
        assert_eq!(src.to_string(), "boom");
    }
}
