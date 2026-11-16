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

use std::sync::Arc;

use gadk_prompts::PromptStore;
use gadk_prompts_firestore::{FirestoreConfig, FirestoreStore};

/// Runs the contract suite against the Firestore emulator:
/// `FIRESTORE_EMULATOR_HOST=localhost:8080 cargo test -p gadk-prompts-firestore --test live -- --ignored`.
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
    gadk_prompts::testsuite::run(|| {
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
    })
    .await;
}
