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

//! Port of ADK-Go `tool/skilltoolset/skill/complete_preload.go`: a [`Source`] proxy that
//! keeps every skill's frontmatter, instructions and resources in memory.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use super::{
    Frontmatter, MAX_RESOURCE_BYTES, ReloadFn, ResourceReader, SkillError, SkillErrorKind, Source,
    quote,
};
use crate::go_fmt::path_clean;

/// One preloaded skill (Go `completePreloadSkillData`).
struct SkillData {
    frontmatter: Frontmatter,
    instructions: String,
    /// Resource bytes by cleaned path.
    resources: HashMap<String, Arc<[u8]>>,
    /// The cleaned resource paths, sorted, for prefix listing.
    sorted_resource_paths: Vec<String>,
}

#[derive(Default)]
struct Cache {
    skills: HashMap<String, Arc<SkillData>>,
    sorted_frontmatters: Vec<Frontmatter>,
}

struct CompletePreloadSource {
    base: Arc<dyn Source>,
    cache: RwLock<Cache>,
}

impl CompletePreloadSource {
    fn skill(&self, name: &str) -> Result<Arc<SkillData>, SkillError> {
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .skills
            .get(name)
            .cloned()
            .ok_or_else(|| SkillError::sentinel(SkillErrorKind::SkillNotFound))
    }

    /// Port of Go `reload` (`complete_preload.go:138`). The cache is swapped only on success.
    async fn reload(&self) -> Result<(), SkillError> {
        let mut frontmatters = self
            .base
            .list_frontmatters()
            .await
            .map_err(|e| SkillError::wrap("list frontmatters", e))?;
        frontmatters.sort_by(|a, b| a.name.cmp(&b.name));

        let mut skills = HashMap::with_capacity(frontmatters.len());
        for fm in &frontmatters {
            let name = fm.name.as_str();
            if skills.contains_key(name) {
                return Err(SkillError::sentinel_with(
                    SkillErrorKind::DuplicateSkill,
                    quote(name),
                ));
            }
            let instructions = self.base.load_instructions(name).await.map_err(|e| {
                SkillError::wrap(format!("load instructions for skill {}", quote(name)), e)
            })?;
            let resource_paths = self.base.list_resources(name, ".").await.map_err(|e| {
                SkillError::wrap(format!("list resources for skill {}", quote(name)), e)
            })?;

            let mut resources = HashMap::new();
            let mut sorted_resource_paths = Vec::with_capacity(resource_paths.len());
            for resource_path in &resource_paths {
                let reader = self
                    .base
                    .load_resource(name, resource_path)
                    .await
                    .map_err(|e| {
                        SkillError::wrap(
                            format!(
                                "resource path {} in skill {}",
                                quote(resource_path),
                                quote(name)
                            ),
                            e,
                        )
                    })?;
                let mut data = Vec::new();
                // Go `io.ReadAll(io.LimitReader(resource, maxResourceSize+1))`.
                reader
                    .take(MAX_RESOURCE_BYTES as u64 + 1)
                    .read_to_end(&mut data)
                    .await
                    .map_err(|e| {
                        SkillError::io(
                            format!(
                                "read resource path {} in skill {}",
                                quote(resource_path),
                                quote(name)
                            ),
                            e,
                        )
                    })?;
                if data.len() > MAX_RESOURCE_BYTES {
                    return Err(SkillError::new(
                        SkillErrorKind::Other,
                        format!(
                            "resource {} in skill {} exceeds {} bytes limit",
                            quote(resource_path),
                            quote(name),
                            MAX_RESOURCE_BYTES
                        ),
                    ));
                }
                let clean_path = path_clean(resource_path);
                resources.insert(clean_path.clone(), Arc::from(data));
                sorted_resource_paths.push(clean_path);
            }
            sorted_resource_paths.sort();

            skills.insert(
                name.to_string(),
                Arc::new(SkillData {
                    frontmatter: fm.clone(),
                    instructions,
                    resources,
                    sorted_resource_paths,
                }),
            );
        }

        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = Cache {
            skills,
            sorted_frontmatters: frontmatters,
        };
        Ok(())
    }
}

/// Port of Go `WithCompletePreloadSource` (`complete_preload.go:56`): wraps `source` and loads
/// every skill's frontmatter, instructions and resources (each at most
/// [`MAX_RESOURCE_BYTES`]) now; every [`Source`] method is then served from memory. `source`
/// must list all of a skill's resources for the subpath `"."`. The returned [`ReloadFn`]
/// reloads everything; a failed reload keeps the previous cache.
///
/// Lookups of an unknown skill fail with a bare `skill not found`, and of an unknown resource
/// with a bare `resource not found` (no path validation at this layer). `list_resources`
/// returns every resource for `"."` or `"/"`, and otherwise the resource named exactly by the
/// cleaned subpath (unless the subpath ends in `/`) plus every resource under it; an unknown
/// subpath gives an empty list, not an error.
pub async fn with_complete_preload(
    source: Arc<dyn Source>,
) -> Result<(Arc<dyn Source>, ReloadFn), SkillError> {
    let preload = Arc::new(CompletePreloadSource {
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
impl Source for CompletePreloadSource {
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        Ok(self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .sorted_frontmatters
            .clone())
    }

    /// Port of Go `ListResources` (`complete_preload.go:105`).
    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        let data = self.skill(name)?;
        let clean_path = path_clean(subpath);
        if clean_path == "." || clean_path == "/" {
            return Ok(data.sorted_resource_paths.clone());
        }
        let mut result = Vec::new();
        if !subpath.ends_with('/') && data.resources.contains_key(&clean_path) {
            result.push(clean_path.clone());
        }
        // `clean_path` never ends in `/`, so this is the directory prefix.
        let prefix = format!("{clean_path}/");
        let start = data
            .sorted_resource_paths
            .partition_point(|p| p.as_str() < prefix.as_str());
        result.extend(
            data.sorted_resource_paths[start..]
                .iter()
                .take_while(|p| p.starts_with(&prefix))
                .cloned(),
        );
        Ok(result)
    }

    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        Ok(self.skill(name)?.frontmatter.clone())
    }

    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        Ok(self.skill(name)?.instructions.clone())
    }

    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        let data = self.skill(name)?;
        let bytes = data
            .resources
            .get(&path_clean(resource_path))
            .cloned()
            .ok_or_else(|| SkillError::sentinel(SkillErrorKind::ResourceNotFound))?;
        Ok(Box::pin(std::io::Cursor::new(bytes)))
    }
}
