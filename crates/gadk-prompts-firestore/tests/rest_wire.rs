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

//! Wire tests of the Firestore prompt store's REST requests against a fake Firestore.

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

#[tokio::test]
async fn store_contract_firestore_rollouts() {
    let fakes = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run_rollouts(|| {
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
async fn slash_names_are_single_escaped_in_request_bodies() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    s.create_version(nv("a/b", "x")).await.unwrap();
    let reqs = fake.requests.lock().unwrap();
    // Transactional reads go through batchGet; no request carries a doc id in its URL path.
    assert!(reqs.iter().all(|r| r.method != "GET"));
    assert!(
        reqs.iter().all(|r| !r.path.contains("/prompts/")),
        "{:?}",
        reqs.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    let get = reqs
        .iter()
        .find(|r| r.path.ends_with(":batchGet"))
        .expect("a batchGet");
    let docs = get.json()["documents"].clone();
    assert!(
        docs[0].as_str().unwrap().ends_with("/prompts/a%2Fb"),
        "{docs}"
    );
    assert!(get.json()["transaction"].is_string());
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

#[test]
fn bad_root_segments_are_invalid() {
    for root in [
        "envs/../x",
        "./prod",
        "envs/pr od",
        "envs/pr%2Fod",
        "envs/pr?od",
        "envs//prod/x",
        "envs/prød",
    ] {
        let r = FirestoreStore::new(config("http://127.0.0.1:1".into(), Some(root)));
        assert!(matches!(r, Err(StoreError::Invalid(_))), "{root}");
    }
    for root in ["envs/prod", "a.b/c_d-1", "/envs/prod/"] {
        let r = FirestoreStore::new(config("http://127.0.0.1:1".into(), Some(root)));
        assert!(r.is_ok(), "{root}");
    }
}

#[tokio::test]
async fn load_all_missing_database_is_http_404_with_body() {
    let body = r#"{"error":{"code":404,"status":"NOT_FOUND","message":"The database (default) does not exist for project proj"}}"#;
    let server = FakeServer::always(FakeResponse::json_status(404, body));
    let s = FirestoreStore::new(config(server.url(), Some("envs/prod"))).unwrap();
    match s.load_all().await {
        Err(StoreError::Http {
            status: 404,
            body: b,
        }) => {
            assert!(b.contains("does not exist"), "{b}");
        }
        other => panic!("want Http 404, got {other:?}"),
    }
}

#[tokio::test]
async fn load_all_failed_precondition_is_http_400_with_body() {
    let body = r#"{"error":{"code":400,"status":"FAILED_PRECONDITION","message":"The query requires an index. You can create it here: https://console.firebase.google.com/x"}}"#;
    let server = FakeServer::always(FakeResponse::json_status(400, body));
    let s = FirestoreStore::new(config(server.url(), None)).unwrap();
    match s.load_all().await {
        Err(StoreError::Http {
            status: 400,
            body: b,
        }) => {
            assert!(b.contains("The query requires an index"), "{b}");
        }
        other => panic!("want Http 400, got {other:?}"),
    }
}

/// `(method-ish suffix, body)` of every request since the last call, oldest first.
fn drain(fake: &fake_firestore::Fake) -> Vec<(String, serde_json::Value)> {
    let mut reqs = fake.requests.lock().unwrap();
    let out = reqs
        .iter()
        .map(|r| {
            let op = r.path.rsplit(':').next().unwrap_or_default().to_owned();
            (op, r.json())
        })
        .collect();
    reqs.clear();
    out
}

/// Deletes every document of the fake (a store wiped behind the cache's back).
fn wipe(fake: &fake_firestore::Fake) {
    fake.state.lock().unwrap().docs.clear();
}

#[tokio::test]
async fn create_version_reporting_says_whether_a_version_was_written() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    let (v1, created) = s.create_version_reporting(nv("p", "a")).await.unwrap();
    assert!(created);
    assert_eq!(v1.version, 1);
    let (same, created) = s.create_version_reporting(nv("p", "a")).await.unwrap();
    assert!(!created);
    assert_eq!(same, v1);
    let (v2, created) = s.create_version_reporting(nv("p", "b")).await.unwrap();
    assert!(created);
    assert_eq!(v2.version, 2);
    // No whole-store read on the write path.
    assert!(drain(&fake).iter().all(|(op, _)| op != "runQuery"));
}

#[tokio::test]
async fn load_all_fetches_only_new_versions() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.create_version(nv("p", "b")).await.unwrap();
    s.create_version(nv("q", "x")).await.unwrap();
    let first = s.load_all().await.unwrap();
    assert_eq!(first.prompts["p"].versions.len(), 2);
    assert_eq!(first.prompts["q"].versions.len(), 1);

    s.create_version(nv("p", "c")).await.unwrap();
    drain(&fake);
    let second = s.load_all().await.unwrap();
    assert_eq!(
        second.prompts["p"]
            .versions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(second.prompts["p"].versions[&3].template, "c");
    assert_eq!(second.prompts["q"].versions.len(), 1);
    let reqs = drain(&fake);
    let ops: Vec<&str> = reqs.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(ops, ["runQuery", "batchGet"], "{reqs:?}");
    assert_eq!(
        reqs[0].1["structuredQuery"]["from"][0]["collectionId"],
        "prompts"
    );
    assert_eq!(
        reqs[0].1["structuredQuery"]["from"][0]["allDescendants"],
        false
    );
    let docs = reqs[1].1["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1, "{docs:?}");
    assert!(docs[0].as_str().unwrap().ends_with("/prompts/p/versions/3"));
    assert!(reqs[1].1.get("transaction").is_none());
}

#[tokio::test]
async fn unchanged_store_costs_one_read_call() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.create_version(nv("p", "b")).await.unwrap();
    let first = s.load_all().await.unwrap();
    drain(&fake);
    let second = s.load_all().await.unwrap();
    assert_eq!(first, second);
    let reqs = drain(&fake);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(reqs[0].0, "runQuery");
}

#[tokio::test]
async fn load_all_drops_removed_prompts_and_refetches_recreated_ones() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "old")).await.unwrap();
    s.create_version(nv("keep", "k")).await.unwrap();
    assert_eq!(s.load_all().await.unwrap().prompts.len(), 2);

    // Removed: gone from the snapshot.
    fake.state
        .lock()
        .unwrap()
        .docs
        .retain(|name, _| !name.contains("/prompts/p"));
    let snap = s.load_all().await.unwrap();
    assert!(!snap.prompts.contains_key("p"));
    assert_eq!(snap.prompts["keep"].versions.len(), 1);

    // Recreated with the same latest_version but other content: the dropped cache must not
    // answer for it.
    s.create_version(nv("p", "new")).await.unwrap();
    let snap = s.load_all().await.unwrap();
    assert_eq!(snap.prompts["p"].versions[&1].template, "new");
}

