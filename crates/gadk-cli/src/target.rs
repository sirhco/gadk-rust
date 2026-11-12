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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use gadk_platform::{go_path_clean, go_quote};
use serde::Deserialize;

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

/// The part of `cargo metadata --format-version 1 --no-deps` that target resolution reads.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetadataPackage>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetadataPackage {
    name: String,
    manifest_path: PathBuf,
    #[serde(default)]
    targets: Vec<MetadataTarget>,
    #[serde(default)]
    dependencies: Vec<MetadataDependency>,
    #[serde(default)]
    rust_version: Option<String>,
}

#[derive(Deserialize)]
struct MetadataTarget {
    name: String,
    #[serde(default)]
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct MetadataDependency {
    #[serde(default)]
    path: Option<PathBuf>,
}

/// Go `%q` of a path.
fn quote_path(p: &Path) -> String {
    go_quote(&p.to_string_lossy())
}

/// Whether `a` and `b` name the same file: equal as given, else equal once both resolve.
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// R4: resolves `-e/--entry_point_path` (a package directory or its `Cargo.toml`, relative to
/// `env.cwd`; empty means `env.cwd`) and `--bin` through
/// `cargo metadata --format-version 1 --no-deps --manifest-path <dir>/Cargo.toml` on
/// `env.runner`. Errors (gadk's texts): missing package, zero binaries, unknown `--bin`,
/// several binaries without `--bin`, no `Cargo.lock` at the workspace root.
// Divergence (ledger 6b): Go deploys a `main.go` entry point (split into directory and file,
// `.go` stripped); gadk deploys a Cargo binary target found through `cargo metadata`, with
// gadk's own error texts (Go has none for these cases) and a required `Cargo.lock`.
pub async fn resolve(env: &Env, entry_point: &str, bin: &str) -> Result<CargoTarget, String> {
    let path = env.abs(entry_point);
    let manifest = if path.file_name().is_some_and(|n| n == "Cargo.toml") {
        path
    } else {
        path.join("Cargo.toml")
    };
    let manifest_arg = manifest.to_string_lossy().into_owned();
    let cmd = env
        .command(
            "cargo",
            &[
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
                &manifest_arg,
            ],
        )
        .with_dir(env.cwd.to_string_lossy().into_owned());
    let output = env
        .runner
        .output(&cmd)
        .await
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.exit.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let mut msg = format!(
            "cargo metadata failed for {}: {}",
            quote_path(&manifest),
            output.exit.go_error()
        );
        if !stderr.is_empty() {
            msg.push_str(": ");
            msg.push_str(stderr);
        }
        return Err(msg);
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot parse cargo metadata output: {e}"))?;

    let Some(package) = metadata
        .packages
        .iter()
        .find(|p| same_path(&p.manifest_path, &manifest))
    else {
        return Err(format!(
            "no package in {}: -e/--entry_point_path must name a Cargo package directory or its Cargo.toml",
            quote_path(&manifest)
        ));
    };

    let bins: Vec<&str> = package
        .targets
        .iter()
        .filter(|t| t.kind.iter().any(|k| k == "bin"))
        .map(|t| t.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let chosen = match (bins.as_slice(), bin) {
        ([], _) => {
            return Err(format!(
                "package {} has no binary target",
                go_quote(&package.name)
            ));
        }
        ([only], "") => (*only).to_string(),
        (_, "") => {
            return Err(format!(
                "package {} has several binary targets ({}); select one with --bin",
                go_quote(&package.name),
                bins.join(", ")
            ));
        }
        (_, wanted) if bins.contains(&wanted) => wanted.to_string(),
        (_, wanted) => {
            return Err(format!(
                "package {} has no binary target {} (binaries: {})",
                go_quote(&package.name),
                go_quote(wanted),
                bins.join(", ")
            ));
        }
    };

    if !metadata.workspace_root.join("Cargo.lock").is_file() {
        return Err(format!(
            "no Cargo.lock in the workspace root {}: run cargo generate-lockfile",
            quote_path(&metadata.workspace_root)
        ));
    }

    let path_dependencies: Vec<PathBuf> = metadata
        .packages
        .iter()
        .flat_map(|p| p.dependencies.iter().filter_map(|d| d.path.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(CargoTarget {
        package: package.name.clone(),
        bin: chosen,
        workspace_root: metadata.workspace_root.clone(),
        manifest_dir: package
            .manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        rust_version: package.rust_version.clone(),
        path_dependencies,
    })
}

/// Whether `path` is `root` or lies under it, compared lexically (both cleaned), else once
/// both resolve.
fn is_within(path: &Path, root: &Path) -> bool {
    let clean = |p: &Path| PathBuf::from(go_path_clean(&p.to_string_lossy()));
    if clean(path).starts_with(clean(root)) {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(root)) {
        (Ok(p), Ok(r)) => p.starts_with(r),
        _ => false,
    }
}

/// R4: rejects a `path` dependency of a workspace member outside `staged_root` (it would not
/// be staged).
// Divergence (ledger 6b): Go has no such check (its build context is a compiled binary, or
// the whole `--source_dir`); gadk rejects, before the temp dir exists, a build context that
// `cargo build --locked` could not build.
pub fn check_path_dependencies(target: &CargoTarget, staged_root: &Path) -> Result<(), String> {
    match target
        .path_dependencies
        .iter()
        .find(|dep| !is_within(dep, staged_root))
    {
        Some(dep) => Err(format!(
            "path dependency {} is outside {} and would not be staged",
            quote_path(dep),
            quote_path(staged_root)
        )),
        None => Ok(()),
    }
}

/// The `[toolchain] channel` of `rust-toolchain.toml` contents when it is a version number,
/// else `""` — Go `goVersionFromModFile`'s analogue. A minimal line reader (no TOML parser):
/// section headers, `channel = "<v>"` or `'<v>'`, trailing `#` comments.
pub fn channel_from_toolchain_file(data: &str) -> String {
    let mut in_toolchain = false;
    for line in data.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let header = header.split('#').next().unwrap_or_default().trim();
            in_toolchain = header.strip_suffix(']').map(str::trim) == Some("toolchain");
            continue;
        }
        if !in_toolchain {
            continue;
        }
        let Some(rest) = line.strip_prefix("channel") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let inner = &value[1..];
        let Some(end) = inner.find(quote) else {
            continue;
        };
        let after = inner[end + 1..].trim_start();
        if !after.is_empty() && !after.starts_with('#') {
            continue;
        }
        let v = inner[..end].trim();
        if is_rust_version(v) {
            return v.to_string();
        }
    }
    String::new()
}

/// R5: `<workspace_root>/rust-toolchain.toml` `[toolchain] channel` → `target.rust_version` →
/// `compiled` (callers pass [`COMPILED_RUSTC_VERSION`]) → [`DEFAULT_BUILDER_RUST_VERSION`];
/// each candidate is used only when [`is_rust_version`].
// Divergence (ledger 6b): Go's chain is go.mod `go` → `runtime.Version()` → `latest`; gadk's is
// rust-toolchain.toml → `rust-version` (as cargo metadata resolves it) → the rustc that
// compiled gadk → `latest`. The legacy plain-text `rust-toolchain` file is not read (rustup in
// the builder image still honours it).
pub fn builder_rust_version(target: &CargoTarget, compiled: &str) -> String {
    if let Ok(data) = fs::read_to_string(target.workspace_root.join("rust-toolchain.toml")) {
        let v = channel_from_toolchain_file(&data);
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(v) = target.rust_version.as_deref().map(str::trim)
        && is_rust_version(v)
    {
        return v.to_string();
    }
    if is_rust_version(compiled) {
        return compiled.to_string();
    }
    DEFAULT_BUILDER_RUST_VERSION.to_string()
}

/// R5: `^[0-9]+(\.[0-9]+){0,2}$`.
pub fn is_rust_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() <= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// R5: `rust:<version>-bookworm`, or `rust:bookworm` for `latest`.
// Divergence (ledger 6b): Go's builder is `golang:<ver>` (with `ENV GOTOOLCHAIN=auto`); gadk's
// is the Debian 12 `rust:<ver>-bookworm`, matching the `distroless/cc-debian12` runtime.
pub fn builder_image(version: &str) -> String {
    if version == DEFAULT_BUILDER_RUST_VERSION {
        "rust:bookworm".to_string()
    } else {
        format!("rust:{version}-bookworm")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::runner::Exit;
    use crate::testutil::{Canned, RecordingRunner, TestEnv};

    /// A trimmed copy of a real `cargo metadata --format-version 1 --no-deps` run on a
    /// two-member workspace (`agent` with binaries `agent` and `tool`, path dependencies on
    /// the member `lib` and on `outside` beyond the workspace; `rust-version.workspace = true`
    /// with `[workspace.package] rust-version = "1.95"`). `{ROOT}` is the fixture's parent dir.
    const TWO_MEMBER_METADATA: &str = r#"{
  "packages": [
    {
      "name": "agent",
      "version": "0.1.0",
      "id": "path+file://{ROOT}/ws/agent#0.1.0",
      "dependencies": [
        {"name": "lib", "source": null, "req": "*", "kind": null, "path": "{ROOT}/ws/lib"},
        {"name": "out", "source": null, "req": "*", "kind": null, "path": "{ROOT}/outside"}
      ],
      "targets": [
        {"kind": ["bin"], "crate_types": ["bin"], "name": "agent", "src_path": "{ROOT}/ws/agent/src/main.rs"},
        {"kind": ["bin"], "crate_types": ["bin"], "name": "tool", "src_path": "{ROOT}/ws/agent/src/bin/tool.rs"}
      ],
      "manifest_path": "{ROOT}/ws/agent/Cargo.toml",
      "edition": "2024",
      "rust_version": "1.95"
    },
    {
      "name": "lib",
      "version": "0.1.0",
      "id": "path+file://{ROOT}/ws/lib#0.1.0",
      "dependencies": [],
      "targets": [
        {"kind": ["lib"], "crate_types": ["lib"], "name": "lib", "src_path": "{ROOT}/ws/lib/src/lib.rs"}
      ],
      "manifest_path": "{ROOT}/ws/lib/Cargo.toml",
      "edition": "2024",
      "rust_version": null
    }
  ],
  "workspace_members": ["path+file://{ROOT}/ws/agent#0.1.0", "path+file://{ROOT}/ws/lib#0.1.0"],
  "resolve": null,
  "target_directory": "{ROOT}/ws/target",
  "version": 1,
  "workspace_root": "{ROOT}/ws",
  "metadata": null
}"#;

    /// A fixture dir: `ws/` (with `Cargo.lock` unless `lock` is false), `ws/agent`, `ws/lib`.
    fn fixture(lock: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("agent/src")).unwrap();
        fs::create_dir_all(ws.join("lib/src")).unwrap();
        fs::write(ws.join("Cargo.toml"), "[workspace]\n").unwrap();
        if lock {
            fs::write(ws.join("Cargo.lock"), "version = 4\n").unwrap();
        }
        dir
    }

    fn two_member_metadata(root: &Path) -> String {
        TWO_MEMBER_METADATA.replace("{ROOT}", &root.to_string_lossy())
    }

    /// Metadata for one package `name` at `<root>/ws/<dir>` with the given targets
    /// (`(kind, name)`), workspace root `<root>/ws`.
    fn one_package_metadata(
        root: &Path,
        dir: &str,
        name: &str,
        targets: &[(&str, &str)],
    ) -> String {
        let ws = root.join("ws");
        let targets: Vec<Value> = targets
            .iter()
            .map(|(kind, name)| json!({"kind": [kind], "crate_types": [kind], "name": name}))
            .collect();
        json!({
            "packages": [{
                "name": name,
                "dependencies": [],
                "targets": targets,
                "manifest_path": ws.join(dir).join("Cargo.toml"),
                "rust_version": null,
            }],
            "workspace_root": ws,
            "version": 1,
        })
        .to_string()
    }

    /// A [`TestEnv`] in `cwd` whose runner answers every command with `stdout`.
    fn env_answering(cwd: &Path, stdout: String) -> TestEnv {
        TestEnv::builder(cwd)
            .runner(RecordingRunner::with_responder(move |_| {
                Canned::success(stdout.clone())
            }))
            .build()
    }

    fn metadata_args(manifest: &Path) -> Vec<String> {
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([manifest.to_string_lossy().into_owned()])
        .collect()
    }

    #[tokio::test]
    async fn resolve_package_dir_with_one_binary() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(
            &ws,
            one_package_metadata(
                dir.path(),
                "agent",
                "agent",
                &[("lib", "agent"), ("bin", "agent")],
            ),
        );

        let got = resolve(&t.env, "agent", "").await.unwrap();

        assert_eq!(
            got,
            CargoTarget {
                package: "agent".into(),
                bin: "agent".into(),
                workspace_root: ws.clone(),
                manifest_dir: ws.join("agent"),
                rust_version: None,
                path_dependencies: vec![],
            }
        );
        let calls = t.runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, metadata_args(&ws.join("agent/Cargo.toml")));
        assert_eq!(calls[0].dir, ws.to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_accepts_the_cargo_toml_path() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(
            &ws,
            one_package_metadata(dir.path(), "agent", "agent", &[("bin", "agent")]),
        );

        let got = resolve(&t.env, "./agent/Cargo.toml", "").await.unwrap();

        assert_eq!(got.package, "agent");
        assert_eq!(got.manifest_dir, ws.join("agent"));
        assert_eq!(
            t.runner.calls()[0].args,
            metadata_args(&ws.join("agent/Cargo.toml"))
        );
    }

    #[tokio::test]
    async fn resolve_omitted_entry_point_is_the_working_directory() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(
            &ws.join("agent"),
            one_package_metadata(dir.path(), "agent", "agent", &[("bin", "agent")]),
        );

        let got = resolve(&t.env, "", "").await.unwrap();

        assert_eq!(got.bin, "agent");
        assert_eq!(
            t.runner.calls()[0].args,
            metadata_args(&ws.join("agent/Cargo.toml"))
        );
    }

