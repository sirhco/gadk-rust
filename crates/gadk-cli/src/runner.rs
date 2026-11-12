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

//! The process seam (Go `os/exec` as `cmd/adkgo` uses it): [`Cmd`] is Go's `exec.Cmd` with
//! `exec.Command`'s `LookPath` and `Cmd.String()`; a [`CommandRunner`] starts it. Production
//! uses [`TokioRunner`]; tests inject a recording runner, so no test spawns `gcloud` or `cargo`.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use gadk_platform::env::EnvProvider;
use gadk_platform::{go_path_join, go_quote};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Go `exec.ErrNotFound`'s text on Unix.
const ERR_NOT_FOUND: &str = "executable file not found in $PATH";
/// Go `exec.ErrDot`'s text.
const ERR_DOT: &str = "cannot run executable found relative to current directory";

/// How a child process ended (Go `os.ProcessState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this status.
    Code(i32),
    /// A signal (this number) terminated it.
    Signal(i32),
}

impl Exit {
    /// Go `ProcessState.Success()`.
    pub fn success(self) -> bool {
        todo!("Task 2")
    }

    /// Go's `*exec.ExitError` text (`ProcessState.String()`): `exit status 1`,
    /// `signal: killed`.
    pub fn go_error(self) -> String {
        todo!("Task 2")
    }
}

/// Go `syscall.Signal.String()` for the signals a deploy can meet; others are `signal <n>`.
// Divergence (ledger 6b): Go names every signal of its table and appends ` (core dumped)`;
// gadk names the common ones and never reports a core dump.
fn signal_name(sig: i32) -> String {
    todo!("Task 2")
}

/// A finished child's exit and captured output (Go `cmd.Output()` plus the stderr Go keeps in
/// `ExitError.Stderr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Go `exec.Cmd`, the fields `cmd/adkgo` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    /// Go `Cmd.Path`: the program `LookPath` resolved, or the name as given.
    pub path: String,
    /// Go `Cmd.Args`: the name as given, then the arguments.
    pub args: Vec<String>,
    /// Go `Cmd.Dir`: the child's working directory; empty for the caller's.
    pub dir: String,
    /// Go `Cmd.Err`: the `LookPath` failure, returned by `Run` without starting anything.
    pub err: Option<String>,
}

impl Cmd {
    /// Go `exec.Command(name, args...)`: a `name` without a `/` is resolved with `LookPath`
    /// through `PATH` as `env` holds it (an empty `PATH` element is `.`; relative elements are
    /// checked against `cwd` and, as Go reports `ErrDot`, fail).
    pub fn new(env: &dyn EnvProvider, cwd: &Path, name: &str, args: &[&str]) -> Cmd {
        todo!("Task 2")
    }

    /// Sets Go `Cmd.Dir`.
    pub fn with_dir(mut self, dir: impl Into<String>) -> Cmd {
        self.dir = dir.into();
        self
    }
}

/// Go `Cmd.String()`: the resolved path and the arguments, or (when the lookup failed) the
/// name as given and the arguments, space-joined.
impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("Task 2")
    }
}

/// Go `exec.LookPath` on Unix (`lp_unix.go`), returning Go's `(path, err)` pair: `err` is the
/// `*exec.Error` text, and `path` is set with `ErrDot` as Go sets it.
fn look_path(path_var: &str, cwd: &Path, file: &str) -> (String, Option<String>) {
    todo!("Task 2")
}

/// Go `findExecutable`: a non-directory with an execute permission bit.
fn is_executable(p: &Path) -> bool {
    todo!("Task 2")
}

/// Starts processes. `Err` is the text of Go's error from starting the child (`Cmd.Err`
/// for a failed lookup, else the spawn failure); a started child that fails is an `Ok` [`Exit`].
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Go `cmd.Run()` with `cmd.Stdout`/`cmd.Stderr` set: runs `cmd` to completion with stdin
    /// from the null device, copying its output to `stdout` and `stderr` as it arrives.
    async fn run(
        &self,
        cmd: &Cmd,
        stdout: &mut (dyn Write + Send),
        stderr: &mut (dyn Write + Send),
    ) -> Result<Exit, String>;

    /// Runs `cmd` to completion with stdin from the null device and captures its output.
    async fn output(&self, cmd: &Cmd) -> Result<CmdOutput, String>;
}

/// Spawns real processes with tokio.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

impl TokioRunner {
    fn command(cmd: &Cmd) -> Result<tokio::process::Command, String> {
        todo!("Task 2")
    }
}

#[async_trait]
impl CommandRunner for TokioRunner {
    async fn run(
        &self,
        cmd: &Cmd,
        stdout: &mut (dyn Write + Send),
        stderr: &mut (dyn Write + Send),
    ) -> Result<Exit, String> {
        todo!("Task 2")
    }

    async fn output(&self, cmd: &Cmd) -> Result<CmdOutput, String> {
        todo!("Task 2")
    }
}

/// Copies a child pipe to `w` until end of file (Go's `exec.Cmd` copying goroutine).
async fn copy_to<R: AsyncRead + Unpin>(
    pipe: Option<R>,
    w: &mut (dyn Write + Send),
) -> std::io::Result<()> {
    todo!("Task 2")
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    todo!("Task 2")
}
