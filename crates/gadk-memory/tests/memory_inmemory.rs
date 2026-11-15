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

use chrono::{DateTime, TimeZone, Utc};
use gadk_genai::{Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_memory::{
    AddEventsRequest, InMemoryMemoryService, MemoryEntry, MemoryError, MemoryService,
    SearchRequest, SearchResponse, add_events_to_memory,
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

/// Port of Go memoryTextEvent (memory/inmemory_search_test.go): one text part per string, role
/// user.
fn memory_text_event(id: &str, texts: &[&str]) -> Event {
    let mut e = Event {
        id: id.into(),
        ..Default::default()
    };
    e.response.content = Some(Content::from_parts(
        texts.iter().map(|t| Part::from_text(*t)).collect(),
        ROLE_USER,
    ));
    e
}

/// Go addEvents: adds `events` to the bucket `session_id` of ("app", "user").
async fn add_events(svc: &dyn MemoryService, session_id: &str, events: Vec<Event>) {
    add_events_to_memory(
        svc,
        AddEventsRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: session_id.into(),
            events,
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("AddEventsToMemory({session_id:?}) error = {e}"));
}

/// Go addSession: adds a session of ("app", "user") holding `events`.
async fn add_session(svc: &dyn MemoryService, session_id: &str, events: Vec<Event>) {
    svc.add_session_to_memory(&make_session("app", "user", session_id, events))
        .await
        .unwrap_or_else(|e| panic!("AddSessionToMemory({session_id:?}) error = {e}"));
}

/// Go search: searches the memories of ("app", "user").
async fn search(svc: &dyn MemoryService, query: &str) -> SearchResponse {
    svc.search_memory(SearchRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        query: query.into(),
    })
    .await
    .unwrap_or_else(|e| panic!("SearchMemory({query:?}) error = {e}"))
}

fn memory_ids(resp: &SearchResponse) -> Vec<String> {
    resp.memories.iter().map(|e| e.id.clone()).collect()
}

/// Go memoryTexts: the text of the last part of each memory.
fn memory_texts(resp: &SearchResponse) -> Vec<String> {
    resp.memories
        .iter()
        .map(|e| {
            e.content
                .as_ref()
                .and_then(|c| c.parts.last())
                .map(|p| p.text.clone())
                .unwrap_or_default()
        })
        .collect()
}

// parity: adapted: Go's nil event in the batch has no counterpart (a batch holds Event values)
#[tokio::test]
async fn test_add_events_to_memory_basic_and_entry_fields() {
    let svc = InMemoryMemoryService::new();
    let ts = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
    let mut ev = Event {
        id: "ev-1".into(),
        author: "user".into(),
        timestamp: ts,
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text("alpha beta", ROLE_USER));
    ev.response.custom_metadata = json!({"event_key": "event_val"})
        .as_object()
        .cloned()
        .unwrap();
    add_events(&svc, "s1", vec![ev]).await;

    let got = search(&svc, "alpha").await;

    let want = vec![MemoryEntry {
        id: "ev-1".into(),
        author: "user".into(),
        timestamp: Some(ts),
        content: Some(Content::from_text("alpha beta", ROLE_USER)),
        custom_metadata: json!({"event_key": "event_val"})
            .as_object()
            .cloned()
            .unwrap(),
    }];
    assert_eq!(got.memories, want, "SearchMemory() mismatch");
}

#[tokio::test]
async fn test_add_events_to_memory_custom_metadata_ignored() {
    let svc = InMemoryMemoryService::new();
    let mut ev = Event {
        id: "ev-1".into(),
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text("alpha beta", ROLE_USER));
    ev.response.custom_metadata = json!({"event_key": "event_val"})
        .as_object()
        .cloned()
        .unwrap();
    add_events_to_memory(
        &svc,
        AddEventsRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "s1".into(),
            events: vec![ev],
            custom_metadata: json!({"call_key": "ignored"}).as_object().cloned().unwrap(),
        },
    )
    .await
    .unwrap();

    let got = search(&svc, "alpha").await;

    assert_eq!(got.memories.len(), 1, "SearchMemory() entries");
    assert_eq!(
        got.memories[0].custom_metadata,
        json!({"event_key": "event_val"})
            .as_object()
            .cloned()
            .unwrap(),
        "Entry.CustomMetadata mismatch"
    );
}

