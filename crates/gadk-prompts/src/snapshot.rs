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

//! An immutable view of every prompt.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::model::{Label, PromptVersion, Selector};
use crate::store::StoreError;

/// One prompt's versions and labels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptEntry {
    /// The versions, by version number.
    pub versions: BTreeMap<u32, Arc<PromptVersion>>,
    /// The labels and the version each points at.
    pub labels: BTreeMap<Label, u32>,
}

/// Every prompt, by name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptSnapshot {
    /// The prompts, by name.
    pub prompts: BTreeMap<String, PromptEntry>,
}

impl PromptSnapshot {
    /// The version `sel` selects in prompt `name`.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown name (empty selector), label, or version.
    pub fn resolve(&self, name: &str, sel: &Selector) -> Result<Arc<PromptVersion>, StoreError> {
        let entry = self.prompts.get(name).ok_or_else(|| StoreError::NotFound {
            name: name.to_owned(),
            selector: String::new(),
        })?;
        let not_found = || StoreError::NotFound {
            name: name.to_owned(),
            selector: sel.to_string(),
        };
        let version = match sel {
            Selector::Latest => *entry.versions.keys().next_back().ok_or_else(not_found)?,
            Selector::Version(n) => *n,
            Selector::Label(l) => *entry.labels.get(l).ok_or_else(not_found)?,
        };
        entry.versions.get(&version).cloned().ok_or_else(not_found)
    }

    /// The highest version of `name`, if the prompt exists.
    pub fn latest_version(&self, name: &str) -> Option<u32> {
        self.prompts
            .get(name)
            .and_then(|e| e.versions.keys().next_back().copied())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model::{VersionMeta, content_hash};

    fn v(n: u32) -> Arc<PromptVersion> {
        Arc::new(PromptVersion {
            name: "p".into(),
            version: n,
            template: format!("t{n}"),
            config: None,
            meta: VersionMeta {
                created_at: chrono::Utc::now(),
                author: String::new(),
                message: String::new(),
                hash: content_hash(&format!("t{n}"), None),
            },
        })
    }

    #[test]
    fn resolve_each_selector_and_miss() {
        let mut e = PromptEntry::default();
        e.versions.insert(1, v(1));
        e.versions.insert(2, v(2));
        e.labels.insert(Label::production(), 1);
        let mut s = PromptSnapshot::default();
        s.prompts.insert("p".into(), e);
        assert_eq!(s.resolve("p", &Selector::Latest).unwrap().version, 2);
        assert_eq!(s.resolve("p", &Selector::Version(1)).unwrap().version, 1);
        assert_eq!(
            s.resolve("p", &Selector::Label(Label::production()))
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            s.resolve("q", &Selector::Latest),
            Err(StoreError::NotFound {
                name: "q".into(),
                selector: String::new()
            })
        );
        assert_eq!(
            s.resolve("p", &Selector::Label(Label::staging())),
            Err(StoreError::NotFound {
                name: "p".into(),
                selector: "staging".into()
            })
        );
        assert_eq!(
            s.resolve("p", &Selector::Version(9)),
            Err(StoreError::NotFound {
                name: "p".into(),
                selector: "v9".into()
            })
        );
        assert_eq!(s.latest_version("p"), Some(2));
    }
}
