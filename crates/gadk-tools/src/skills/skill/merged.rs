// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of ADK-Go `tool/skilltoolset/skill/merged_source.go`: several [`Source`]s as one.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use super::{Frontmatter, ResourceReader, SkillError, SkillErrorKind, Source, quote};

/// A [`Source`] combining several sources, queried in the order given (Go
/// `NewMergedSource`).
///
/// Listing concatenates every source's frontmatters and fails with
/// [`SkillErrorKind::DuplicateSkill`] when a name appears twice. A lookup returns the first
/// source's success; a [`SkillErrorKind::SkillNotFound`] error moves on to the next source,
/// any other error is returned at once, and a skill no source has is a bare
/// `skill not found`.
pub struct MergedSource {
    sources: Vec<Arc<dyn Source>>,
}

impl MergedSource {
    /// A source over `sources`, in order.
    pub fn new(sources: Vec<Arc<dyn Source>>) -> Self {
        Self { sources }
    }
}

/// Go's per-lookup loop: the first success wins, `SkillNotFound` falls through.
macro_rules! first_found {
    ($self:ident, |$source:ident| $call:expr) => {{
        for $source in &$self.sources {
            match $call.await {
                Ok(value) => return Ok(value),
                Err(e) if e.kind() == SkillErrorKind::SkillNotFound => continue,
                Err(e) => return Err(e),
            }
        }
        Err(SkillError::sentinel(SkillErrorKind::SkillNotFound))
    }};
}

#[async_trait]
impl Source for MergedSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        let mut all = Vec::new();
        let mut names = HashSet::new();
        for source in &self.sources {
            let frontmatters = source.list_frontmatters().await?;
            for fm in &frontmatters {
                if !names.insert(fm.name.clone()) {
                    return Err(SkillError::sentinel_with(
                        SkillErrorKind::DuplicateSkill,
                        quote(&fm.name),
                    ));
                }
            }
            all.extend(frontmatters);
        }
        Ok(all)
    }

    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        first_found!(self, |source| source.list_resources(name, subpath))
    }

    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        first_found!(self, |source| source.load_frontmatter(name))
    }

    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        first_found!(self, |source| source.load_instructions(name))
    }

    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        first_found!(self, |source| source.load_resource(name, resource_path))
    }
}
