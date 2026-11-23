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
use crate::rollout::{Rollout, RolloutSnapshot};
use crate::snapshot::PromptSnapshot;

/// The `Invalid` message of [`PromptStore::set_rollout`] on a store without rollout support.
pub(crate) const UNSUPPORTED: &str = "rollouts are not supported by this store";

/// A store or lookup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// No prompt `name`, or no version or label `selector` in it (`selector` is empty when the
    /// name itself is unknown).
    #[error("not found: {name}{}", if selector.is_empty() { String::new() } else { format!("@{selector}") })]
    NotFound {
        /// The prompt name.
        name: String,
        /// The version or label looked up; empty when the prompt itself is unknown.
        selector: String,
    },
    /// A compare-and-set or concurrent write lost: `expected` was required, `actual` was found.
    #[error("{}", conflict_message(*expected, *actual))]
    Conflict {
        /// The version that was required; `None` means unset (both `None`: a concurrent write lost).
        expected: Option<u32>,
        /// The version found; `None` means unset.
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
    Http {
        /// The HTTP status code.
        status: u16,
        /// The reply body.
        body: String,
    },
    /// Stored data that cannot be decoded.
    #[error("decode: {0}")]
    Decode(String),
}

/// `conflict: expected v1, found unset`; `conflict: concurrent write; retry` when neither side
/// is known (a lost transaction).
fn conflict_message(expected: Option<u32>, actual: Option<u32>) -> String {
    let show = |v: Option<u32>| v.map_or_else(|| "unset".to_owned(), |n| format!("v{n}"));
    if expected.is_none() && actual.is_none() {
        "conflict: concurrent write; retry".to_owned()
    } else {
        format!(
            "conflict: expected {}, found {}",
            show(expected),
            show(actual)
        )
    }
}

/// Where prompts are kept. Implementations must make `create_version`, `set_label` and
/// `remove_label` atomic with respect to each other.
///
/// A store that supports rollouts overrides [`set_rollout`](Self::set_rollout),
/// [`set_rollout_if`](Self::set_rollout_if), [`load_rollouts`](Self::load_rollouts) and
/// [`load_all_with_rollouts`](Self::load_all_with_rollouts), makes `set_rollout` and
/// `set_rollout_if` atomic with the other writes, removes a label's rollout in the same write
/// whenever `set_label` or `remove_label` succeeds on that label (also when `set_label` keeps
/// its version), and keeps every rollout in `create_version`.
#[async_trait]
pub trait PromptStore: Send + Sync {
    /// Every prompt with all its versions and labels.
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError>;
    /// Appends version N+1 of `new.name` (creating the prompt at version 1). If the content
    /// hash equals the current latest version's, returns that version unchanged instead.
    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError>;
    /// Like [`create_version`](Self::create_version), and also reports whether a version was
    /// written: the `bool` is `true` when a new version was appended and `false` when the
    /// content hash matched the latest version and that version was returned unchanged.
    ///
    /// The default implementation reads the whole store with
    /// [`load_all`](Self::load_all) to learn the latest version before the write. Stores should
    /// override it with a native check; [`FileStore`](crate::FileStore) and the Firestore store do.
    async fn create_version_reporting(
        &self,
        new: NewVersion,
    ) -> Result<(PromptVersion, bool), StoreError> {
        let before = self.load_all().await?.latest_version(&new.name);
        let v = self.create_version(new).await?;
        let created = before != Some(v.version);
        Ok((v, created))
    }
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
    /// Every rollout, by prompt name and label.
    ///
    /// The default implementation returns no rollouts (a store without rollout support).
    async fn load_rollouts(&self) -> Result<RolloutSnapshot, StoreError> {
        Ok(RolloutSnapshot::default())
    }
    /// Every prompt and every rollout, read together so the rollouts match the prompts.
    ///
    /// The default implementation calls [`load_all`](Self::load_all), then
    /// [`load_rollouts`](Self::load_rollouts). Stores that support rollouts should override it
    /// to read both in one pass.
    async fn load_all_with_rollouts(
        &self,
    ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
        let prompts = self.load_all().await?;
        let rollouts = self.load_rollouts().await?;
        Ok((prompts, rollouts))
    }
    /// Starts, replaces (`Some`) or removes (`None`) the rollout on `label` of prompt `name`.
    ///
    /// `Some(r)`: `Invalid` when `r` is outside [`Rollout::new`]'s bounds; `NotFound` with an
    /// empty selector for an unknown prompt, with the label as selector for an unknown label,
    /// and with selector `v<N>` when `r.version` is not a version of the prompt; `Invalid` when
    /// `r.version` is the label's version. `None`: `NotFound` for an unknown prompt or label, and
    /// `Ok` when the label has no rollout.
    ///
    /// The default implementation returns
    /// `Invalid("rollouts are not supported by this store")`.
    async fn set_rollout(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        let _ = (name, label, rollout);
        Err(StoreError::Invalid(UNSUPPORTED.to_owned()))
    }

    /// [`set_rollout`](Self::set_rollout) with a compare-and-set on what the caller read: the
    /// write happens only while `label` points at `expected_version` and its rollout equals
    /// `expected_rollout` (version and percent; `None` = no rollout). Atomic with the other
    /// writes.
    ///
    /// Errors, checked in this order: `Invalid` when `rollout` is `Some` and outside
    /// [`Rollout::new`]'s bounds; `NotFound` with an empty selector for an unknown prompt;
    /// `Conflict { expected: Some(expected_version), actual }` when the label is missing
    /// (`actual: None`) or points at another version (`actual` = that version);
    /// `Conflict { expected: expected_rollout.map(|r| r.version), actual }` (`actual` = the
    /// current rollout's version, or `None` without one) when the rollout differs, so a
    /// percent-only difference reports equal versions; then every error of `set_rollout`.
    ///
    /// The default implementation returns
    /// `Invalid("rollouts are not supported by this store")`.
    async fn set_rollout_if(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
        expected_version: u32,
        expected_rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        let _ = (name, label, rollout, expected_version, expected_rollout);
        Err(StoreError::Invalid(UNSUPPORTED.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::StoreError;

    #[test]
    fn conflict_display() {
        let c = |expected, actual| StoreError::Conflict { expected, actual }.to_string();
        assert_eq!(c(Some(1), Some(2)), "conflict: expected v1, found v2");
        assert_eq!(c(None, Some(1)), "conflict: expected unset, found v1");
        assert_eq!(c(Some(3), None), "conflict: expected v3, found unset");
        assert_eq!(c(None, None), "conflict: concurrent write; retry");
    }
}
