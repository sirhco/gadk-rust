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

//! Binary end-to-end tests of `gadk` (R1, R8): help and error reporting through the real
//! `main`, and a Cloud Run deploy whose `gcloud` is a recording shell script on a test-local
//! `PATH`. Only the spawned child's environment and working directory are set; `cargo
//! metadata` is the real one (the `cargo` that builds these tests), nothing is built.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pretty_assertions::assert_eq;

/// The `gadk` built with these tests.
const GADK: &str = env!("CARGO_BIN_EXE_gadk");

/// Runs `gadk args` in `cwd` with `path` (when given) as its whole `PATH`.
fn gadk(args: &[&str], cwd: &Path, path: Option<&str>) -> Output {
    let mut cmd = Command::new(GADK);
    cmd.args(args).current_dir(cwd);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The built subcommand `gadk deploy [<name>]`.
fn subcommand(name: Option<&str>) -> clap::Command {
    let mut root = gadk_cli::root::command();
    root.build();
    let deploy = root.find_subcommand("deploy").unwrap().clone();
    match name {
        Some(name) => deploy.find_subcommand(name).unwrap().clone(),
        None => deploy,
    }
}

/// R1: a bare `gadk deploy` prints its help (Go `DeployCmd.RunE` → `cmd.Help()`) and exits 0.
#[test]
fn deploy_without_arguments_prints_help() {
    let dir = tempfile::tempdir().unwrap();
    let out = gadk(&["deploy"], dir.path(), None);

    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    assert_eq!(stdout, subcommand(None).render_long_help().to_string());
    assert!(stdout.starts_with("Please see subcommands for details\n"));
    assert!(stdout.contains("  agentengine  Deploys the application to Agent Engine.\n"));
    assert!(stdout.contains("  cloudrun     Deploys the application to cloudrun.\n"));
    assert_eq!(text(&out.stderr), "");
}

/// R1: an unknown flag is cobra's `Error: unknown flag: --nope`, the command's usage and an
/// empty line on stderr, exit 1.
#[test]
fn unknown_flag_prints_error_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let out = gadk(&["deploy", "cloudrun", "--nope"], dir.path(), None);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let usage = subcommand(Some("cloudrun"))
        .help_template("{usage-heading} {usage}\n\n{all-args}")
        .render_help()
        .to_string();
    let stderr = text(&out.stderr);
    assert_eq!(stderr, format!("Error: unknown flag: --nope\n{usage}\n"));
    assert!(stderr.starts_with(
        "Error: unknown flag: --nope\nUsage: gadk deploy cloudrun [OPTIONS]\n\nOptions:\n"
    ));
}

