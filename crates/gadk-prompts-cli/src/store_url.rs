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

//! `--store` URLs.

use std::sync::Arc;

use gadk_prompts::{FileStore, PromptStore, StoreError};

/// `file://<dir>` or `firestore://<project>/<database>[/<root…>]`.
pub(crate) fn open(url: &str) -> Result<Arc<dyn PromptStore>, StoreError> {
    if let Some(dir) = url.strip_prefix("file://") {
        if dir.is_empty() {
            return Err(StoreError::Invalid(
                "file:// store needs a directory".into(),
            ));
        }
        return Ok(Arc::new(FileStore::new(dir)));
    }
    if let Some(rest) = url.strip_prefix("firestore://") {
        return firestore(rest);
    }
    Err(StoreError::Invalid(format!(
        "unsupported store {url:?}: want file://<dir> or firestore://<project>/<database>[/<root>]"
    )))
}

#[cfg(feature = "firestore")]
fn firestore(rest: &str) -> Result<Arc<dyn PromptStore>, StoreError> {
    let mut parts = rest.splitn(3, '/');
    let project = parts.next().unwrap_or_default();
    let database = parts
        .next()
        .filter(|d| !d.is_empty())
        .unwrap_or("(default)");
    let root = parts.next().map(str::to_owned).filter(|r| !r.is_empty());
    if project.is_empty() {
        return Err(StoreError::Invalid(
            "firestore:// store needs a project".into(),
        ));
    }
    Ok(Arc::new(gadk_prompts_firestore::FirestoreStore::new(
        gadk_prompts_firestore::FirestoreConfig {
            project: project.into(),
            database: database.into(),
            root,
            ..Default::default()
        },
    )?))
}

#[cfg(not(feature = "firestore"))]
fn firestore(_: &str) -> Result<Arc<dyn PromptStore>, StoreError> {
    Err(StoreError::Invalid(
        "this build has no Firestore support (feature `firestore`)".into(),
    ))
}
