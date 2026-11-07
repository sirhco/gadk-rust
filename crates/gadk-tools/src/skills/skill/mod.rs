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

//! Port of ADK-Go `tool/skilltoolset/skill`: the agent-skill frontmatter, the [`Source`]
//! abstraction over skill storage, the [`SkillFs`] file-system abstraction with [`DirFs`] and
//! [`MemFs`], and the [`FileSystemSource`], [`MergedSource`] and preload wrappers built on them.

use std::error::Error as StdError;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;

mod complete_preload;
mod filesystem;
mod frontmatter;
mod frontmatter_preload;
mod fs;
mod merged;

pub use complete_preload::with_complete_preload;
pub use filesystem::FileSystemSource;
pub use frontmatter::{Frontmatter, build, parse, parse_bytes, validate};
pub use frontmatter_preload::with_frontmatter_preload;
pub use fs::{DirFs, MemFs, SkillDirEntry, SkillFs, SkillMetadata};
pub use merged::MergedSource;

/// The most bytes a skill resource may hold (Go `maxResourceSize`, 10 MiB). A resource of
/// exactly this size is accepted; one byte more is rejected.
pub const MAX_RESOURCE_BYTES: usize = 10 * 1024 * 1024;

/// A stream over one skill resource (Go's `io.ReadCloser`; dropping it closes it).
pub type ResourceReader = Pin<Box<dyn tokio::io::AsyncRead + Send>>;

/// Re-reads a preloading source's backing source (the `reload` callback Go's
/// `WithFrontmatterPreloadSource` / `WithCompletePreloadSource` return). A failed reload keeps
/// the previous cache.
pub type ReloadFn = Arc<dyn Fn() -> BoxFuture<'static, Result<(), SkillError>> + Send + Sync>;

/// What a [`SkillError`] is, for the checks Go makes with `errors.Is` against its sentinel
/// errors (`skill/source.go:24-31`). Every wrapping keeps the innermost sentinel's kind.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillErrorKind {
    /// Go `ErrInvalidSkillName` ("invalid skill name").
    InvalidSkillName,
    /// Go `ErrInvalidFrontmatter` ("invalid frontmatter").
    InvalidFrontmatter,
    /// Go `ErrSkillNotFound` ("skill not found").
    SkillNotFound,
    /// Go `ErrDuplicateSkill` ("duplicate skill").
    DuplicateSkill,
    /// Go `ErrInvalidResourcePath` ("invalid resource path").
    InvalidResourcePath,
    /// Go `ErrResourceNotFound` ("resource not found").
    ResourceNotFound,
    /// Any error that wraps none of Go's sentinels.
    Other,
}

impl SkillErrorKind {
    /// Go's sentinel error text, or `None` for [`SkillErrorKind::Other`].
    fn sentinel_text(self) -> Option<&'static str> {
        match self {
            Self::InvalidSkillName => Some("invalid skill name"),
            Self::InvalidFrontmatter => Some("invalid frontmatter"),
            Self::SkillNotFound => Some("skill not found"),
            Self::DuplicateSkill => Some("duplicate skill"),
            Self::InvalidResourcePath => Some("invalid resource path"),
            Self::ResourceNotFound => Some("resource not found"),
            Self::Other => None,
        }
    }
}

/// An error from a skill [`Source`], the frontmatter functions or a [`SkillFs`] wrapper.
///
/// `Display` is Go's full wrapped error text (for example
/// `invalid frontmatter: parse frontmatter: reading first line: EOF`). [`SkillError::kind`]
/// answers what Go's `errors.Is(err, skill.ErrX)` answers, and `source()` is the wrapped cause.
#[derive(Debug)]
pub struct SkillError {
    kind: SkillErrorKind,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl SkillError {
    /// An error of `kind` whose `Display` is exactly `msg`.
    pub fn new(kind: SkillErrorKind, msg: impl Into<String>) -> Self {
        Self {
            kind,
            message: msg.into(),
            source: None,
        }
    }

    /// The sentinel kind this error carries (Go `errors.Is`).
    pub fn kind(&self) -> SkillErrorKind {
        self.kind
    }

    /// Go's bare sentinel (`return nil, ErrSkillNotFound`): the message is the sentinel text.
    pub(crate) fn sentinel(kind: SkillErrorKind) -> Self {
        Self::new(kind, kind.sentinel_text().unwrap_or("error"))
    }

    /// Go `fmt.Errorf("%w: <detail>", ErrX)`: `"<sentinel text>: <detail>"`.
    pub(crate) fn sentinel_with(kind: SkillErrorKind, detail: impl fmt::Display) -> Self {
        Self::new(
            kind,
            format!("{}: {detail}", kind.sentinel_text().unwrap_or("error")),
        )
    }

    /// An error of `kind` whose `Display` is `msg` and whose `source()` is `source`. `msg`
    /// already contains `source`'s text, as Go's `%w` formatting does.
    pub(crate) fn with_source(
        kind: SkillErrorKind,
        msg: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: msg.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Go `fmt.Errorf("<prefix>: %w", err)` for an I/O cause: kind `Other`.
    pub(crate) fn io(prefix: impl fmt::Display, err: std::io::Error) -> Self {
        Self::with_source(SkillErrorKind::Other, format!("{prefix}: {err}"), err)
    }

    /// Go `fmt.Errorf("<prefix>: %w", err)` for a skill error: the kind is kept.
    pub(crate) fn wrap(prefix: impl fmt::Display, inner: SkillError) -> Self {
        Self::with_source(inner.kind, format!("{prefix}: {inner}"), inner)
    }
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for SkillError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn StdError + 'static))
    }
}

/// Access to skill components (Go `skill.Source`). Implementations must be safe for
/// concurrent use and return [`SkillError`]s of the matching [`SkillErrorKind`].
#[async_trait]
pub trait Source: Send + Sync {
    /// The frontmatters of every available skill.
    async fn list_frontmatters(&self) -> Result<Vec<Frontmatter>, SkillError>;

    /// The resource paths of skill `name` under `subpath`. Both `subpath` and the returned
    /// paths are relative to the skill directory.
    async fn list_resources(&self, name: &str, subpath: &str) -> Result<Vec<String>, SkillError>;

    /// The frontmatter of the skill `name`.
    async fn load_frontmatter(&self, name: &str) -> Result<Frontmatter, SkillError>;

    /// The Markdown instruction body of the skill `name`.
    async fn load_instructions(&self, name: &str) -> Result<String, SkillError>;

    /// A stream over one resource of the skill `name`; `resource_path` is relative to the
    /// skill directory.
    async fn load_resource(
        &self,
        name: &str,
        resource_path: &str,
    ) -> Result<ResourceReader, SkillError>;
}

/// Go `fmt` `%q` on a string (`strconv.Quote`).
fn quote(s: &str) -> String {
    gadk_agent::go_quote(s)
}