#[tokio::test]
async fn test_add_events_to_memory_default_bucket_does_not_collide_with_unknown_session_id() {
    let svc = InMemoryMemoryService::new();
    add_events(
        &svc,
        "",
        vec![memory_text_event("shared-id", &["default copy"])],
    )
    .await;
    add_events(
        &svc,
        "__unknown_session_id__",
        vec![memory_text_event("shared-id", &["sentinel copy"])],
    )
    .await;
    add_session(
        &svc,
        "",
        vec![memory_text_event("shared-id", &["empty session copy"])],
    )
    .await;

    let got = search(&svc, "copy").await;

    assert_eq!(
        memory_texts(&got),
        ["default copy", "sentinel copy", "empty session copy"],
        "SearchMemory() texts mismatch"
    );
}

/// How a test stores its first event in bucket "s1" (Go `viaSession` / `viaEvents`).
#[derive(Clone, Copy)]
enum Via {
    Session,
    Events,
}

/// Go reuseAfter: stores `first` in bucket "s1" `via`, then adds an event to "s1" with the
/// same ID "dup" and `text`.
async fn reuse_after(svc: &InMemoryMemoryService, via: Via, first: Event, text: &str) {
    match via {
        Via::Session => add_session(svc, "s1", vec![first]).await,
        Via::Events => add_events(svc, "s1", vec![first]).await,
    }
    add_events(svc, "s1", vec![memory_text_event("dup", &[text])]).await;
}

// parity: adapted: Go's "only nil parts" cases have no counterpart (a Content holds Part values); they run as content with no parts
#[tokio::test]
async fn test_add_events_to_memory_deduplication_scope_and_wordless_reservations() {
    let with_content = |content: Content| {
        let mut e = Event {
            id: "dup".into(),
            ..Default::default()
        };
        e.response.content = Some(content);
        e
    };
    let blob_only = || {
        with_content(Content::from_parts(
            vec![Part::from_bytes(b"abc".to_vec().into(), "text/plain")],
            ROLE_USER,
        ))
    };
    let thought_only = || {
        with_content(Content::from_parts(
            vec![Part {
                text: "hidden".into(),
                thought: true,
                ..Default::default()
            }],
            ROLE_MODEL,
        ))
    };
    let empty_content = || with_content(Content::default());

    // (name, how the first event is stored, first event, reused text, want)
    let reserved: [(&str, Via, Event, &str, &[&str]); 8] = [
        (
            "blob-only session event reserves id",
            Via::Session,
            blob_only(),
            "reused text",
            &[],
        ),
        (
            "blob-only added event reserves id",
            Via::Events,
            blob_only(),
            "reused text",
            &[],
        ),
        (
            "thought-only session event reserves id",
            Via::Session,
            thought_only(),
            "reused text",
            &[],
        ),
        (
            "thought-only added event reserves id",
            Via::Events,
            thought_only(),
            "reused text",
            &[],
        ),
        (
            "session event without parts does not reserve id",
            Via::Session,
            empty_content(),
            "reused after nil part",
            &["reused after nil part"],
        ),
        (
            "added event without parts does not reserve id",
            Via::Events,
            empty_content(),
            "reused after nil part",
            &["reused after nil part"],
        ),
        (
            "session event with only nil parts does not reserve id",
            Via::Session,
            empty_content(),
            "reused after nil part",
            &["reused after nil part"],
        ),
        (
            "added event with only nil parts does not reserve id",
            Via::Events,
            empty_content(),
            "reused after nil part",
            &["reused after nil part"],
        ),
    ];
    for (name, via, first, text, want) in reserved {
        let svc = InMemoryMemoryService::new();
        reuse_after(&svc, via, first, text).await;
        assert_eq!(
            memory_texts(&search(&svc, "reused").await),
            want,
            "{name}: SearchMemory(\"reused\") texts mismatch"
        );
    }

    // duplicates in one call
    let svc = InMemoryMemoryService::new();
    add_events(
        &svc,
        "s1",
        vec![
            memory_text_event("dup", &["in-call first"]),
            memory_text_event("dup", &["in-call second"]),
        ],
    )
    .await;
    assert_eq!(
        memory_texts(&search(&svc, "call").await),
        ["in-call first"],
        "duplicates in one call"
    );

    // duplicate of an added session event
    let svc = InMemoryMemoryService::new();
    add_session(
        &svc,
        "s1",
        vec![memory_text_event("dup", &["from session"])],
    )
    .await;
    add_events(&svc, "s1", vec![memory_text_event("dup", &["from events"])]).await;
    assert_eq!(
        memory_texts(&search(&svc, "from").await),
        ["from session"],
        "duplicate of an added session event"
    );

    // same id in other bucket
    let svc = InMemoryMemoryService::new();
    add_events(&svc, "s1", vec![memory_text_event("dup", &["s1 text"])]).await;
    add_events(&svc, "s2", vec![memory_text_event("dup", &["s2 text"])]).await;
    assert_eq!(
        memory_texts(&search(&svc, "text").await),
        ["s1 text", "s2 text"],
        "same id in other bucket"
    );

    // empty ids are not duplicates
    let svc = InMemoryMemoryService::new();
    add_events(
        &svc,
        "s1",
        vec![
            memory_text_event("", &["empty id 1"]),
            memory_text_event("", &["empty id 2"]),
        ],
    )
    .await;
    assert_eq!(
        memory_texts(&search(&svc, "empty").await),
        ["empty id 1", "empty id 2"],
        "empty ids are not duplicates"
    );
}

