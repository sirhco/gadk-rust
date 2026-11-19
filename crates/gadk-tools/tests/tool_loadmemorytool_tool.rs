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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go tool/loadmemorytool/tool_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use gadk_agent::{AgentError, Context, Memory};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_memory::{MemoryEntry, SearchResponse};
use gadk_model::LlmRequest;
use gadk_session::Session;
use gadk_tools::load_memory;
use serde_json::{Value, json};

/// Port of Go `mockMemory`.
struct MockMemory {
    memories: Vec<MemoryEntry>,
    err: Option<String>,
}

#[async_trait]
impl Memory for MockMemory {
    async fn add_session_to_memory(&self, _session: Arc<dyn Session>) -> Result<(), AgentError> {
        Ok(())
    }

    async fn search_memory(&self, _query: &str) -> Result<SearchResponse, AgentError> {
        if let Some(e) = &self.err {
            return Err(AgentError::other(e.clone()));
        }
        Ok(SearchResponse {
            memories: self.memories.clone(),
        })
    }
}

/// Port of Go `createToolContext`.
fn create_tool_context(mem: MockMemory) -> Context {
    Context::builder().memory(Arc::new(mem)).build()
}

fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

fn entry(author: &str, ts: DateTime<Utc>, text: &str, role: &str) -> MemoryEntry {
    MemoryEntry {
        author: author.into(),
        timestamp: Some(ts),
        content: Some(Content::from_text(text, role)),
        ..Default::default()
    }
}

#[test]
fn test_load_memory_tool_basic_properties() {
    let tool = load_memory::new();
    assert_eq!(tool.name(), "load_memory", "Name()");
    assert_eq!(
        tool.description(),
        "Loads the memory for the current user.",
        "Description()"
    );
    assert!(
        !tool.is_long_running(),
        "IsLongRunning() = true, want false"
    );
}

/// A `TestLoadMemoryTool_Run` case: (name, args, stored memories, Ok(memory count) or Err(error
/// text)).
type RunCase = (
    &'static str,
    Value,
    Vec<MemoryEntry>,
    Result<usize, &'static str>,
);

// parity: adapted: Go's int 123 argument is a JSON number (Go float64) in Rust, and Go's []memory.Entry result is the Go-default JSON rendering of each entry; the error cases also pin Go's error texts
#[tokio::test]
async fn test_load_memory_tool_run() {
    let tool = load_memory::new();
    let cases: Vec<RunCase> = vec![
        (
            "empty memories",
            json!({"query": "test query"}),
            vec![],
            Ok(0),
        ),
        (
            "single memory entry",
            json!({"query": "test query"}),
            vec![entry("user", at(2025, 1, 1, 12), "Hello world", ROLE_USER)],
            Ok(1),
        ),
        (
            "multiple memory entries",
            json!({"query": "search term"}),
            vec![
                entry("user", at(2025, 1, 1, 12), "First memory", ROLE_USER),
                entry("assistant", at(2025, 1, 2, 12), "Second memory", ROLE_MODEL),
            ],
            Ok(2),
        ),
        (
            "missing query parameter",
            json!({}),
            vec![],
            Err("missing required parameter: query"),
        ),
        (
            "invalid query type",
            json!({"query": 123}),
            vec![],
            Err("invalid query: expected a string, found a number"),
        ),
    ];
    for (name, args, memories, want) in cases {
        let tc = create_tool_context(MockMemory {
            memories,
            err: None,
        });
        let got = tool
            .as_runnable()
            .expect("load_memory is runnable")
            .run(tc, args)
            .await;
        match want {
            Err(want_err) => {
                let err = got.expect_err(&format!("{name}: Run() error = nil, wantErr true"));
                assert_eq!(err.to_string(), want_err, "{name}");
            }
            Ok(want_len) => {
                let result = got.unwrap_or_else(|e| panic!("{name}: Run() error = {e}"));
                let memories = result
                    .get("memories")
                    .and_then(Value::as_array)
                    .unwrap_or_else(|| {
                        panic!("{name}: result['memories'] is not a list: {result:?}")
                    });
                assert_eq!(memories.len(), want_len, "{name}: Run() returned memories");
                if name == "single memory entry" {
                    assert_eq!(
                        memories[0],
                        json!({
                            "ID": "",
                            "Content": {"parts": [{"text": "Hello world"}], "role": "user"},
                            "Author": "user",
                            "Timestamp": "2025-01-01T12:00:00Z",
                            "CustomMetadata": null
                        }),
                        "{name}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn test_load_memory_tool_process_request() {
    let tool = load_memory::new();
    let tc = create_tool_context(MockMemory {
        memories: vec![],
        err: None,
    });
    let mut llm_request = LlmRequest::default();
    tool.process_request(&tc, &mut llm_request)
        .await
        .unwrap_or_else(|e| panic!("ProcessRequest failed: {e}"));
    let si = llm_request
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
        .expect("ProcessRequest did not set SystemInstruction");
    let instruction = &si.parts[0].text;
    assert!(
        instruction.contains("You have memory"),
        "Instruction should contain 'You have memory', got: {instruction}"
    );
    assert!(
        instruction.contains("load_memory"),
        "Instruction should contain 'load_memory', got: {instruction}"
    );
}
