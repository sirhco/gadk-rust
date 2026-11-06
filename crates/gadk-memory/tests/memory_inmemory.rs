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

//! Port of Go memory/inmemory_test.go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_memory::{
    InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest, SearchResponse,
};
use gadk_session::{Event, Session, SessionError, State, zero_time};
use serde_json::{Map, Value, json};

/// Port of Go testSession: fixed identity and events; State/LastUpdateTime are unused.
struct TestSession {
    app: String,
    user: String,
    id: String,
    events: Vec<Event>,
}
struct NoState;
impl State for NoState {
    fn get(&self, _: &str) -> Result<Value, SessionError> {
        Err(SessionError::StateKeyNotExist)
    }
    fn set(&self, _: &str, _: Value) -> Result<(), SessionError> {
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
}
impl Session for TestSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app
    }
    fn user_id(&self) -> &str {
        &self.user
    }
    fn state(&self) -> &dyn State {
        &NoState
    }
    fn events(&self) -> Vec<Event> {
        self.events.clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        zero_time()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
fn make_session(app: &str, user: &str, id: &str, events: Vec<Event>) -> TestSession {
    TestSession {
        app: app.into(),
        user: user.into(),
        id: id.into(),
        events,
    }
}
fn text_event(id: &str, author: &str, text: &str, role: &str, ts: Option<&str>) -> Event {
    let mut e = Event {
        id: id.into(),
        author: author.into(),
        ..Default::default()
    };
    e.response.content = Some(Content::from_text(text, role));
    if let Some(ts) = ts {
        e.timestamp = DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc);
    }
    e
}
fn entry(
    id: &str,
    author: &str,
    text: &str,
    role: &str,
    ts: Option<&str>,
    meta: Map<String, Value>,
) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        author: author.into(),
        content: Some(Content::from_text(text, role)),
        timestamp: ts.map(|t| DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc)),
        custom_metadata: meta,
    }
}
/// Go sortMemories: order by timestamp (None first, like Go's zero time).
fn sorted(mut r: SearchResponse) -> SearchResponse {
    r.memories.sort_by_key(|m| m.timestamp);
    r
}