#[tokio::test]
async fn load_all_refetches_when_latest_version_goes_backwards() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.create_version(nv("p", "b")).await.unwrap();
    assert_eq!(s.load_all().await.unwrap().prompts["p"].versions.len(), 2);

    wipe(&fake);
    s.create_version(nv("p", "fresh")).await.unwrap();
    drain(&fake);
    let snap = s.load_all().await.unwrap();
    let p = &snap.prompts["p"];
    assert_eq!(p.versions.keys().copied().collect::<Vec<_>>(), vec![1]);
    assert_eq!(p.versions[&1].template, "fresh");
    let reqs = drain(&fake);
    assert_eq!(reqs[1].1["documents"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn load_all_skips_a_missing_version_document() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.create_version(nv("p", "b")).await.unwrap();
    fake.state
        .lock()
        .unwrap()
        .docs
        .retain(|name, _| !name.ends_with("/versions/1"));
    let snap = s.load_all().await.unwrap();
    assert_eq!(
        snap.prompts["p"]
            .versions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[tokio::test]
async fn load_all_sees_a_recreated_prompt_with_more_versions() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a1")).await.unwrap();
    s.create_version(nv("p", "a2")).await.unwrap();
    assert_eq!(s.load_all().await.unwrap().prompts["p"].versions.len(), 2);

    wipe(&fake);
    for t in ["b1", "b2", "b3"] {
        s.create_version(nv("p", t)).await.unwrap();
    }
    let snap = s.load_all().await.unwrap();
    let p = &snap.prompts["p"];
    let got: Vec<(u32, &str)> = p
        .versions
        .iter()
        .map(|(n, v)| (*n, v.template.as_str()))
        .collect();
    assert_eq!(got, [(1, "b1"), (2, "b2"), (3, "b3")]);
}

#[tokio::test]
async fn load_all_heals_a_gap_in_the_cache() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    s.create_version(nv("p", "b")).await.unwrap();
    // v1 is unreadable for one poll: the cache ends up holding only v2.
    let v1 = {
        let mut st = fake.state.lock().unwrap();
        let key = st.docs.keys().find(|k| k.ends_with("/versions/1")).cloned();
        let key = key.unwrap();
        (key.clone(), st.docs.remove(&key).unwrap())
    };
    let snap = s.load_all().await.unwrap();
    assert_eq!(
        snap.prompts["p"]
            .versions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
    fake.state.lock().unwrap().docs.insert(v1.0, v1.1);

    drain(&fake);
    let snap = s.load_all().await.unwrap();
    assert_eq!(
        snap.prompts["p"]
            .versions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let reqs = drain(&fake);
    let docs = reqs[1].1["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1, "{docs:?}");
    assert!(docs[0].as_str().unwrap().ends_with("/versions/1"));
}

#[tokio::test]
async fn load_all_never_reuses_the_cache_without_a_create_time() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), Some("envs/t"))).unwrap();
    s.create_version(nv("p", "old")).await.unwrap();
    fake.state.lock().unwrap().created.clear(); // createTime now empty
    assert_eq!(
        s.load_all().await.unwrap().prompts["p"].versions[&1].template,
        "old"
    );

    wipe(&fake);
    s.create_version(nv("p", "new")).await.unwrap();
    fake.state.lock().unwrap().created.clear();
    assert_eq!(
        s.load_all().await.unwrap().prompts["p"].versions[&1].template,
        "new"
    );
}

