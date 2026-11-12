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

//! Test fixtures shared by every gadk-cli test: [`TestEnv`] (an [`Env`] with captured
//! stdout/stderr, a map environment, a fixed clock and a [`RecordingRunner`]) and the
//! recording runner itself (canned exits and output, every [`Cmd`] recorded with its working
//! directory), plus the fixture Cargo workspace on disk and the canned `cargo metadata` answer
//! both deploy commands' tests resolve their target through. Nothing here touches the process
//! environment or the current directory.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use gadk_platform::env::MapEnv;
use serde_json::{Value, json};

use crate::runner::{Cmd, CmdOutput, CommandRunner, Exit};
use crate::{Env, SharedWriter};

/// A captured output stream: everything written through the [`SharedWriter`] it backs.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// The bytes written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The text written so far (lossy UTF-8).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    fn writer(&self) -> SharedWriter {
        SharedWriter::from_shared(self.0.clone())
    }
}

/// A canned answer to one command: the bytes the child "writes" and how it "ends"
/// (`Err` is a start failure, Go's error from `Run` before the child exists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canned {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub result: Result<Exit, String>,
}

impl Canned {
    /// Exit 0 with `stdout`.
    pub fn success(stdout: impl Into<Vec<u8>>) -> Canned {
        Canned {
            stdout: stdout.into(),
            stderr: Vec::new(),
            result: Ok(Exit::Code(0)),
        }
    }

    /// Exit `code` with no output.
    pub fn exit(code: i32) -> Canned {
        Canned {
            stdout: Vec::new(),
            stderr: Vec::new(),
            result: Ok(Exit::Code(code)),
        }
    }

    /// The child never starts; `run`/`output` return `Err(msg)`.
    pub fn start_error(msg: &str) -> Canned {
        Canned {
            stdout: Vec::new(),
            stderr: Vec::new(),
            result: Err(msg.to_string()),
        }
    }
}

type Responder = Box<dyn Fn(&Cmd) -> Canned + Send + Sync>;

/// A [`CommandRunner`] that starts nothing: it records every [`Cmd`] (in call order, with its
/// `dir`) and answers from its responder, which sees the command first (and may inspect the
/// file system at that moment, e.g. the temp dir `gcloud run deploy` would upload). It answers
/// regardless of `Cmd::err`, so a test need not put a fake program on `PATH`.
pub struct RecordingRunner {
    calls: Mutex<Vec<Cmd>>,
    responder: Responder,
}

impl RecordingRunner {
    /// Every command succeeds with no output.
    pub fn new() -> RecordingRunner {
        RecordingRunner::with_responder(|_| Canned::success(Vec::new()))
    }

    /// Commands are answered by `f`.
    pub fn with_responder(f: impl Fn(&Cmd) -> Canned + Send + Sync + 'static) -> RecordingRunner {
        RecordingRunner {
            calls: Mutex::new(Vec::new()),
            responder: Box::new(f),
        }
    }

    /// The commands run so far, in order.
    pub fn calls(&self) -> Vec<Cmd> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn answer(&self, cmd: &Cmd) -> Canned {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(cmd.clone());
        (self.responder)(cmd)
    }
}

impl Default for RecordingRunner {
    fn default() -> Self {
        RecordingRunner::new()
    }
}

#[async_trait]
impl CommandRunner for RecordingRunner {
    async fn run(
        &self,
        cmd: &Cmd,
        stdout: &mut (dyn Write + Send),
        stderr: &mut (dyn Write + Send),
    ) -> Result<Exit, String> {
        let canned = self.answer(cmd);
        if canned.result.is_ok() {
            if !canned.stdout.is_empty() {
                stdout
                    .write_all(&canned.stdout)
                    .map_err(|e| e.to_string())?;
            }
            if !canned.stderr.is_empty() {
                stderr
                    .write_all(&canned.stderr)
                    .map_err(|e| e.to_string())?;
            }
        }
        canned.result
    }

    async fn output(&self, cmd: &Cmd) -> Result<CmdOutput, String> {
        let canned = self.answer(cmd);
        canned.result.map(|exit| CmdOutput {
            exit,
            stdout: canned.stdout,
            stderr: canned.stderr,
        })
    }
}

/// The fixed clock's default instant: 2026-01-02T03:04:05Z.
pub fn default_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
        .single()
        .unwrap_or_default()
}

/// An [`Env`] for tests with its captured streams and recording runner.
pub struct TestEnv {
    pub env: Env,
    /// What the commands wrote to `Env::out` (Go `os.Stdout`).
    pub out: Captured,
    /// What the commands wrote to `Env::err` (Go `os.Stderr`).
    pub err: Captured,
    /// The runner behind `Env::runner`.
    pub runner: Arc<RecordingRunner>,
}

impl TestEnv {
    /// A builder: working directory `cwd`, no environment variables, the clock fixed at
    /// [`default_now`], a [`RecordingRunner::new`].
    pub fn builder(cwd: impl AsRef<Path>) -> TestEnvBuilder {
        TestEnvBuilder {
            cwd: cwd.as_ref().to_path_buf(),
            vars: HashMap::new(),
            now: default_now(),
            runner: RecordingRunner::new(),
        }
    }
}