#[tokio::test]
async fn test_add_events_to_memory_append_replace_and_empty_bucket_order() {
    // append after session then replace
    {
        let svc = InMemoryMemoryService::new();
        add_session(&svc, "s1", vec![memory_text_event("1", &["note one"])]).await;
        add_events(&svc, "s1", vec![memory_text_event("2", &["note two"])]).await;
        assert_eq!(
            memory_ids(&search(&svc, "note").await),
            ["1", "2"],
            "SearchMemory() after append IDs mismatch"
        );

        add_session(
            &svc,
            "s1",
            vec![
                memory_text_event("dup", &["note a"]),
                memory_text_event("dup", &["note b"]),
            ],
        )
        .await;
        assert_eq!(
            memory_texts(&search(&svc, "note").await),
            ["note a", "note b"],
            "SearchMemory() after replace texts mismatch"
        );
    }
    // zero-event call fixes named bucket position
    {
        let svc = InMemoryMemoryService::new();
        add_events(&svc, "early", vec![]).await;
        add_events(&svc, "", vec![memory_text_event("def", &["tie"])]).await;
        add_session(&svc, "late", vec![memory_text_event("late", &["tie"])]).await;
        add_events(&svc, "early", vec![memory_text_event("early", &["tie"])]).await;
        assert_eq!(
            memory_ids(&search(&svc, "tie").await),
            ["early", "def", "late"],
            "zero-event call fixes named bucket position"
        );
    }
    // zero-event call fixes default bucket position
    {
        let svc = InMemoryMemoryService::new();
        add_events(&svc, "", vec![]).await;
        add_session(&svc, "late", vec![memory_text_event("late", &["tie"])]).await;
        add_events(&svc, "", vec![memory_text_event("def", &["tie"])]).await;
        assert_eq!(
            memory_ids(&search(&svc, "tie").await),
            ["def", "late"],
            "zero-event call fixes default bucket position"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_in_memory_service_search_memory_concurrent_add_events() {
    let svc = Arc::new(InMemoryMemoryService::new());

    // Each worker adds 5 events to its own session and 5 to the shared default bucket, all
    // carrying "x" and the worker's own word "w<i>". Together the workers put 20 events in the
    // default bucket.
    const WORKERS: usize = 4;
    let mut tasks = Vec::new();
    for i in 0..WORKERS {
        let w = svc.clone();
        tasks.push(tokio::spawn(async move {
            for j in 0..5 {
                for sid in [format!("s{i}"), String::new()] {
                    let id = format!("{sid}-{i}-{j}");
                    add_events_to_memory(
                        w.as_ref(),
                        AddEventsRequest {
                            app_name: "app".into(),
                            user_id: "user".into(),
                            session_id: sid,
                            events: vec![memory_text_event(&id, &[&format!("x w{i}")])],
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                }
            }
        }));
        let a = svc.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..25 {
                let id = format!("sess{i}");
                a.add_session_to_memory(&make_session(
                    "app",
                    "user",
                    &id,
                    vec![memory_text_event(&id, &["x"])],
                ))
                .await
                .unwrap();
            }
        }));
        let r = svc.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..25 {
                search(r.as_ref(), "x").await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    // The sessions alone hold 4 matching events; the rest come from the concurrent
    // add_events_to_memory calls.
    assert_eq!(
        search(svc.as_ref(), "x").await.memories.len(),
        10,
        "SearchMemory() entries"
    );
    for i in 0..WORKERS {
        let mut got = memory_ids(&search(svc.as_ref(), &format!("w{i}")).await);
        got.sort();
        let mut want: Vec<String> = (0..5).map(|j| format!("-{i}-{j}")).collect();
        want.extend((0..5).map(|j| format!("s{i}-{i}-{j}")));
        assert_eq!(got, want, "SearchMemory(\"w{i}\") IDs mismatch");
    }
}

/// Go eventlessService: a MemoryService without an EventAdder.
struct EventlessService;

#[async_trait::async_trait]
impl MemoryService for EventlessService {
    async fn add_session_to_memory(&self, _: &dyn Session) -> Result<(), MemoryError> {
        Ok(())
    }
    async fn search_memory(&self, _: SearchRequest) -> Result<SearchResponse, MemoryError> {
        Ok(SearchResponse::default())
    }
}

// parity: adapted: Go's errors.Is(err, ErrAddEventsUnsupported) and errors.Is(err, errors.ErrUnsupported) are the MemoryError::AddEventsUnsupported variant
#[tokio::test]
async fn test_add_events_to_memory_unsupported_service() {
    let err = add_events_to_memory(
        &EventlessService,
        AddEventsRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "s1".into(),
            events: vec![memory_text_event("e1", &["hello"])],
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(err, Err(MemoryError::AddEventsUnsupported)),
        "AddEventsToMemory() error = {err:?}, want MemoryError::AddEventsUnsupported"
    );
}

// The unsupported error must point a caller at the fallback method, in the package's error
// style, and not at adk-python's add_session_to_memory.
#[tokio::test]
async fn test_add_events_to_memory_unsupported_service_names_go_fallback() {
    let err = add_events_to_memory(
        &EventlessService,
        AddEventsRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            events: vec![memory_text_event("e1", &["hello"])],
            ..Default::default()
        },
    )
    .await
    .expect_err("AddEventsToMemory() error = nil, want an unsupported error");

    let msg = err.to_string();
    assert!(
        msg.starts_with("memory: "),
        "error = {msg:?}, want prefix \"memory: \""
    );
    assert!(
        msg.contains("AddSessionToMemory"),
        "error = {msg:?}, want it to name \"AddSessionToMemory\""
    );
    assert!(
        !msg.contains("add_session_to_memory"),
        "error = {msg:?}, want no Python name"
    );
    assert!(
        !msg.ends_with('.'),
        "error = {msg:?}, want no trailing full stop"
    );
}

// parity: skipped TestAddEventsToMemory_NilRequests: Go-only nil mechanics (a nil service, request or session); Rust passes references and values, so none can be nil
