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

//! Port of Go memory/inmemory_search_test.go.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, Utc};
use gadk_genai::{Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_memory::{InMemoryMemoryService, MemoryService, SearchRequest};
use gadk_session::{Event, Session, SessionError, State, zero_time};
use serde_json::Value;

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

/// Port of Go memoryTextEvent: one text part per string, role user.
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
async fn search(s: &InMemoryMemoryService, q: &str) -> Vec<gadk_memory::MemoryEntry> {
    s.search_memory(SearchRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        query: q.into(),
    })
    .await
    .unwrap()
    .memories
}
async fn ids(s: &InMemoryMemoryService, q: &str) -> Vec<String> {
    search(s, q).await.into_iter().map(|m| m.id).collect()
}

#[tokio::test]
async fn test_in_memory_service_search_memory_ranking() {
    let cases: Vec<(&str, Vec<&str>, &str, Vec<&str>)> = vec![
        (
            "most query words first",
            vec!["deploy ready", "ready", "deploy status ready", "unrelated"],
            "deploy status ready",
            vec!["2", "0", "1"],
        ),
        (
            "repeated words count once",
            vec!["ready ready ready", "deploy status", "ready deploy"],
            "READY ready ready deploy status",
            vec!["1", "2", "0"],
        ),
        (
            "token and substring matches count once",
            vec!["caf\u{e9}", "deploy ready", "CAF\u{c9} deploy"],
            "caf\u{e9} deploy ready",
            vec!["1", "2", "0"],
        ),
        (
            "combine token and non ASCII substring scores",
            vec![
                "deploy",
                "\u{6211}\u{559c}\u{6b22}\u{673a}\u{5668}\u{5b66}\u{4e60} deploy",
                "\u{6211}\u{559c}\u{6b22}\u{673a}\u{5668}\u{5b66}\u{4e60}",
            ],
            "deploy \u{673a}\u{5668}\u{5b66}\u{4e60}",
            vec!["1", "0", "2"],
        ),
        ("empty query", vec!["ready"], "", vec![]),
        ("no matching words", vec!["ready"], "missing", vec![]),
    ];
    for (name, texts, query, want) in cases {
        let s = InMemoryMemoryService::new();
        let events = texts
            .iter()
            .enumerate()
            .map(|(i, t)| memory_text_event(&i.to_string(), &[*t]))
            .collect();
        s.add_session_to_memory(&make_session("app", "user", "session", events))
            .await
            .unwrap();
        assert_eq!(ids(&s, query).await, want, "{name}");
    }
}

#[tokio::test]
async fn test_in_memory_service_search_memory_limit_and_session_order() {
    let s = InMemoryMemoryService::new();
    // Session IDs and timestamps run backwards so neither can define tie order.
    for i in 0..12i64 {
        let events = (0..2i64)
            .map(|j| {
                let mut e = memory_text_event(&(2 * i + j).to_string(), &["note about work"]);
                e.timestamp = DateTime::from_timestamp(100 - 2 * i - j, 0).unwrap();
                e
            })
            .collect();
        s.add_session_to_memory(&make_session("app", "user", &(12 - i).to_string(), events))
            .await
            .unwrap();
    }
    s.add_session_to_memory(&make_session(
        "app",
        "user",
        "best",
        vec![memory_text_event("best", &["backlog note about work"])],
    ))
    .await
    .unwrap();
    let want = vec!["best", "0", "1", "2", "3", "4", "5", "6", "7", "8"];
    for i in 0..20 {
        assert_eq!(ids(&s, "work backlog note").await, want, "iteration {i}");
    }
}

