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

//! Contract tests every `PromptStore` implementation must pass.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use gadk_prompts::{
    FileStore, Label, NewVersion, PromptSnapshot, PromptStore, PromptVersion, Rollout, StoreError,
};

/// Implements only the required methods, so `create_version_reporting` and the rollout methods
/// are the trait defaults.
struct DefaultsOnly(FileStore);

#[async_trait]
impl PromptStore for DefaultsOnly {
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
        self.0.load_all().await
    }
    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
        self.0.create_version(new).await
    }
    async fn set_label(
        &self,
        name: &str,
        label: &Label,
        version: u32,
        expected: Option<Option<u32>>,
    ) -> Result<(), StoreError> {
        self.0.set_label(name, label, version, expected).await
    }
    async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
        self.0.remove_label(name, label).await
    }
}

#[tokio::test]
async fn store_contract_default_reporting() {
    let dirs = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run(|| {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn PromptStore> = Arc::new(DefaultsOnly(FileStore::new(dir.path())));
        dirs.lock().unwrap().push(dir);
        async move { store }
    })
    .await;
}

#[tokio::test]
async fn store_contract_file_store() {
    let dirs = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run(|| {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(dir.path()));
        dirs.lock().unwrap().push(dir); // keep alive for the whole run
        async move { store }
    })
    .await;
}

#[tokio::test]
async fn rollout_defaults_on_a_store_without_rollout_support() {
    let dir = tempfile::tempdir().unwrap();
    let s = DefaultsOnly(FileStore::new(dir.path()));
    s.create_version(NewVersion {
        name: "p".into(),
        template: "a".into(),
        config: None,
        author: String::new(),
        message: String::new(),
    })
    .await
    .unwrap();
    let prod = Label::production();
    s.set_label("p", &prod, 1, None).await.unwrap();
    assert!(s.load_rollouts().await.unwrap().is_empty());
    let (snap, rollouts) = s.load_all_with_rollouts().await.unwrap();
    assert_eq!(snap, s.load_all().await.unwrap());
    assert!(rollouts.is_empty());
    let unsupported = Err(StoreError::Invalid(
        "rollouts are not supported by this store".into(),
    ));
    let r = Rollout::new(1, 10).unwrap();
    assert_eq!(s.set_rollout("p", &prod, Some(r)).await, unsupported);
    assert_eq!(s.set_rollout("p", &prod, None).await, unsupported);
}

#[tokio::test]
async fn store_contract_file_store_rollouts() {
    let dirs = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run_rollouts(|| {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(dir.path()));
        dirs.lock().unwrap().push(dir); // keep alive for the whole run
        async move { store }
    })
    .await;
}

#[tokio::test]
async fn set_rollout_if_default_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let s = DefaultsOnly(FileStore::new(dir.path()));
    s.create_version(NewVersion {
        name: "p".into(),
        template: "a".into(),
        config: None,
        author: String::new(),
        message: String::new(),
    })
    .await
    .unwrap();
    let prod = Label::production();
    s.set_label("p", &prod, 1, None).await.unwrap();
    let unsupported = Err(StoreError::Invalid(
        "rollouts are not supported by this store".into(),
    ));
    let r = Rollout::new(1, 10).unwrap();
    assert_eq!(
        s.set_rollout_if("p", &prod, Some(r), 1, None).await,
        unsupported
    );
    assert_eq!(
        s.set_rollout_if("p", &prod, None, 1, Some(r)).await,
        unsupported
    );
}

#[tokio::test]
async fn store_contract_file_store_rollout_cas() {
    let dirs = std::sync::Mutex::new(Vec::new());
    gadk_prompts::testsuite::run_rollout_cas(|| {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(dir.path()));
        dirs.lock().unwrap().push(dir); // keep alive for the whole run
        async move { store }
    })
    .await;
}