#[tokio::test]
async fn test_in_memory_service_search_memory() {
    struct Case {
        name: &'static str,
        sessions: Vec<TestSession>,
        req: SearchRequest,
        want: SearchResponse,
    }
    let req = |app: &str, user: &str, q: &str| SearchRequest {
        app_name: app.into(),
        user_id: user.into(),
        query: q.into(),
    };
    let single = |sid: &str, text: &str| {
        vec![make_session(
            "app1",
            "user1",
            sid,
            vec![text_event("", "", text, ROLE_MODEL, None)],
        )]
    };
    let only = |text: &str| SearchResponse {
        memories: vec![entry("", "", text, ROLE_MODEL, None, Map::new())],
    };
    let mut first = text_event(
        "event1",
        "user1",
        "The Quick brown fox",
        ROLE_USER,
        Some("2023-10-01T10:00:00Z"),
    );
    first.response.custom_metadata = json!({"key": "value"}).as_object().cloned().unwrap();
    let cases = vec![
        Case {
            name: "find events",
            sessions: vec![
                make_session(
                    "app1",
                    "user1",
                    "sess1",
                    vec![
                        first,
                        text_event("", "", "jumps over the lazy dog", ROLE_MODEL, None),
                    ],
                ),
                make_session(
                    "app1",
                    "user1",
                    "sess2",
                    vec![text_event(
                        "",
                        "test-bot",
                        "hello world",
                        ROLE_MODEL,
                        Some("2023-10-02T10:00:00Z"),
                    )],
                ),
                make_session(
                    "app1",
                    "user1",
                    "sess3",
                    vec![text_event("", "", "test text", ROLE_USER, None)],
                ),
            ],
            req: req("app1", "user1", "quick hello"),
            want: SearchResponse {
                memories: vec![
                    entry(
                        "event1",
                        "user1",
                        "The Quick brown fox",
                        ROLE_USER,
                        Some("2023-10-01T10:00:00Z"),
                        json!({"key": "value"}).as_object().cloned().unwrap(),
                    ),
                    entry(
                        "",
                        "test-bot",
                        "hello world",
                        ROLE_MODEL,
                        Some("2023-10-02T10:00:00Z"),
                        Map::new(),
                    ),
                ],
            },
        },
        Case {
            name: "matches punctuation and non-space whitespace",
            sessions: single(
                "sess-punctuation",
                "Error: connection\ntimeout! Please\tretry.",
            ),
            req: req("app1", "user1", "error timeout retry"),
            want: only("Error: connection\ntimeout! Please\tretry."),
        },
        Case {
            name: "splits newline independently",
            sessions: single("sess-newline-only", "prefix\nzebra"),
            req: req("app1", "user1", "zebra"),
            want: only("prefix\nzebra"),
        },
        Case {
            name: "splits tab independently",
            sessions: single("sess-tab-only", "prefix\twalrus"),
            req: req("app1", "user1", "walrus"),
            want: only("prefix\twalrus"),
        },
        Case {
            name: "splits internal punctuation",
            sessions: single(
                "sess-internal-punctuation",
                "error,timeout Use the built-in tool.",
            ),
            req: req("app1", "user1", "timeout built"),
            want: only("error,timeout Use the built-in tool."),
        },
        Case {
            name: "keeps digits as words",
            sessions: single("sess-digits", "Error 404."),
            req: req("app1", "user1", "404"),
            want: only("Error 404."),
        },
        Case {
            name: "preserves combining marks and underscores",
            sessions: single("sess-marks", "CAFE\u{0301}TERIA snake_case"),
            req: req("app1", "user1", "cafe\u{0301} snake_case"),
            want: only("CAFE\u{0301}TERIA snake_case"),
        },
        Case {
            name: "does not treat leading Unicode marks as words",
            sessions: single("sess-leading-mark", "\u{26A0}\u{FE0F} disk full"),
            req: req("app1", "user1", "thanks \u{2764}\u{FE0F}"),
            want: SearchResponse::default(),
        },
        Case {
            name: "preserves combining mark boundary",
            sessions: single("sess-combining-negative", "Je bois un cafe\u{0301}."),
            req: req("app1", "user1", "cafe"),
            want: SearchResponse::default(),
        },
        Case {
            name: "preserves underscore boundary",
            sessions: single("sess-underscore-negative", "call snake_case now"),
            req: req("app1", "user1", "snake"),
            want: SearchResponse::default(),
        },
        Case {
            name: "no leakage for different appName",
            sessions: vec![make_session(
                "app1",
                "user1",
                "sess3",
                vec![text_event("", "", "test text", ROLE_USER, None)],
            )],
            req: req("other_app", "user1", "test text"),
            want: SearchResponse::default(),
        },
        Case {
            name: "no leakage for different user",
            sessions: vec![make_session(
                "app1",
                "user1",
                "sess3",
                vec![text_event("", "", "test text", ROLE_USER, None)],
            )],
            req: req("app1", "test_user", "test text"),
            want: SearchResponse::default(),
        },
        Case {
            name: "no matches",
            sessions: vec![make_session(
                "app1",
                "user1",
                "sess3",
                vec![text_event("", "", "test text", ROLE_USER, None)],
            )],
            req: req("app1", "test_user", "something different"),
            want: SearchResponse::default(),
        },
        Case {
            name: "lookup on empty store",
            sessions: vec![],
            req: req("app1", "test_user", "something different"),
            want: SearchResponse::default(),
        },
    ];
    for c in cases {
        let s = InMemoryMemoryService::new();
        for sess in &c.sessions {
            s.add_session_to_memory(sess).await.unwrap();
        }
        let got = s.search_memory(c.req).await.unwrap();
        assert_eq!(sorted(got), sorted(c.want), "{}", c.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_in_memory_service_search_memory_concurrent() {
    let s = Arc::new(InMemoryMemoryService::new());
    s.add_session_to_memory(&make_session(
        "app1",
        "user1",
        "seed",
        vec![text_event("seed", "", "x", ROLE_USER, None)],
    ))
    .await
    .unwrap();
    let mut tasks = Vec::new();
    for i in 0..8 {
        let w = s.clone();
        tasks.push(tokio::spawn(async move {
            for j in 0..50 {
                let id = format!("s{i}-{j}");
                w.add_session_to_memory(&make_session(
                    "app1",
                    "user1",
                    &id,
                    vec![text_event(&id, "", "x", ROLE_USER, None)],
                ))
                .await
                .unwrap();
            }
        }));
        let r = s.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..50 {
                r.search_memory(SearchRequest {
                    app_name: "app1".into(),
                    user_id: "user1".into(),
                    query: "x".into(),
                })
                .await
                .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}
