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

//! The build context a deploy ships: the two-stage Dockerfile's text (R7), the staged source
//! tree (R8 copy, R10 archive) and the temp-dir helper (Go `os.MkdirTemp` with the
//! `<prefix>20060102_150405__*` pattern).
//!
//! Task 2 (contracts) declares the API; the bodies marked `todo!()` are Task 3's.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// The runtime image (glibc, Debian 12 like the `rust:*-bookworm` builder).
pub const RUNTIME_IMAGE: &str = "gcr.io/distroless/cc-debian12";
/// The builder's `cargo build --target-dir`.
pub const TARGET_DIR: &str = "/gadk-target";

/// The builder stage of the generated Dockerfile (R7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuilderStage {
    /// `rust:<ver>-bookworm` or `rust:bookworm` ([`crate::target::builder_image`]).
    pub image: String,
    /// The workspace root relative to the staged root (`""` for Cloud Run): `WORKDIR /app/<workdir>`.
    pub workdir: String,
    /// `cargo build -p <package>`.
    pub package: String,
    /// `cargo build --bin <bin>`; the runtime stage's `/app/<bin>`.
    pub bin: String,
}

/// R7: the whole Dockerfile, starting with Go's leading newline: the builder stage, the
/// distroless runtime stage, `EXPOSE <server_port>`, Go's comment and
/// `CMD ["/app/<bin>", "web", "-host", "0.0.0.0", "-port", "<server_port>"` + `cmd_tail` + `]`
/// (`cmd_tail` is Go's segment list, each starting `, `). Task 3.
pub fn dockerfile(builder: &BuilderStage, server_port: i64, cmd_tail: &str) -> String {
    todo!("Task 3")
}

/// What a [`SourceTree`] entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// One staged path, relative to the tree's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// The files of a source root in sorted path order, without `.git`, `target` (at any depth)
/// and the excluded paths (R8, R10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTree {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}

impl SourceTree {
    /// Walks `root`, skipping `.git` and `target` at any depth and every absolute path in
    /// `exclude` (the temp dir when nested; the root `Dockerfile` for an archive). Task 3.
    pub fn walk(root: &Path, exclude: &[PathBuf]) -> io::Result<SourceTree> {
        todo!("Task 3")
    }

    /// Copies the entries under `dir` (R8). Task 3.
    pub fn copy_into(&self, dir: &Path) -> io::Result<()> {
        todo!("Task 3")
    }

    /// Writes a gzip tar of the entries in order at `path`, then each `extra` `(name, bytes)`
    /// file at the archive root (R10: the generated `Dockerfile` last). Task 3.
    pub fn write_tar_gz(&self, path: &Path, extra: &[(&str, &[u8])]) -> io::Result<()> {
        todo!("Task 3")
    }
}

/// Go `os.MkdirTemp(base, prefix + now.Format("20060102_150405__") + "*")` with `now` in local
/// time (`chrono::Local`, as Go's `time.Now()`); returns the created directory. Task 3.
pub fn make_temp(base: &Path, prefix: &str, now: DateTime<Utc>) -> io::Result<PathBuf> {
    todo!("Task 3")
}
