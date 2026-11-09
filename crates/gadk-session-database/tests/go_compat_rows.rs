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
//! gadk writes the rows ADK-Go writes, byte for byte, and reads ADK-Go's rows (spec R6, R7).

mod common;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use common::{dump_rows, load_rows, memory_service, testdata};
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use gadk_session_database::DatabaseSessionService;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
struct Script {
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    op: String,
    now: Option<DateTime<Utc>>,
    app_name: String,
    user_id: String,
    session_id: String,
    state: Option<Map<String, Value>>,
    event: Option<Event>,
}

#[derive(Deserialize)]
struct Expected {
    get: Vec<View>,
    list: Vec<View>,
}

#[derive(Deserialize)]
struct View {
    app_name: String,
    user_id: String,
    id: String,
    state: Map<String, Value>,
    last_update_time: DateTime<Utc>,
    #[serde(default)]
    events: Vec<Event>,
}

/// A listed session as `(user_id, id, state, last_update_time)`.
type Listed = (String, String, Map<String, Value>, DateTime<Utc>);

fn state_of(s: &dyn Session) -> Map<String, Value> {
    s.state().all().into_iter().collect()
}

/// Replays `script.json` exactly as the Go probe did: `create` at the step's clock, `append`
/// after a fresh `get`, `delete`.
async fn replay() -> DatabaseSessionService {
    let clock = Arc::new(parking_lot::Mutex::new(Utc::now()));
    let c = clock.clone();
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap()
        .with_platform(Platform::default().with_time(move || *c.lock()));
    svc.auto_migrate().await.unwrap();
    let script: Script = serde_json::from_str(&testdata("script.json")).unwrap();
    for step in script.steps {
        match step.op.as_str() {
            "create" => {
                *clock.lock() = step.now.unwrap();
                svc.create(CreateRequest {
                    app_name: step.app_name,
                    user_id: step.user_id,
                    session_id: Some(step.session_id),
                    state: step.state,
                })
                .await
                .unwrap();
            }
            "append" => {
                let s = svc
                    .get(GetRequest {
                        app_name: step.app_name,
                        user_id: step.user_id,
                        session_id: step.session_id,
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                let mut ev = step.event.unwrap();
                svc.append_event(s.as_ref(), &mut ev).await.unwrap();
            }
            "delete" => svc
                .delete(DeleteRequest {
                    app_name: step.app_name,
                    user_id: step.user_id,
                    session_id: step.session_id,
                })
                .await
                .unwrap(),
            other => panic!("unknown op {other:?}"),
        }
    }
    svc
}

#[tokio::test]
async fn sqlite_rows_match_go() {
    let svc = replay().await;
    assert_eq!(
        dump_rows(svc.sqlite_pool().unwrap()).await,
        testdata("rows.sqlite.sql")
    );
}

#[tokio::test]
async fn reads_go_written_database() {
    let svc = memory_service().await;
    load_rows(svc.sqlite_pool().unwrap(), &testdata("rows.sqlite.sql")).await;
    let expected: Expected = serde_json::from_str(&testdata("expected.json")).unwrap();

    for want in &expected.get {
        let got = svc
            .get(GetRequest {
                app_name: want.app_name.clone(),
                user_id: want.user_id.clone(),
                session_id: want.id.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(state_of(got.as_ref()), want.state, "{} state", want.id);
        assert_eq!(
            got.last_update_time(),
            want.last_update_time,
            "{} last update",
            want.id
        );
        assert_eq!(got.events(), want.events, "{} events", want.id);
    }

    let mut listed = svc
        .list(ListRequest {
            app_name: "app".into(),
            user_id: String::new(),
        })
        .await
        .unwrap();
    listed.sort_by(|a, b| (a.user_id(), a.id()).cmp(&(b.user_id(), b.id())));
    let got: Vec<Listed> = listed
        .iter()
        .map(|s| {
            (
                s.user_id().to_string(),
                s.id().to_string(),
                state_of(s.as_ref()),
                s.last_update_time(),
            )
        })
        .collect();
    let want: Vec<Listed> = expected
        .list
        .iter()
        .map(|v| {
            (
                v.user_id.clone(),
                v.id.clone(),
                v.state.clone(),
                v.last_update_time,
            )
        })
        .collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn reads_go_times_written_with_another_offset() {
    let svc = memory_service().await;
    let pool = svc.sqlite_pool().unwrap();
    load_rows(pool, &testdata("rows.sqlite.sql")).await;
    sqlx::query(
        "UPDATE `sessions` SET `update_time` = '2026-03-04 07:06:12.5+02:00' WHERE `id` = 's2'",
    )
    .execute(pool)
    .await
    .unwrap();
    let got = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u2".into(),
            session_id: "s2".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        got.last_update_time(),
        DateTime::parse_from_rfc3339("2026-03-04T05:06:12.5Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}
