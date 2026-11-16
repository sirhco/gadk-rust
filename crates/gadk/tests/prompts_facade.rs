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
#![cfg(feature = "prompts-firestore")]

use std::sync::Arc;

#[tokio::test]
async fn facade_exposes_prompts_and_firestore() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(gadk::prompts::FileStore::new(dir.path()));
    let registry = gadk::prompts::PromptRegistry::builder(store)
        .refresh_interval(std::time::Duration::ZERO)
        .load()
        .await
        .unwrap();
    assert!(registry.snapshot().prompts.is_empty());
    let r =
        gadk::prompts_firestore::FirestoreStore::new(gadk::prompts_firestore::FirestoreConfig {
            project: "p".into(),
            endpoint: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        });
    assert!(r.is_ok());
}
