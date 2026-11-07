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

//! Port of ADK-Go `tool/skilltoolset/skill/filesystem_source.go`: a [`Source`] over a
//! [`SkillFs`].

use std::io::ErrorKind;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, BufReader};

use super::{
    Frontmatter, ResourceReader, SkillError, SkillErrorKind, SkillFs, Source, parse, quote,
};
use crate::go_fmt::{path_clean, valid_path};

/// The directories a skill resource may live in, in Go's listing order.
const RESOURCE_DIRS: [&str; 3] = ["references", "assets", "scripts"];

/// A [`Source`] backed by a [`SkillFs`] (Go `NewFileSystemSource`).
///
/// Skills are the immediate subdirectories of the root that contain a `SKILL.md` whose
/// frontmatter is valid and whose `name` equals the directory name:
///
/// ```text
/// skill-1/
///     SKILL.md
///     assets/
/// skill-2/
///     SKILL.md
///     references/
///     scripts/
/// ```
///
/// Nothing is cached: every call reads the file system again.
pub struct FileSystemSource {
    fs: Arc<dyn SkillFs>,
}

impl FileSystemSource {
    /// A source reading skills from `fs`.
    pub fn new(fs: Arc<dyn SkillFs>) -> Self {
        Self { fs }
    }

    /// Port of Go `readSkill` (`filesystem_source.go:208`): opens and parses
    /// `<name>/SKILL.md`, and returns the frontmatter with the reader positioned at the body.
    async fn read_skill(
        &self,
        name: &str,
    ) -> Result<(Frontmatter, BufReader<ResourceReader>), SkillError> {
        let skill_file_path = path_join(name, "SKILL.md");
        let file = match self.fs.open(&skill_file_path).await {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(SkillError::sentinel_with(
                    SkillErrorKind::SkillNotFound,
                    quote(name),
                ));
            }
            Err(e) => {
                return Err(SkillError::io(
                    format!("open {}", quote(&skill_file_path)),
                    e,
                ));
            }
        };
        let mut reader = BufReader::new(file);
        let frontmatter = parse(&mut reader).await.map_err(|e| {
            let msg = format!("invalid frontmatter: parse frontmatter: {e}");
            SkillError::with_source(SkillErrorKind::InvalidFrontmatter, msg, e)
        })?;
        if frontmatter.name != name {
            return Err(SkillError::sentinel_with(
                SkillErrorKind::InvalidSkillName,
                format!(
                    "name in SKILL.md ({}) does not match directory name ({})",
                    quote(&frontmatter.name),
                    quote(name)
                ),
            ));
        }
        Ok((frontmatter, reader))
    }

    /// Go `validateSkill`.
    async fn validate_skill(&self, name: &str) -> Result<(), SkillError> {
        self.read_skill(name).await.map(|_| ())
    }

    /// Go `fs.WalkDir(skillFS, target, ...)` collecting the non-directory paths (relative to
    /// the skill directory) in WalkDir's lexical pre-order. A missing target, or a directory
    /// that disappears during the walk, is skipped (Go's `fs.ErrNotExist` branch).
    async fn walk(
        &self,
        name: &str,
        target: &str,
        out: &mut Vec<String>,
    ) -> Result<(), SkillError> {
        let walk_err =
            |e: std::io::Error| SkillError::io(format!("walk target {}", quote(target)), e);
        let root = match self.fs.metadata(&path_join(name, target)).await {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(walk_err(e)),
        };
        // (path relative to the skill, is_dir); entries are pushed in reverse name order so
        // they pop in name order.
        let mut stack = vec![(target.to_string(), root.is_dir)];
        while let Some((path, is_dir)) = stack.pop() {
            if !is_dir {
                out.push(path);
                continue;
            }
            let entries = match self.fs.read_dir(&path_join(name, &path)).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(walk_err(e)),
            };
            for entry in entries.into_iter().rev() {
                stack.push((format!("{path}/{}", entry.name), entry.is_dir));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Source for FileSystemSource {
    /// Port of Go `ListFrontmatters` (`filesystem_source.go:55`): the frontmatters of the
    /// root's immediate subdirectories that contain a `SKILL.md`, in name order. Directories
    /// without one are skipped; any other error (an invalid frontmatter, a name mismatch)
    /// aborts the listing, as Go's code does (Go-parity note 3).
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError> {
        let entries = self
            .fs
            .read_dir(".")
            .await
            .map_err(|e| SkillError::io("read root directory", e))?;
        let mut frontmatters = Vec::new();
        for entry in entries.into_iter().filter(|e| e.is_dir) {
            match self.read_skill(&entry.name).await {
                Ok((frontmatter, _)) => frontmatters.push(frontmatter),
                Err(e) if e.kind() == SkillErrorKind::SkillNotFound => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(frontmatters)
    }

    /// Port of Go `ListResources` (`filesystem_source.go:141`).
    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError> {
        self.validate_skill(name).await?;
        let clean_path = path_clean(subpath);
        let is_root = clean_path == "." || clean_path.is_empty();
        if !is_root {
            let first = clean_path.split('/').next().unwrap_or_default();
            if !RESOURCE_DIRS.contains(&first) {
                return Err(SkillError::sentinel_with(
                    SkillErrorKind::InvalidResourcePath,
                    format!(
                        "{} must be empty, root (.), or within 'references/', 'assets/', or 'scripts/'",
                        quote(subpath)
                    ),
                ));
            }
        }
        // Go `fs.Sub(f.filesystem, name)`.
        if !valid_path(name) {
            return Err(SkillError::new(
                SkillErrorKind::Other,
                format!(
                    "create sub-filesystem for {}: sub {name}: invalid argument",
                    quote(name)
                ),
            ));
        }
        // Go `fs.Stat(skillFS, cleanPath)`.
        if let Err(e) = self.fs.metadata(&path_join(name, &clean_path)).await {
            if e.kind() == ErrorKind::NotFound {
                return Err(SkillError::sentinel_with(
                    SkillErrorKind::ResourceNotFound,
                    quote(&clean_path),
                ));
            }
            return Err(SkillError::io(format!("stat {}", quote(&clean_path)), e));
        }
        let targets: Vec<&str> = if is_root {
            RESOURCE_DIRS.to_vec()
        } else {
            vec![clean_path.as_str()]
        };
        let mut resources = Vec::new();
        for target in targets {
            self.walk(name, target, &mut resources).await?;
        }
        Ok(resources)
    }

    /// Port of Go `LoadFrontmatter` (`filesystem_source.go:83`).
    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError> {
        self.read_skill(name)
            .await
            .map(|(frontmatter, _)| frontmatter)
    }

    /// Port of Go `LoadInstructions` (`filesystem_source.go:94`): the body after the
    /// frontmatter. A body that is not UTF-8 is decoded lossily.
    async fn load_instructions(&self, name: &str) -> Result<String, SkillError> {
        let (_, mut reader) = self.read_skill(name).await?;
        let mut instructions = Vec::new();
        reader
            .read_to_end(&mut instructions)
            .await
            .map_err(|e| SkillError::io("read instructions", e))?;
        Ok(String::from_utf8_lossy(&instructions).into_owned())
    }

    /// Port of Go `LoadResource` (`filesystem_source.go:115`): the path is cleaned with Go's
    /// `path.Clean` and must then lie under `references/`, `assets/` or `scripts/`.
    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError> {
        self.validate_skill(name).await?;
        let clean_path = path_clean(resource_path);
        if !RESOURCE_DIRS.iter().any(|dir| {
            clean_path
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return Err(SkillError::sentinel_with(
                SkillErrorKind::InvalidResourcePath,
                format!(
                    "{} must be within 'references/', 'assets/', or 'scripts/' (relative to skill directory)",
                    quote(resource_path)
                ),
            ));
        }
        let full_path = path_join(name, &clean_path);
        match self.fs.open(&full_path).await {
            Ok(file) => Ok(file),
            Err(e) if e.kind() == ErrorKind::NotFound => Err(SkillError::sentinel_with(
                SkillErrorKind::ResourceNotFound,
                quote(&clean_path),
            )),
            Err(e) => Err(SkillError::io(
                format!("open resource file {}", quote(&full_path)),
                e,
            )),
        }
    }
}

/// Go `path.Join` for two elements.
fn path_join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (true, false) => path_clean(b),
        (false, true) => path_clean(a),
        (false, false) => path_clean(&format!("{a}/{b}")),
    }
}
