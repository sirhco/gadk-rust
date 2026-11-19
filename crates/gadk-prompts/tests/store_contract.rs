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
    FileStore, Label, NewVersion, PromptSnapshot, PromptStore, PromptVersion, StoreError,
};

/// Implements only the required methods, so `create_version_reporting` is the trait default.
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
