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

//! Port of ADK-Go `tool/skilltoolset/skill/frontmatter_preload.go`: a [`Source`] proxy that
//! keeps every frontmatter in memory.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;

use super::{Frontmatter, ReloadFn, ResourceReader, SkillError, SkillErrorKind, Source, quote};

/// The cached frontmatters: by name for lookups, sorted by name for listing.
#[derive(Default)]
struct Cache {
    by_name: HashMap<String, Frontmatter>,
    sorted: Vec<Frontmatter>,
}

struct FrontmatterPreloadSource {
    base: Arc<dyn Source>,
    cache: RwLock<Cache>,
}

impl FrontmatterPreloadSource {
    /// Port of Go `reload` (`frontmatter_preload.go:62`). The cache is swapped only on
    /// success.
    async fn reload(&self) -> Result<(), SkillError> {
        let mut sorted = self
            .base
            .list_frontmatters()
            .await
            .map_err(|e| SkillError::wrap("preload", e))?;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let mut by_name = HashMap::with_capacity(sorted.len());
        for fm in &sorted {
            if by_name.insert(fm.name.clone(), fm.clone()).is_some() {
                return Err(SkillError::sentinel_with(
                    SkillErrorKind::DuplicateSkill,
                    quote(&fm.name),
                ));
            }
        }
        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = Cache { by_name, sorted };
        Ok(())
    }
}

/// Port of Go `WithFrontmatterPreloadSource` (`frontmatter_preload.go:37`): wraps `source`,
/// loads every frontmatter now, and serves [`Source::list_frontmatters`] (sorted by name) and
/// [`Source::load_frontmatter`] from memory. The other methods go to `source`. The returned
/// [`ReloadFn`] reloads the frontmatters; a failed reload keeps the previous cache.
pub async fn with_frontmatter_preload(
    source: Arc<dyn Source>,
) -> Result<(Arc<dyn Source>, ReloadFn), SkillError> {
    let preload = Arc::new(FrontmatterPreloadSource {
        base: source,
        cache: RwLock::default(),
    });
    preload.reload().await?;
    let for_reload = Arc::clone(&preload);
    let reload: ReloadFn = Arc::new(move || {
        let preload = Arc::clone(&for_reload);
        Box::pin(async move { preload.reload().await })
    });
    Ok((preload, reload))
}

#[async_trait]
impl Source for FrontmatterPreloadSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        Ok(self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .sorted
            .clone())
    }

    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        self.base.list_resources(name, subpath).await
    }

    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_name
            .get(name)
            .cloned()
            .ok_or_else(|| SkillError::sentinel(SkillErrorKind::SkillNotFound))
    }

    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        self.base.load_instructions(name).await
    }

    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        self.base.load_resource(name, resource_path).await
    }
}
