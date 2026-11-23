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

//! Live test of the Firestore prompt store against a real project (ignored by default).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gadk_prompts::PromptStore;
use gadk_prompts_firestore::{FirestoreConfig, FirestoreStore};

/// Runs the contract suites (`run`, `run_rollouts`, then `run_rollout_cas`) against the Firestore
/// emulator: `FIRESTORE_EMULATOR_HOST=localhost:8080 cargo test -p gadk-prompts-firestore --test live live_emulator_contract -- --ignored`.
/// Each scenario uses its own root so runs do not collide.
#[tokio::test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST; run with -- --ignored"]
async fn live_emulator_contract() {
    assert!(
        std::env::var("FIRESTORE_EMULATOR_HOST").is_ok(),
        "set FIRESTORE_EMULATOR_HOST"
    );
    let n = std::sync::atomic::AtomicUsize::new(0);
    let run = std::process::id();
    let factory = || {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let store: Arc<dyn PromptStore> = Arc::new(
            FirestoreStore::new(FirestoreConfig {
                project: "gadk-test".into(),
                root: Some(format!("runs/r{run}-{i}")),
                ..FirestoreConfig::default()
            })
            .unwrap(),
        );
        async move { store }
    };
    gadk_prompts::testsuite::run(&factory).await;
    gadk_prompts::testsuite::run_rollouts(&factory).await;
    gadk_prompts::testsuite::run_rollout_cas(&factory).await;
}

/// Runs the contract suites (`run`, `run_rollouts`, then `run_rollout_cas`) against a real
/// Firestore project with Application Default Credentials: `GADK_FIRESTORE_PROJECT=my-project
/// cargo test -p gadk-prompts-firestore --test live live_project_contract -- --ignored`
/// (`GADK_FIRESTORE_DATABASE` defaults to `(default)`).
/// Each scenario writes under its own root, printed as `root: …`; delete the `runs/gadk-test-*`
/// documents afterwards.
#[tokio::test]
#[ignore = "needs GADK_FIRESTORE_PROJECT (real project, ADC); run with -- --ignored"]
async fn live_project_contract() {
    assert!(
        std::env::var("FIRESTORE_EMULATOR_HOST").is_err(),
        "unset FIRESTORE_EMULATOR_HOST: live_project_contract must hit a real project"
    );
    let project = std::env::var("GADK_FIRESTORE_PROJECT")
        .expect("set GADK_FIRESTORE_PROJECT to a real Google Cloud project");
    let database =
        std::env::var("GADK_FIRESTORE_DATABASE").unwrap_or_else(|_| "(default)".to_owned());
    let n = std::sync::atomic::AtomicUsize::new(0);
    let pid = std::process::id();
    let factory = || {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = format!("runs/gadk-test-{pid}-{i}");
        eprintln!("root: {root}");
        let store: Arc<dyn PromptStore> = Arc::new(
            FirestoreStore::new(FirestoreConfig {
                project: project.clone(),
                database: database.clone(),
                root: Some(root),
                ..FirestoreConfig::default()
            })
            .unwrap(),
        );
        async move { store }
    };
    gadk_prompts::testsuite::run(&factory).await;
    gadk_prompts::testsuite::run_rollouts(&factory).await;
    gadk_prompts::testsuite::run_rollout_cas(&factory).await;
}
