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

#![cfg(feature = "postgres")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The shared suite and delete/migrate behaviour on PostgreSQL. Runs only when
//! `GADK_TEST_POSTGRES_URL` is set; otherwise each test reports itself skipped.

mod common;

use std::sync::Arc;

use chrono::Utc;
use gadk_session::testsuite::{SuiteOptions, run};
use gadk_session::{CreateRequest, DeleteRequest, Event, SessionService};

fn url() -> Option<String> {
    let url = std::env::var("GADK_TEST_POSTGRES_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("skipped: GADK_TEST_POSTGRES_URL is unset");
    }
    url
}

#[tokio::test]
async fn shared_suite_on_postgres() {
    let Some(url) = url() else { return };
    let schemas = Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let (url, schemas) = (url.clone(), schemas.clone());
        run(
            SuiteOptions {
                supports_user_provided_session_id: true,
                provides_server_assigned_event_id: false,
                checks_requested_auth_configs: true,
                app_name: None,
            },
            move || {
                let (url, schemas) = (url.clone(), schemas.clone());
                async move {
                    let (svc, schema) = common::pg_service(&url).await;
                    schemas.lock().push(schema);
                    Arc::new(svc) as Arc<dyn SessionService>
                }
            },
        )
        .await;
    }
    let names: Vec<String> = schemas.lock().clone();
    for schema in names {
        common::drop_pg_schema(&url, &schema).await;
    }
}

#[tokio::test]
async fn delete_cascades_events() {
    let Some(url) = url() else { return };
    let (svc, schema) = common::pg_service(&url).await;
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
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(svc.pg_pool().unwrap())
        .await
        .unwrap();
    common::drop_pg_schema(&url, &schema).await;
    assert_eq!(
        n, 0,
        "ON DELETE CASCADE removes the session's events on PostgreSQL"
    );
}

#[tokio::test]
async fn auto_migrate_is_idempotent_on_postgres() {
    let Some(url) = url() else { return };
    let (svc, schema) = common::pg_service(&url).await;
    svc.auto_migrate().await.unwrap();
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'events' ORDER BY ordinal_position",
    )
    .fetch_all(svc.pg_pool().unwrap())
    .await
    .unwrap();
    common::drop_pg_schema(&url, &schema).await;
    assert_eq!(cols.len(), 27, "{cols:?}");
    assert_eq!(cols.first().map(String::as_str), Some("id"));
    assert_eq!(cols.last().map(String::as_str), Some("interrupted"));
}

#[tokio::test]
async fn append_event_locks_app_then_user_rows() {
    let Some(url) = url() else { return };
    let (svc, schema) = common::pg_service(&url).await;
    let (svc, log) = svc.with_statement_log();
    let s = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    log.lock().clear();
    let delta = serde_json::json!({"app:a": 1, "user:u": 2, "s": 3});
    let mut ev = Event {
        id: "e1".into(),
        author: "user".into(),
        timestamp: Utc::now(),
        actions: gadk_session::EventActions {
            state_delta: delta.as_object().cloned(),
            ..Default::default()
        },
        ..Default::default()
    };
    svc.append_event(s.as_ref(), &mut ev).await.unwrap();
    let got = log.lock().clone();
    common::drop_pg_schema(&url, &schema).await;
    assert_eq!(
        got,
        [
            "insert app_states on conflict do nothing",
            "select app_states for update",
            "insert user_states on conflict do nothing",
            "select user_states for update",
        ]
    );
}