#[tokio::test]
async fn a_prompt_document_that_fails_to_decode_rolls_back_the_transaction() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    {
        let mut st = fake.state.lock().unwrap();
        let (fields, _) = st
            .docs
            .iter_mut()
            .find(|(name, _)| name.ends_with("/prompts/p"))
            .unwrap()
            .1;
        fields["latest_version"] = serde_json::json!({"stringValue": "not a number"});
    }
    drain(&fake);
    assert!(matches!(
        s.create_version(nv("p", "b")).await,
        Err(StoreError::Decode(_))
    ));
    let ops: Vec<String> = drain(&fake).into_iter().map(|(op, _)| op).collect();
    assert!(ops.iter().any(|op| op == "rollback"), "{ops:?}");
    assert!(ops.iter().all(|op| op != "commit"), "{ops:?}");
    assert!(matches!(
        s.set_label("p", &Label::production(), 1, None).await,
        Err(StoreError::Decode(_))
    ));
    let ops: Vec<String> = drain(&fake).into_iter().map(|(op, _)| op).collect();
    assert!(ops.iter().any(|op| op == "rollback"), "{ops:?}");
    assert!(ops.iter().all(|op| op != "commit"), "{ops:?}");
}

#[tokio::test]
async fn a_latest_version_document_that_fails_to_decode_rolls_back_the_transaction() {
    let fake = fake_firestore::start();
    let s = FirestoreStore::new(config(fake.server.url(), None)).unwrap();
    s.create_version(nv("p", "a")).await.unwrap();
    {
        let mut st = fake.state.lock().unwrap();
        let (fields, _) = st
            .docs
            .iter_mut()
            .find(|(name, _)| name.ends_with("/prompts/p/versions/1"))
            .unwrap()
            .1;
        fields["version"] = serde_json::json!({"stringValue": "not a number"});
    }
    drain(&fake);
    assert!(matches!(
        s.create_version(nv("p", "b")).await,
        Err(StoreError::Decode(_))
    ));
    let ops: Vec<String> = drain(&fake).into_iter().map(|(op, _)| op).collect();
    assert!(ops.iter().any(|op| op == "rollback"), "{ops:?}");
    assert!(ops.iter().all(|op| op != "commit"), "{ops:?}");
}