/// A fake `gcloud` that appends its working directory and arguments to `log` (one per line,
/// each call ended by `--`) and keeps a copy of the staged `Dockerfile` of `run deploy` at
/// `seen`; it always exits 0.
fn write_fake_gcloud(bin: &Path, log: &Path, seen: &Path) {
    let script = format!(
        "#!/bin/sh\n\
         {{ pwd; for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo --; }} >> '{log}'\n\
         if [ \"$2\" = deploy ]; then /bin/cp Dockerfile '{seen}'; LC_ALL=C /bin/ls -A > '{seen}.ls'; fi\n\
         exit 0\n",
        log = log.display(),
        seen = seen.display(),
    );
    let gcloud = bin.join("gcloud");
    fs::write(&gcloud, script).unwrap();
    fs::set_permissions(&gcloud, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A one-package Cargo workspace `agent` (binary `agent`) at `root` that `cargo metadata`
/// accepts, with a `.git` and a `target` dir staging must leave out.
fn write_workspace(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"agent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"agent\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.90.0\"\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref\n").unwrap();
    fs::create_dir_all(root.join("target/release")).unwrap();
    fs::write(root.join("target/release/agent"), "bin").unwrap();
}

/// The recorded calls: `(working directory, arguments)` each.
fn calls(log: &Path) -> Vec<(PathBuf, Vec<String>)> {
    let log = fs::read_to_string(log).unwrap();
    let mut calls = Vec::new();
    let mut lines = log.lines();
    while let Some(dir) = lines.next() {
        let args = lines
            .by_ref()
            .take_while(|l| *l != "--")
            .map(str::to_string)
            .collect();
        calls.push((PathBuf::from(dir), args));
    }
    calls
}

/// R8 through the binary: `cargo metadata` resolves the fixture workspace, the staged temp dir
/// holds the source and the generated Dockerfile when `gcloud run deploy` runs there, the temp
/// dir is removed, and `gcloud run services proxy` runs in the caller's directory.
#[test]
fn cloud_run_deploy_runs_fake_gcloud() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let (bin, ws, temp, log, seen) = (
        base.join("bin"),
        base.join("ws"),
        base.join("tmp"),
        base.join("gcloud.log"),
        base.join("Dockerfile.seen"),
    );
    for d in [&bin, &ws, &temp] {
        fs::create_dir_all(d).unwrap();
    }
    write_workspace(&ws);
    write_fake_gcloud(&bin, &log, &seen);
    let cargo_dir = Path::new(env!("CARGO")).parent().unwrap();
    let path = format!("{}:{}", bin.display(), cargo_dir.display());

    let out = gadk(
        &[
            "deploy",
            "cloudrun",
            "-r",
            "us-central1",
            "-p",
            "my-proj",
            "-s",
            "my-svc",
            "-t",
            &temp.to_string_lossy(),
        ],
        &ws,
        Some(&path),
    );

    let stdout = text(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        text(&out.stderr)
    );
    let calls = calls(&log);
    assert_eq!(calls.len(), 2, "{calls:?}");
    let (deploy_dir, deploy_args) = &calls[0];
    assert_eq!(deploy_dir.parent(), Some(temp.as_path()));
    let temp_name = deploy_dir.file_name().unwrap().to_string_lossy();
    assert!(temp_name.starts_with("cloudrun_"), "{temp_name}");
    assert_eq!(
        deploy_args,
        &[
            "run",
            "deploy",
            "my-svc",
            "--source",
            ".",
            "--set-secrets=GOOGLE_API_KEY=GOOGLE_API_KEY:latest",
            "--region",
            "us-central1",
            "--project",
            "my-proj",
            "--ingress",
            "all",
            "--no-allow-unauthenticated",
        ]
    );
    assert_eq!(
        calls[1],
        (
            ws.clone(),
            [
                "run",
                "services",
                "proxy",
                "my-svc",
                "--project",
                "my-proj",
                "--port",
                "8081",
                "--region",
                "us-central1",
            ]
            .map(String::from)
            .to_vec()
        )
    );

    let dockerfile = fs::read_to_string(&seen).unwrap();
    assert!(
        dockerfile.starts_with(
            "\nFROM rust:1.90.0-bookworm AS builder\nWORKDIR /app/\nCOPY . /app/\nRUN cargo build --release --locked --target-dir /gadk-target -p agent --bin agent\n"
        ),
        "{dockerfile}"
    );
    assert!(dockerfile.ends_with(
        r#"CMD ["/app/agent", "web", "-host", "0.0.0.0", "-port", "8080", "api", "-webui_address", "127.0.0.1:8081", "a2a", "--a2a_agent_url", "http://127.0.0.1:8081", "webui", "--api_server_address", "http://127.0.0.1:8081/api"]"#
    ));
    assert_eq!(
        fs::read_to_string(seen.with_extension("seen.ls")).unwrap(),
        ".gcloudignore\nCargo.lock\nCargo.toml\nDockerfile\nrust-toolchain.toml\nsrc\n"
    );
    assert_eq!(fs::read_dir(&temp).unwrap().count(), 0, "temp dir kept");
    for step in [
        "Computing flags & preparing temp",
        "Staging source",
        "Preparing Dockerfile",
        "Deploying to Cloud Run",
        "Cleaning temp",
        "Running local gcloud authenticating proxy",
    ] {
        assert!(
            stdout.contains(&format!("{step} : \x1b[32mFinished successfully\x1b[0m\n")),
            "{step} missing:\n{stdout}"
        );
    }
}