    #[tokio::test]
    async fn resolve_member_crate_with_bin_flag() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        let got = resolve(&t.env, "agent", "tool").await.unwrap();

        assert_eq!(
            got,
            CargoTarget {
                package: "agent".into(),
                bin: "tool".into(),
                workspace_root: ws.clone(),
                manifest_dir: ws.join("agent"),
                rust_version: Some("1.95".into()),
                path_dependencies: vec![dir.path().join("outside"), ws.join("lib")],
            }
        );
    }

    #[tokio::test]
    async fn resolve_several_binaries_without_bin_is_an_error() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        let err = resolve(&t.env, "agent", "").await.unwrap_err();

        assert_eq!(
            err,
            r#"package "agent" has several binary targets (agent, tool); select one with --bin"#
        );
    }

    #[tokio::test]
    async fn resolve_unknown_bin_is_an_error() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        let err = resolve(&t.env, "agent", "nope").await.unwrap_err();

        assert_eq!(
            err,
            r#"package "agent" has no binary target "nope" (binaries: agent, tool)"#
        );
    }

    #[tokio::test]
    async fn resolve_unknown_bin_is_an_error_with_one_binary() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(
            &ws,
            one_package_metadata(dir.path(), "agent", "agent", &[("bin", "agent")]),
        );

        let err = resolve(&t.env, "agent", "other").await.unwrap_err();

        assert_eq!(
            err,
            r#"package "agent" has no binary target "other" (binaries: agent)"#
        );
    }

    #[tokio::test]
    async fn resolve_package_without_binary_is_an_error() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        let err = resolve(&t.env, "lib", "").await.unwrap_err();

        assert_eq!(err, r#"package "lib" has no binary target"#);
    }

    #[tokio::test]
    async fn resolve_missing_package_is_an_error() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        // The virtual workspace manifest is no package.
        let err = resolve(&t.env, ".", "").await.unwrap_err();

        assert_eq!(
            err,
            format!(
                "no package in {}: -e/--entry_point_path must name a Cargo package directory or its Cargo.toml",
                go_quote(&ws.join("Cargo.toml").to_string_lossy())
            )
        );
    }

    #[tokio::test]
    async fn resolve_without_cargo_lock_is_an_error() {
        let dir = fixture(false);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, two_member_metadata(dir.path()));

        let err = resolve(&t.env, "agent", "tool").await.unwrap_err();

        assert_eq!(
            err,
            format!(
                "no Cargo.lock in the workspace root {}: run cargo generate-lockfile",
                go_quote(&ws.to_string_lossy())
            )
        );
    }

    #[tokio::test]
    async fn resolve_reports_a_failing_cargo_metadata() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = TestEnv::builder(&ws)
            .runner(RecordingRunner::with_responder(|_| Canned {
                stdout: Vec::new(),
                stderr: b"error: manifest path `x` does not exist\n".to_vec(),
                result: Ok(Exit::Code(101)),
            }))
            .build();

        let err = resolve(&t.env, "missing", "").await.unwrap_err();

        assert_eq!(
            err,
            format!(
                "cargo metadata failed for {}: exit status 101: error: manifest path `x` does not exist",
                go_quote(&ws.join("missing/Cargo.toml").to_string_lossy())
            )
        );
    }

    #[tokio::test]
    async fn resolve_reports_a_cargo_that_does_not_start() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = TestEnv::builder(&ws)
            .runner(RecordingRunner::with_responder(|_| {
                Canned::start_error(r#"exec: "cargo": executable file not found in $PATH"#)
            }))
            .build();

        let err = resolve(&t.env, "agent", "").await.unwrap_err();

        assert_eq!(
            err,
            r#"cannot run cargo metadata: exec: "cargo": executable file not found in $PATH"#
        );
    }

    #[tokio::test]
    async fn resolve_reports_unparsable_metadata() {
        let dir = fixture(true);
        let ws = dir.path().join("ws");
        let t = env_answering(&ws, "not json".into());

        let err = resolve(&t.env, "agent", "").await.unwrap_err();

        assert!(
            err.starts_with("cannot parse cargo metadata output: "),
            "{err}"
        );
    }

    #[test]
    fn check_path_dependencies_accepts_deps_inside_the_staged_root() {
        let target = CargoTarget {
            path_dependencies: vec![PathBuf::from("/src/ws/lib"), PathBuf::from("/src/vendor/x")],
            ..CargoTarget::default()
        };

        assert_eq!(check_path_dependencies(&target, Path::new("/src")), Ok(()));
        assert_eq!(check_path_dependencies(&target, Path::new("/src/")), Ok(()));
    }

    #[test]
    fn check_path_dependencies_rejects_a_dep_outside_the_staged_root() {
        let target = CargoTarget {
            path_dependencies: vec![PathBuf::from("/src/outside"), PathBuf::from("/src/ws/lib")],
            ..CargoTarget::default()
        };

        assert_eq!(
            check_path_dependencies(&target, Path::new("/src/ws")),
            Err(
                r#"path dependency "/src/outside" is outside "/src/ws" and would not be staged"#
                    .to_string()
            )
        );
        // A sibling whose name merely starts with the root's is outside too.
        let sibling = CargoTarget {
            path_dependencies: vec![PathBuf::from("/src/ws-extra")],
            ..CargoTarget::default()
        };
        assert!(check_path_dependencies(&sibling, Path::new("/src/ws")).is_err());
    }

    // Adapted from Go TestGoVersionFromModFile's cases (agentengine_test.go): the version
    // directive, a trailing comment, absent and malformed values.
    #[test]
    fn channel_from_toolchain_file_cases() {
        let cases = [
            ("pinned", "[toolchain]\nchannel = \"1.99.1\"\n", "1.99.1"),
            ("single quotes", "[toolchain]\nchannel = '1.95'\n", "1.95"),
            ("major only", "[toolchain]\nchannel=\"1\"\n", "1"),
            (
                "trailing comment",
                "[toolchain]\nchannel = \"1.99.1\" # pinned\ncomponents = [\"clippy\"]\n",
                "1.99.1",
            ),
            (
                "after other keys",
                "# top\n[toolchain]\nprofile = \"minimal\"\nchannel = \"1.90.0\"\n",
                "1.90.0",
            ),
            ("named channel", "[toolchain]\nchannel = \"stable\"\n", ""),
            (
                "dated nightly",
                "[toolchain]\nchannel = \"nightly-2026-01-01\"\n",
                "",
            ),
            ("other section", "[other]\nchannel = \"1.99.1\"\n", ""),
            ("no channel", "[toolchain]\nprofile = \"minimal\"\n", ""),
            ("four parts", "[toolchain]\nchannel = \"1.2.3.4\"\n", ""),
            ("unterminated", "[toolchain]\nchannel = \"1.99.1\n", ""),
            ("empty", "", ""),
        ];
        for (name, data, want) in cases {
            assert_eq!(channel_from_toolchain_file(data), want, "{name}");
        }
    }

    #[test]
    fn is_rust_version_cases() {
        for v in ["1", "1.95", "1.99.1", "10.200.3000"] {
            assert!(is_rust_version(v), "{v}");
        }
        for v in [
            "",
            "latest",
            "stable",
            "1.",
            ".1",
            "1..2",
            "1.2.3.4",
            "1.99.1-nightly",
            " 1.95",
            "v1",
        ] {
            assert!(!is_rust_version(v), "{v:?}");
        }
    }

    #[test]
    fn builder_image_tags() {
        assert_eq!(builder_image("1.99.1"), "rust:1.99.1-bookworm");
        assert_eq!(builder_image("1.95"), "rust:1.95-bookworm");
        assert_eq!(builder_image("latest"), "rust:bookworm");
    }

    #[test]
    fn default_builder_rust_version_is_not_pinned() {
        assert!(!is_rust_version(DEFAULT_BUILDER_RUST_VERSION));
        assert_eq!(builder_image(DEFAULT_BUILDER_RUST_VERSION), "rust:bookworm");
    }

    fn target_in(root: &Path, rust_version: Option<&str>) -> CargoTarget {
        CargoTarget {
            workspace_root: root.to_path_buf(),
            rust_version: rust_version.map(str::to_string),
            ..CargoTarget::default()
        }
    }

    #[test]
    fn builder_rust_version_prefers_the_toolchain_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.99.1\"\n",
        )
        .unwrap();

        let got = builder_rust_version(&target_in(dir.path(), Some("1.95")), "1.98.0");

        assert_eq!(got, "1.99.1");
    }

    #[test]
    fn builder_rust_version_falls_back_to_rust_version() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        )
        .unwrap();

        let got = builder_rust_version(&target_in(dir.path(), Some("1.95")), "1.98.0");

        assert_eq!(got, "1.95");
    }

    #[test]
    fn builder_rust_version_falls_back_to_the_compiled_rustc() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            builder_rust_version(&target_in(dir.path(), None), "1.98.0"),
            "1.98.0"
        );
        assert_eq!(
            builder_rust_version(&target_in(dir.path(), Some("1.95-beta")), "1.98.0"),
            "1.98.0"
        );
    }

    #[test]
    fn builder_rust_version_last_resort_is_latest() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            builder_rust_version(&target_in(dir.path(), None), ""),
            "latest"
        );
        assert_eq!(
            builder_rust_version(&target_in(dir.path(), None), "1.100.0-nightly"),
            "latest"
        );
    }
}
