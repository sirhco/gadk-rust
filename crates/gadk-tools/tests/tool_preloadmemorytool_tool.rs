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

//! Port of Go tool/preloadmemorytool/tool_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use gadk_agent::{AgentError, Context, Memory};
use gadk_genai::{Blob, Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_memory::{MemoryEntry, SearchResponse};
use gadk_model::LlmRequest;
use gadk_session::Session;
use gadk_tools::preload_memory;

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
fn create_tool_context(mem: MockMemory, user_content: Option<Content>) -> Context {
    let mut b = Context::builder().memory(Arc::new(mem));
    if let Some(c) = user_content {
        b = b.user_content(c);
    }
    b.build()
}

fn jan(day: u32) -> Option<DateTime<Utc>> {
    Some(Utc.with_ymd_and_hms(2025, 1, day, 12, 0, 0).unwrap())
}

fn inline_part() -> Part {
    Part {
        inline_data: Some(Blob::default()),
        ..Default::default()
    }
}

fn text_part(text: &str) -> Part {
    Part::from_text(text)
}

fn thought_part(text: &str) -> Part {
    Part {
        text: text.into(),
        thought: true,
        ..Default::default()
    }
}

fn parts(parts: Vec<Part>) -> Content {
    Content {
        parts,
        role: String::new(),
    }
}

#[test]
fn test_preload_memory_tool_basic_properties() {
    let tool = preload_memory::new();
    assert_eq!(tool.name(), "preload_memory", "Name()");
    assert_eq!(
        tool.description(),
        "Preloads relevant memory for the current user.",
        "Description()"
    );
    assert!(
        !tool.is_long_running(),
        "IsLongRunning() = true, want false"
    );
}

#[derive(Default)]
struct Case {
    name: &'static str,
    user_content: Option<Content>,
    memories: Vec<MemoryEntry>,
    search_err: Option<&'static str>,
    want_err: bool,
    want_instruction: bool,
    want_text_contains: Vec<&'static str>,
    want_text_omits: Vec<&'static str>,
}

// parity: adapted: Go's nil *genai.Part entries become empty Parts because a Rust part list cannot hold nil
#[tokio::test]
async fn test_preload_memory_tool_process_request() {
    let cases = vec![
        Case {
            name: "nil user content",
            user_content: None,
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "empty user content parts",
            user_content: Some(parts(vec![])),
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "user content with no text",
            user_content: Some(parts(vec![inline_part()])),
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "text in later part is ignored",
            user_content: Some(parts(vec![inline_part(), text_part("later text")])),
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "no memories found",
            user_content: Some(Content::from_text("test query", ROLE_USER)),
            memories: vec![],
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "memory search error",
            user_content: Some(Content::from_text("test query", ROLE_USER)),
            search_err: Some("search failed"),
            want_err: true,
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "memories with nil and empty parts",
            user_content: Some(Content::from_text("test query", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "user".into(),
                timestamp: jan(1),
                content: Some(parts(vec![
                    Part::default(),
                    text_part("valid memory"),
                    Part::default(),
                    text_part(""),
                ])),
                ..Default::default()
            }],
            want_instruction: true,
            want_text_contains: vec!["user: valid memory"],
            ..Default::default()
        },
        Case {
            name: "single memory entry",
            user_content: Some(Content::from_text("test query", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "user".into(),
                timestamp: jan(1),
                content: Some(Content::from_text("Hello world", ROLE_USER)),
                ..Default::default()
            }],
            want_instruction: true,
            want_text_contains: vec![
                "PAST_CONVERSATIONS",
                "user: Hello world",
                "Time: 2025-01-01T12:00:00Z",
            ],
            ..Default::default()
        },
        Case {
            name: "multiple memory entries",
            user_content: Some(Content::from_text("search term", ROLE_USER)),
            memories: vec![
                MemoryEntry {
                    author: "user".into(),
                    timestamp: jan(1),
                    content: Some(Content::from_text("First memory", ROLE_USER)),
                    ..Default::default()
                },
                MemoryEntry {
                    author: "assistant".into(),
                    timestamp: jan(2),
                    content: Some(Content::from_text("Second memory", ROLE_MODEL)),
                    ..Default::default()
                },
            ],
            want_instruction: true,
            want_text_contains: vec!["First memory", "Second memory", "user:", "assistant:"],
            ..Default::default()
        },
        Case {
            name: "memory entry without author",
            user_content: Some(Content::from_text("test", ROLE_USER)),
            memories: vec![MemoryEntry {
                timestamp: jan(1),
                content: Some(Content::from_text("Anonymous message", ROLE_USER)),
                ..Default::default()
            }],
            want_instruction: true,
            want_text_contains: vec!["Anonymous message"],
            ..Default::default()
        },
        Case {
            name: "memory entry without timestamp",
            user_content: Some(Content::from_text("test", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "user".into(),
                content: Some(Content::from_text("No timestamp", ROLE_USER)),
                ..Default::default()
            }],
            want_instruction: true,
            want_text_contains: vec!["user: No timestamp"],
            ..Default::default()
        },
        Case {
            name: "thought parts are omitted",
            user_content: Some(Content::from_text("test", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "model".into(),
                content: Some(parts(vec![
                    thought_part("private reasoning"),
                    text_part("visible answer"),
                ])),
                ..Default::default()
            }],
            want_instruction: true,
            want_text_contains: vec!["model: visible answer"],
            want_text_omits: vec!["private reasoning"],
            ..Default::default()
        },
        Case {
            name: "thought-only memory entry",
            user_content: Some(Content::from_text("test", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "model".into(),
                timestamp: jan(1),
                content: Some(parts(vec![thought_part("private reasoning")])),
                ..Default::default()
            }],
            want_instruction: false,
            ..Default::default()
        },
        Case {
            name: "memory entry with empty content",
            user_content: Some(Content::from_text("test", ROLE_USER)),
            memories: vec![MemoryEntry {
                author: "user".into(),
                timestamp: jan(1),
                content: None,
                ..Default::default()
            }],
            want_instruction: false,
            ..Default::default()
        },
    ];

    for tt in cases {
        let name = tt.name;
        let tc = create_tool_context(
            MockMemory {
                memories: tt.memories,
                err: tt.search_err.map(str::to_string),
            },
            tt.user_content,
        );
        let mut llm_request = LlmRequest::default();
        let pmt = preload_memory::new();
        let res = pmt.process_request(&tc, &mut llm_request).await;
        if tt.want_err {
            assert!(
                res.is_err(),
                "{name}: ProcessRequest expected error, got nil"
            );
        } else if let Err(e) = res {
            panic!("{name}: ProcessRequest failed: {e}");
        }

        let si = llm_request
            .config
            .as_ref()
            .and_then(|c| c.system_instruction.as_ref());
        assert_eq!(si.is_some(), tt.want_instruction, "{name}: hasInstruction");
        if let (true, Some(si)) = (tt.want_instruction, si) {
            let instruction = &si.parts[0].text;
            for want in &tt.want_text_contains {
                assert!(
                    instruction.contains(want),
                    "{name}: Instruction should contain {want:?}, got: {instruction}"
                );
            }
            for omit in &tt.want_text_omits {
                assert!(
                    !instruction.contains(omit),
                    "{name}: Instruction should not contain {omit:?}, got: {instruction}"
                );
            }
        }
    }
}
