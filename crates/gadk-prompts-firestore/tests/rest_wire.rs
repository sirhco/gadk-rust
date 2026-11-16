// Copyright 2026 Chris Olson
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

mod fake_firestore;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gadk_model::rest::test_util::{FakeResponse, FakeServer};
use gadk_platform::env::MapEnv;
use gadk_prompts::{Label, NewVersion, PromptStore, StoreError};
use gadk_prompts_firestore::{FirestoreConfig, FirestoreStore, encode_doc_id};

fn config(endpoint: String, root: Option<&str>) -> FirestoreConfig {
    FirestoreConfig {
        project: "proj".into(),
        root: root.map(str::to_owned),
        endpoint: Some(endpoint),
        env: Arc::new(MapEnv(HashMap::new())),
        // No credentials: an explicit endpoint with no ADC available must still work in tests,
        // so the store treats a configured endpoint like the emulator (no auth) when
        // `credentials` is None AND `FIRESTORE_EMULATOR_HOST`/endpoint is http://.
        ..FirestoreConfig::default()
    }
}

fn nv(name: &str, t: &str) -> NewVersion {
    NewVersion {
        name: name.into(),
        template: t.into(),
        config: None,
        author: String::new(),
        message: String::new(),
    }
}

#[tokio::test]
async fn store_contract_firestore() {
    let fakes = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run(|| {
        let fake = fake_firestore::start();
        let store: Arc<dyn PromptStore> =
            Arc::new(FirestoreStore::new(config(fake.server.url(), Some("envs/test"))).unwrap());
        fakes.lock().unwrap().push(fake);
        async move { store }
    })
    .await;
}

#[test]
fn encodes_doc_ids() {
    assert_eq!(encode_doc_id("a/b"), "a%2Fb");
    assert_eq!(encode_doc_id("50%/x"), "50%25%2Fx");
}

#[tokio::test]
async fn slash_names_are_double_escaped_in_url_paths() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    s.create_version(nv("a/b", "x")).await.unwrap();
    let reqs = fake.requests.lock().unwrap();
    let get = reqs.iter().find(|r| r.method == "GET").expect("a GET");
    assert!(get.path.ends_with("/prompts/a%252Fb"), "{}", get.path);
    let commit = reqs.iter().find(|r| r.path.ends_with(":commit")).unwrap();
    assert!(
        commit.body_text().contains("/prompts/a%2Fb"),
        "{}",
        commit.body_text()
    );
}

// Real time: the fake is a real socket server and the store sets a response-header timeout,
// which paused time would fire.
#[tokio::test]
async fn aborted_commits_retry_then_conflict() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    fake.abort_next.store(2, Ordering::SeqCst);
    assert_eq!(s.create_version(nv("p", "a")).await.unwrap().version, 1);
    fake.abort_next.store(3, Ordering::SeqCst);
    assert_eq!(
        s.create_version(nv("p", "b")).await,
        Err(StoreError::Conflict {
            expected: None,
            actual: None
        })
    );
    fake.abort_next.store(3, Ordering::SeqCst);
    assert!(matches!(
        s.set_label("p", &Label::production(), 1, None).await,
        Err(StoreError::Conflict { .. })
    ));
}

#[tokio::test]
async fn cas_mismatch_sends_no_commit() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.set_label("p", &Label::production(), 1, None)
        .await
        .unwrap();
    let before = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.path.ends_with(":commit"))
        .count();
    let r = s.set_label("p", &Label::production(), 1, Some(None)).await;
    assert_eq!(
        r,
        Err(StoreError::Conflict {
            expected: None,
            actual: Some(1)
        })
    );
    let after = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.path.ends_with(":commit"))
        .count();
    assert_eq!(before, after);
}

#[tokio::test]
async fn server_error_maps_to_http() {
    let server = FakeServer::always(FakeResponse::json_status(
        500,
        r#"{"error":{"code":500,"status":"INTERNAL","message":"x"}}"#,
    ));
    let s = FirestoreStore::new(config(server.url(), None)).unwrap();
    assert!(matches!(
        s.load_all().await,
        Err(StoreError::Http { status: 500, .. })
    ));
}

#[test]
fn odd_segment_root_is_invalid() {
    let r = FirestoreStore::new(config("http://127.0.0.1:1".into(), Some("envs")));
    assert!(matches!(r, Err(StoreError::Invalid(_))));
    let r = FirestoreStore::new(config("http://127.0.0.1:1".into(), Some("envs/prod")));
    assert!(r.is_ok());
}