#[tokio::test]
async fn test_in_memory_service_search_memory_replaced_session_order() {
    let s = InMemoryMemoryService::new();
    let sessions = vec![
        make_session(
            "app",
            "user",
            "z",
            vec![memory_text_event("old", &["note"])],
        ),
        make_session("app", "user", "empty", vec![]),
        make_session(
            "app",
            "user",
            "a",
            vec![memory_text_event("last", &["note"])],
        ),
        make_session(
            "app",
            "user",
            "z",
            vec![memory_text_event("replacement", &["note"])],
        ),
        make_session(
            "app",
            "user",
            "empty",
            vec![memory_text_event("middle", &["note"])],
        ),
    ];
    for sess in &sessions {
        s.add_session_to_memory(sess).await.unwrap();
    }
    for i in 0..20 {
        assert_eq!(
            ids(&s, "note").await,
            vec!["replacement", "middle", "last"],
            "iteration {i}"
        );
    }
}

#[tokio::test]
async fn test_in_memory_service_search_memory_non_ascii() {
    let cases: Vec<(&str, Vec<&str>, &str, usize)> = vec![
        (
            "Chinese substring",
            vec!["\u{6211}\u{559c}\u{6b22}\u{673a}\u{5668}\u{5b66}\u{4e60}"],
            "\u{673a}\u{5668}\u{5b66}\u{4e60}",
            1,
        ),
        (
            "Chinese no match",
            vec!["\u{6211}\u{559c}\u{6b22}\u{673a}\u{5668}\u{5b66}\u{4e60}"],
            "\u{5929}\u{6c14}\u{9884}\u{62a5}",
            0,
        ),
        (
            "Japanese substring",
            vec!["\u{79c1}\u{306e}\u{540d}\u{524d}\u{306f}\u{592a}\u{90ce}\u{3067}\u{3059}"],
            "\u{592a}\u{90ce}",
            1,
        ),
        (
            "non ASCII case folding",
            vec!["CAF\u{c9}TERIA"],
            "caf\u{e9}",
            1,
        ),
        (
            "substring in later part",
            vec![
                "hello",
                "\u{6211}\u{559c}\u{6b22}\u{673a}\u{5668}\u{5b66}\u{4e60}",
            ],
            "\u{673a}\u{5668}\u{5b66}\u{4e60}",
            1,
        ),
        (
            "do not join words across parts",
            vec!["\u{673a}\u{5668}", "\u{5b66}\u{4e60}"],
            "\u{673a}\u{5668}\u{5b66}\u{4e60}",
            0,
        ),
        (
            "ASCII partial word does not match",
            vec!["Python"],
            "thon",
            0,
        ),
        ("ASCII token matches", vec!["Python"], "python", 1),
        ("empty text", vec![""], "\u{673a}\u{5668}", 0),
    ];
    for (name, parts, query, want) in cases {
        let s = InMemoryMemoryService::new();
        s.add_session_to_memory(&make_session(
            "app",
            "user",
            "session",
            vec![memory_text_event("event", &parts)],
        ))
        .await
        .unwrap();
        assert_eq!(search(&s, query).await.len(), want, "{name}");
    }
}

#[tokio::test]
async fn test_in_memory_service_search_memory_skips_thought_parts() {
    let thought = Part {
        text: "secret plan".into(),
        thought: true,
        ..Default::default()
    };
    let visible = Part::from_text("deploy ready");
    let mut thought_only = Event {
        id: "thought-only".into(),
        ..Default::default()
    };
    thought_only.response.content = Some(Content::from_parts(vec![thought.clone()], ROLE_MODEL));
    let mut mixed = Event {
        id: "mixed".into(),
        ..Default::default()
    };
    mixed.response.content = Some(Content::from_parts(
        vec![thought, visible.clone()],
        ROLE_MODEL,
    ));
    let s = InMemoryMemoryService::new();
    s.add_session_to_memory(&make_session(
        "app",
        "user",
        "session",
        vec![thought_only, mixed],
    ))
    .await
    .unwrap();

    assert_eq!(
        search(&s, "secret plan").await.len(),
        0,
        "matched thought text"
    );

    let got = search(&s, "deploy").await;
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].content,
        Some(Content::from_parts(vec![visible], ROLE_MODEL))
    );
}
