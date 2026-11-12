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

//! The Cargo target a deploy builds (R4) and the builder image's Rust version (R5): gadk's
//! replacement for Go's entry-point split and `builderGoVersion`.
//!
//! Task 2 (contracts) declares the API; the bodies marked `todo!()` are Task 3's.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::path::{Path, PathBuf};

use crate::Env;

/// The version of the compiler that built gadk-cli (`build.rs`), Go's `runtime.Version()`
/// analogue in the R5 chain; empty when `build.rs` could not read it.
pub const COMPILED_RUSTC_VERSION: &str = env!("GADK_CLI_RUSTC_VERSION");

/// Go `defaultBuilderGoVersion`: the last-resort, rolling builder tag.
pub const DEFAULT_BUILDER_RUST_VERSION: &str = "latest";

/// The resolved Cargo target (R4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoTarget {
    /// The package's name (`cargo build -p`).
    pub package: String,
    /// The binary target's name (`cargo build --bin`, the image's `/app/<bin>`).
    pub bin: String,
    /// `cargo metadata`'s `workspace_root`.
    pub workspace_root: PathBuf,
    /// The directory of the package's `Cargo.toml`.
    pub manifest_dir: PathBuf,
    /// The package's `rust_version` as `cargo metadata` reports it (workspace inheritance
    /// resolved), the R5 chain's second step.
    pub rust_version: Option<String>,
    /// The directory of every `path` dependency of every workspace member.
    pub path_dependencies: Vec<PathBuf>,
}

/// R4: resolves `-e/--entry_point_path` (a package directory or its `Cargo.toml`, relative to
/// `env.cwd`; empty means `env.cwd`) and `--bin` through
/// `cargo metadata --format-version 1 --no-deps --manifest-path <dir>/Cargo.toml` on
/// `env.runner`. Errors (gadk's texts): missing package, zero binaries, unknown `--bin`,
/// several binaries without `--bin`, no `Cargo.lock` at the workspace root. Task 3.
pub async fn resolve(env: &Env, entry_point: &str, bin: &str) -> Result<CargoTarget, String> {
    todo!("Task 3")
}

/// R4: rejects a `path` dependency of a workspace member outside `staged_root` (it would not
/// be staged). Task 3.
pub fn check_path_dependencies(target: &CargoTarget, staged_root: &Path) -> Result<(), String> {
    todo!("Task 3")
}

/// R5: `<workspace_root>/rust-toolchain.toml` `[toolchain] channel` → `target.rust_version` →
/// `compiled` (callers pass [`COMPILED_RUSTC_VERSION`]) → [`DEFAULT_BUILDER_RUST_VERSION`];
/// each candidate is used only when [`is_rust_version`]. Task 3.
pub fn builder_rust_version(target: &CargoTarget, compiled: &str) -> String {
    todo!("Task 3")
}

/// R5: `^[0-9]+(\.[0-9]+){0,2}$`. Task 3.
pub fn is_rust_version(s: &str) -> bool {
    todo!("Task 3")
}

/// R5: `rust:<version>-bookworm`, or `rust:bookworm` for `latest`. Task 3.
pub fn builder_image(version: &str) -> String {
    todo!("Task 3")
}
