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

//! Port of Go internal/memory/memory_test.go.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use gadk_agent::Memory;
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchResponse};
use gadk_model::LlmResponse;
use gadk_session::{
    CreateRequest, Event, GetRequest, InMemorySessionService, Session, SessionService,
};
use pretty_assertions::assert_eq;

use crate::memory::ScopedMemory;

/// Go `memory.Memory{Service, AppName, UserID}`.
fn scoped_memory(service: Arc<dyn MemoryService>, app: &str, user: &str) -> ScopedMemory {
    ScopedMemory::new(service, app, user)
}

/// Creates the session, appends `events`, and re-reads it so the returned value holds them.
async fn session_with(
    svc: &InMemorySessionService,
    app: &str,
    user: &str,
    id: &str,
    events: Vec<Event>,
) -> Arc<dyn Session> {
    let s = svc
        .create(CreateRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: Some(id.into()),
            state: None,
        })
        .await
        .expect("Failed to create session");
    for mut e in events {
        svc.append_event(s.as_ref(), &mut e)
            .await
            .expect("Failed to append event");
    }
    svc.get(GetRequest {
        app_name: app.into(),
        user_id: user.into(),
        session_id: id.into(),
        ..Default::default()
    })
    .await
    .unwrap()
}

fn event(id: &str, ts: chrono::DateTime<Utc>, author: &str, content: Content) -> Event {
    Event {
        id: id.into(),
        timestamp: ts,
        author: author.into(),
        response: LlmResponse {
            content: Some(content),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn test_memory_add_and_search() {
    let (app, user, sid) = ("testApp", "testUser", "sess1");
    let memory = scoped_memory(Arc::new(InMemoryMemoryService::new()), app, user);
    let c1 = Content::from_text("The quick brown fox", ROLE_USER);
    let c2 = Content::from_text("jumps over the lazy dog", ROLE_USER);
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2025, 1, 1, 10, 5, 0).unwrap();
    let svc = InMemorySessionService::new();
    let s = session_with(
        &svc,
        app,
        user,
        sid,
        vec![
            event("event-1", t1, "user1", c1.clone()),
            event("event-2", t2, "user1", c2.clone()),
        ],
    )
    .await;
    memory
        .add_session_to_memory(s)
        .await
        .expect("AddSessionToMemory failed");

    let e1 = MemoryEntry {
        id: "event-1".into(),
        content: Some(c1),
        author: "user1".into(),
        timestamp: Some(t1),
        ..Default::default()
    };
    let e2 = MemoryEntry {
        id: "event-2".into(),
        content: Some(c2),
        author: "user1".into(),
        timestamp: Some(t2),
        ..Default::default()
    };
    let cases: [(&str, &str, Vec<MemoryEntry>); 6] = [
        ("match first entry", "fox", vec![e1.clone()]),
        ("match second entry", "DOG", vec![e2.clone()]),
        (
            "match both entries (any word)",
            "quick dog",
            vec![e1.clone(), e2.clone()],
        ),
        ("match word in both", "the", vec![e1.clone(), e2.clone()]),
        ("no match", "unrelated", vec![]),
        ("empty query", "", vec![]),
    ];
    for (name, query, want) in cases {
        let got = memory
            .search_memory(query)
            .await
            .unwrap_or_else(|e| panic!("{name}: SearchMemory({query:?}) failed: {e}"));
        assert_eq!(got, SearchResponse { memories: want }, "{name}");
    }
}

#[tokio::test]
async fn test_memory_search_no_data() {
    let memory = scoped_memory(
        Arc::new(InMemoryMemoryService::new()),
        "testApp",
        "testUser",
    );
    let got = memory
        .search_memory("any query")
        .await
        .expect("SearchMemory() failed");
    assert!(got.memories.is_empty());
}

#[tokio::test]
async fn test_memory_search_isolation() {
    let service: Arc<dyn MemoryService> = Arc::new(InMemoryMemoryService::new());
    let memory1 = scoped_memory(service.clone(), "testApp", "user1");
    let memory2 = scoped_memory(service, "testApp", "user2");
    let c1 = Content::from_text("Content for user1", ROLE_USER);
    let c2 = Content::from_text("Content for user2", ROLE_USER);
    let svc = InMemorySessionService::new();
    let s1 = session_with(
        &svc,
        "testApp",
        "user1",
        "sess1",
        vec![event("", Utc::now(), "user1", c1.clone())],
    )
    .await;
    let s2 = session_with(
        &svc,
        "testApp",
        "user2",
        "sess2",
        vec![event("", Utc::now(), "user2", c2.clone())],
    )
    .await;
    memory1
        .add_session_to_memory(s1)
        .await
        .expect("AddSessionToMemory(user1) failed");
    memory2
        .add_session_to_memory(s2)
        .await
        .expect("AddSessionToMemory(user2) failed");

    for (who, memory, want) in [("user1", &memory1, c1), ("user2", &memory2, c2)] {
        let got = memory
            .search_memory("Content")
            .await
            .unwrap_or_else(|e| panic!("SearchMemory for {who} failed: {e}"));
        assert_eq!(got.memories.len(), 1, "{who}: want 1 memory");
        assert_eq!(got.memories[0].content, Some(want), "{who}: wrong content");
    }
}
