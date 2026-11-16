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

//! The storage interface.

use async_trait::async_trait;

use crate::model::{Label, NewVersion, PromptVersion};
use crate::snapshot::PromptSnapshot;

/// A store or lookup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// No prompt `name`, or no version or label `selector` in it (`selector` is empty when the
    /// name itself is unknown).
    #[error("not found: {name}{}", if selector.is_empty() { String::new() } else { format!("@{selector}") })]
    NotFound { name: String, selector: String },
    /// A compare-and-set or concurrent write lost: `expected` was required, `actual` was found.
    #[error("conflict: expected {expected:?}, found {actual:?}")]
    Conflict {
        expected: Option<u32>,
        actual: Option<u32>,
    },
    /// Invalid input (name, label, store URL, root path).
    #[error("invalid: {0}")]
    Invalid(String),
    /// A filesystem failure.
    #[error("io: {0}")]
    Io(String),
    /// A non-success HTTP reply from a remote store.
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    /// Stored data that cannot be decoded.
    #[error("decode: {0}")]
    Decode(String),
}

/// Where prompts are kept. Implementations must make `create_version`, `set_label` and
/// `remove_label` atomic with respect to each other.
#[async_trait]
pub trait PromptStore: Send + Sync {
    /// Every prompt with all its versions and labels.
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError>;
    /// Appends version N+1 of `new.name` (creating the prompt at version 1). If the content
    /// hash equals the current latest version's, returns that version unchanged instead.
    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError>;
    /// Points `label` at `version` (compare-and-set): `expected = None` skips the check,
    /// `Some(None)` requires the label to be unset, `Some(Some(v))` requires it to point at `v`;
    /// a mismatch is `Conflict { expected: <inner>, actual }`. Unknown name or version:
    /// `NotFound`.
    async fn set_label(
        &self,
        name: &str,
        label: &Label,
        version: u32,
        expected: Option<Option<u32>>,
    ) -> Result<(), StoreError>;
    /// Removes `label`. Unknown name or label: `NotFound`.
    async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError>;
}
