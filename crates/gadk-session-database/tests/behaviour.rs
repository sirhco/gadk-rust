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

#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Behaviour beyond the Go tests: delete semantics, validation texts, concurrent appends to
//! shared state, temp: keys, partial events and timestamp precision.

mod common;

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use common::memory_service;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, EventActions, GetRequest, InMemorySessionService,
    ListRequest, SessionError, SessionService,
};
use gadk_session_database::DatabaseSessionService;
use serde_json::{Map, json};

#[tokio::test]
async fn sqlite_delete_removes_events_like_go() {
    let svc = memory_service().await;
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut ev = Event {
        id: "e1".into(),
        author: "user".into(),
        timestamp: Utc::now(),
        ..Default::default()
    };
    svc.append_event(s.as_ref(), &mut ev).await.unwrap();
    svc.delete(DeleteRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: s.id().into(),
    })
    .await
    .unwrap();
    let pool = svc.sqlite_pool().unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        (sessions, events),
        (0, 0),
        "ADK-Go deletes the session's events explicitly (#1774), even with sqlite foreign keys off"
    );
}

#[tokio::test]
async fn validation_texts_are_gos() {
    let svc = memory_service().await;
    let msg = |e: SessionError| match e {
        SessionError::InvalidRequest(m) => m,
        other => panic!("want InvalidRequest, got {other:?}"),
    };
    let e = svc
        .create(CreateRequest {
            app_name: "app".into(),
            ..Default::default()
        })
        .await
        .err()
        .unwrap();
    assert_eq!(msg(e), "app_name and user_id are required");
    let e = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .err()
        .unwrap();
    assert_eq!(
        msg(e),
        "app_name, user_id, session_id are required, got app_name: \"app\", user_id: \"u\", session_id: \"\""
    );
    let e = svc.list(ListRequest::default()).await.err().unwrap();
    assert_eq!(msg(e), "app_name is required, got app_name: \"\"");
    let e = svc
        .delete(DeleteRequest {
            app_name: "app".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        msg(e),
        "app_name, user_id, session_id are required, got app_name: \"app\", user_id: \"\", session_id: \"\""
    );
}

#[tokio::test]
async fn append_event_rejects_a_foreign_session() {
    let svc = memory_service().await;
    let other = InMemorySessionService::new();
    let s = other
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut ev = Event {
        id: "e".into(),
        ..Default::default()
    };
    let err = svc.append_event(s.as_ref(), &mut ev).await.unwrap_err();
    assert!(matches!(err, SessionError::Backend(_)), "{err:?}");
    assert!(
        err.to_string().starts_with("unexpected session type"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_keep_every_shared_key() {
    const SESSIONS: usize = 8;
    const EVENTS: usize = 5;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("race.db").display());
    let svc = Arc::new(DatabaseSessionService::connect(&url).await.unwrap());
    svc.auto_migrate().await.unwrap();
    let mut tasks = Vec::new();
    for i in 0..SESSIONS {
        let svc = svc.clone();
        tasks.push(tokio::spawn(async move {
            let s = svc
                .create(CreateRequest {
                    app_name: "app".into(),
                    user_id: "u".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            for j in 0..EVENTS {
                let mut delta = Map::new();
                delta.insert(format!("app:a{i}_{j}"), json!(1));
                delta.insert(format!("user:u{i}_{j}"), json!(1));
                let mut ev = Event {
                    id: format!("e{i}_{j}"),
                    author: "user".into(),
                    timestamp: Utc::now() + Duration::seconds(1),
                    actions: EventActions {
                        state_delta: Some(delta),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                svc.append_event(s.as_ref(), &mut ev).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let keys: Vec<String> = s.state().all().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys.iter().filter(|k| k.starts_with("app:")).count(),
        SESSIONS * EVENTS,
        "{keys:?}"
    );
    assert_eq!(
        keys.iter().filter(|k| k.starts_with("user:")).count(),
        SESSIONS * EVENTS,
        "{keys:?}"
    );
}

#[tokio::test]
async fn temp_keys_reach_the_session_state_but_not_the_stored_event() {
    let svc = memory_service().await;
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut delta = Map::new();
    delta.insert("temp:k1".into(), json!("v1"));
    delta.insert("sk".into(), json!("v2"));
    let mut ev = Event {
        id: "e1".into(),
        author: "user".into(),
        timestamp: Utc::now(),
        actions: EventActions {
            state_delta: Some(delta),
            ..Default::default()
        },
        ..Default::default()
    };
    svc.append_event(s.as_ref(), &mut ev).await.unwrap();
    assert_eq!(
        s.state().get("temp:k1").ok(),
        Some(json!("v1")),
        "temp: key missing from the caller's session state"
    );
    assert_eq!(s.state().get("sk").ok(), Some(json!("v2")));
    let pool = svc.sqlite_pool().unwrap();
    let actions: Vec<u8> = sqlx::query_scalar("SELECT actions FROM events WHERE id = 'e1'")
        .fetch_one(pool)
        .await
        .unwrap();
    let actions: serde_json::Value = serde_json::from_slice(&actions).unwrap();
    assert_eq!(
        actions["stateDelta"],
        json!({"sk": "v2"}),
        "stored event delta: {actions}"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(state, r#"{"sk":"v2"}"#);
}

#[tokio::test]
async fn partial_event_is_not_stored() {
    let svc = memory_service().await;
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let before = s.last_update_time();
    let mut ev = Event {
        author: "agent".into(),
        timestamp: Utc::now() + Duration::seconds(5),
        ..Default::default()
    };
    ev.response.partial = true;
    svc.append_event(s.as_ref(), &mut ev).await.unwrap();
    assert!(
        ev.id.is_empty(),
        "a partial event is returned untouched, as in Go"
    );
    assert!(
        s.events().is_empty(),
        "partial event reached the caller's session"
    );
    assert_eq!(s.last_update_time(), before);
    let pool = svc.sqlite_pool().unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(events, 0, "partial event was stored");
}

#[tokio::test]
async fn append_path_times_are_truncated_to_microseconds_and_create_time_is_not() {
    let t0 =
        Utc.with_ymd_and_hms(2026, 7, 17, 10, 0, 0).unwrap() + Duration::nanoseconds(123_456_789);
    let svc = memory_service()
        .await
        .with_platform(Platform::default().with_time(move || t0));
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let pool = svc.sqlite_pool().unwrap();
    let times = || async {
        sqlx::query_as::<_, (String, String)>("SELECT create_time, update_time FROM sessions")
            .fetch_one(pool)
            .await
            .unwrap()
    };
    let full = "2026-07-17 10:00:00.123456789+00:00".to_string();
    assert_eq!(
        times().await,
        (full.clone(), full.clone()),
        "Create stores the full nanoseconds"
    );

    let t1 = t0 + Duration::seconds(1);
    let mut delta = Map::new();
    delta.insert("app:a".into(), json!(1));
    delta.insert("user:b".into(), json!(2));
    let mut ev = Event {
        id: "e1".into(),
        author: "user".into(),
        timestamp: t1,
        actions: EventActions {
            state_delta: Some(delta),
            ..Default::default()
        },
        ..Default::default()
    };
    svc.append_event(s.as_ref(), &mut ev).await.unwrap();
    let want = "2026-07-17 10:00:01.123456+00:00".to_string();
    assert_eq!(
        ev.timestamp,
        t1 - Duration::nanoseconds(789),
        "the caller's event is truncated"
    );
    assert_eq!(s.last_update_time(), ev.timestamp);
    assert_eq!(
        times().await,
        (full, want.clone()),
        "sessions.create_time keeps nanoseconds; update_time is truncated"
    );
    let ts: String = sqlx::query_scalar("SELECT timestamp FROM events")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(ts, want, "events.timestamp");
    let app: String = sqlx::query_scalar("SELECT update_time FROM app_states")
        .fetch_one(pool)
        .await
        .unwrap();
    let user: String = sqlx::query_scalar("SELECT update_time FROM user_states")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        (app, user),
        (want.clone(), want),
        "app_states/user_states update_time"
    );
}
