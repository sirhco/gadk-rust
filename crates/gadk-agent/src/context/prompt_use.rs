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

//! The prompt version an agent's instruction was rendered from.

/// The prompt version that produced an agent's instruction. The instruction provider records it
/// in the invocation's context ([`ReadonlyContext::record_prompt`](crate::ReadonlyContext::record_prompt));
/// plugins read it to attribute model calls to prompt versions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PromptUse {
    /// The prompt name.
    pub name: String,
    /// The version number.
    pub version: u32,
    /// The label the version was selected by (`latest` for the newest version), or `None` for a
    /// version selected by number.
    pub label: Option<String>,
}

impl PromptUse {
    /// Version `version` of prompt `name`, without a label.
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
            label: None,
        }
    }

    /// Sets the label the version was selected by.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::PromptUse;

    #[test]
    fn new_has_no_label_and_with_label_sets_it() {
        let used = PromptUse::new("triage", 3);
        assert_eq!(
            (used.name.as_str(), used.version, used.label.as_deref()),
            ("triage", 3, None)
        );
        let labelled = used.clone().with_label("production");
        assert_eq!(labelled.label.as_deref(), Some("production"));
        assert_eq!(labelled.version, 3);
        assert_ne!(used, labelled);
    }
}