/// Builds a [`TestEnv`].
pub struct TestEnvBuilder {
    cwd: PathBuf,
    vars: HashMap<String, String>,
    now: DateTime<Utc>,
    runner: RecordingRunner,
}

impl TestEnvBuilder {
    /// Sets one environment variable.
    pub fn var(mut self, key: &str, value: &str) -> TestEnvBuilder {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }

    /// Sets several environment variables.
    pub fn vars(mut self, vars: HashMap<String, String>) -> TestEnvBuilder {
        self.vars.extend(vars);
        self
    }

    /// Fixes the clock at `now`.
    pub fn now(mut self, now: DateTime<Utc>) -> TestEnvBuilder {
        self.now = now;
        self
    }

    /// Uses `runner`.
    pub fn runner(mut self, runner: RecordingRunner) -> TestEnvBuilder {
        self.runner = runner;
        self
    }

    pub fn build(self) -> TestEnv {
        let out = Captured::default();
        let err = Captured::default();
        let runner = Arc::new(self.runner);
        let now = self.now;
        let env = Env::new(
            runner.clone(),
            out.writer(),
            err.writer(),
            self.cwd,
            Arc::new(MapEnv(self.vars)),
            Arc::new(move || now),
        );
        TestEnv {
            env,
            out,
            err,
            runner,
        }
    }
}

/// Writes a one-package Cargo workspace at `root`: `Cargo.lock` and, for an empty `member`, a
/// root `Cargo.toml` that is both the package `package` and the workspace; otherwise a root
/// `Cargo.toml` listing `member` and `<member>/Cargo.toml` for `package`; the package's
/// `src/main.rs` either way.
pub fn write_cargo_workspace(root: &Path, member: &str, package: &str) {
    let manifest = format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n");
    let package_dir = if member.is_empty() {
        root.to_path_buf()
    } else {
        root.join(member)
    };
    fs::create_dir_all(package_dir.join("src")).unwrap();
    if member.is_empty() {
        fs::write(
            root.join("Cargo.toml"),
            format!("{manifest}\n[workspace]\n"),
        )
        .unwrap();
    } else {
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nmembers = [\"{member}\"]\n"),
        )
        .unwrap();
        fs::write(package_dir.join("Cargo.toml"), manifest).unwrap();
    }
    fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(package_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
}

/// What `cargo metadata --format-version 1 --no-deps` prints for a workspace at
/// `workspace_root` whose only package `package` (manifest `manifest_path`) has the binary
/// targets `bins`, a library target, and the `path` dependencies `path_dependencies`.
pub fn cargo_metadata_json(
    workspace_root: &Path,
    manifest_path: &Path,
    package: &str,
    bins: &[&str],
    path_dependencies: &[PathBuf],
) -> String {
    let targets: Vec<Value> = bins
        .iter()
        .map(|b| json!({"kind": ["bin"], "crate_types": ["bin"], "name": b}))
        .chain([json!({"kind": ["lib"], "crate_types": ["lib"], "name": package})])
        .collect();
    let dependencies: Vec<Value> = path_dependencies
        .iter()
        .map(|p| json!({"name": "dep", "source": null, "req": "*", "kind": null, "path": p}))
        .collect();
    json!({
        "packages": [{
            "name": package,
            "version": "0.1.0",
            "dependencies": dependencies,
            "targets": targets,
            "manifest_path": manifest_path,
            "rust_version": null
        }],
        "workspace_root": workspace_root,
        "version": 1
    })
    .to_string()
}

/// Whether `cmd` is `cargo metadata …`.
pub fn is_cargo_metadata(cmd: &Cmd) -> bool {
    cmd.args.get(1).map(String::as_str) == Some("metadata")
}

/// A runner answering `cargo metadata` with exit 0 and the text `metadata` returns for the
/// command, and every other command with `other`.
pub fn cargo_metadata_runner(
    metadata: impl Fn(&Cmd) -> String + Send + Sync + 'static,
    other: impl Fn(&Cmd) -> Canned + Send + Sync + 'static,
) -> RecordingRunner {
    RecordingRunner::with_responder(move |c| {
        if is_cargo_metadata(c) {
            Canned::success(metadata(c))
        } else {
            other(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn recording_runner_records_and_answers() {
        let runner = cargo_metadata_runner(|_| "{}".to_string(), |_| Canned::exit(1));
        let t = TestEnv::builder("/w")
            .runner(runner)
            .now(default_now())
            .build();
        let meta = t.env.command("cargo", &["metadata"]).with_dir("/w/a");
        let got = t.env.runner.output(&meta).await.unwrap();
        assert_eq!((got.exit, got.stdout), (Exit::Code(0), b"{}".to_vec()));
        let other = t.env.command("gcloud", &["x"]);
        let (mut o, mut e) = (Vec::new(), Vec::new());
        assert_eq!(
            t.env.runner.run(&other, &mut o, &mut e).await,
            Ok(Exit::Code(1))
        );
        assert_eq!(t.runner.calls(), vec![meta, other]);
        assert_eq!(t.runner.calls()[0].dir, "/w/a");
        assert_eq!(t.env.time.now(), default_now());
        assert_eq!(t.env.cwd, PathBuf::from("/w"));
    }
}
