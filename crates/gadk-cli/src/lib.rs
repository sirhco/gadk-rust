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

//! gadk-cli: the `gadk` deploy CLI, a port of ADK-Go's `cmd/adkgo` (`adkgo deploy cloudrun`,
//! `adkgo deploy agentengine`) and its helper package `internal/cli/util`.
//!
//! The binary `gadk` deploys a gadk agent (a Cargo binary target) to Cloud Run or Vertex AI
//! Agent Engine. Everything a command touches outside the process (child processes, stdout and
//! stderr, the working directory, the environment, the clock) goes through [`Env`], so tests
//! never spawn `gcloud` or `cargo`, never read the process environment and never change the
//! current directory.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod deploy;
pub mod flags;
pub mod root;
pub mod runner;
pub mod stage;
pub mod target;
pub mod util;

#[cfg(test)]
mod parity_tests;
#[cfg(test)]
pub(crate) mod testutil;

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::Utc;
use gadk_platform::TimeProvider;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{go_path_clean, go_path_join};

use crate::runner::{Cmd, CommandRunner, TokioRunner};

/// A cloneable, shared output stream (Go's `os.Stdout` / `os.Stderr` as the commands use them):
/// every clone writes to the same underlying writer, one `write` call at a time.
#[derive(Clone)]
pub struct SharedWriter(Arc<Mutex<dyn Write + Send>>);

impl SharedWriter {
    /// Wraps `w`.
    pub fn new(w: impl Write + Send + 'static) -> SharedWriter {
        SharedWriter(Arc::new(Mutex::new(w)))
    }

    /// Shares an existing writer (a test keeps its own handle to read what was written).
    pub fn from_shared(w: Arc<Mutex<dyn Write + Send>>) -> SharedWriter {
        SharedWriter(w)
    }

    /// Go `fmt.Fprint(w, s)`: writes `s`, ignoring a write error as `fmt.Println` callers do.
    pub fn write_str(&self, s: &str) {
        todo!("Task 2")
    }

    fn lock(&self) -> MutexGuard<'_, dyn Write + Send + 'static> {
        todo!("Task 2")
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        todo!("Task 2")
    }

    fn flush(&mut self) -> io::Result<()> {
        todo!("Task 2")
    }
}

impl fmt::Debug for SharedWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedWriter")
    }
}

/// Everything a command reads or writes outside the process; Go reads these from globals
/// (`os.Stdout`, `os.Getwd`, `os.Getenv`, `time.Now`, `exec.Command`).
pub struct Env {
    /// Launches child processes (`gcloud`, `cargo metadata`).
    pub runner: Arc<dyn CommandRunner>,
    /// Go `os.Stdout`: step banners, printer lines and child output.
    pub out: SharedWriter,
    /// Go `os.Stderr`: cobra's `Error:` line and usage.
    pub err: SharedWriter,
    /// Go `os.Getwd()`: relative paths resolve against it.
    pub cwd: PathBuf,
    /// Go `os.Getenv` (`PATH` for `exec.Command`'s `LookPath`, `TMPDIR` for `os.TempDir`).
    pub env: Arc<dyn EnvProvider>,
    /// Go `time.Now`.
    pub time: Arc<dyn TimeProvider>,
}

impl Env {
    /// An `Env` from its parts. Outside this file an `Env` is built only here, by
    /// [`Env::process`] or by the test builder, never by a struct literal (plan decision D3).
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        out: SharedWriter,
        err: SharedWriter,
        cwd: PathBuf,
        env: Arc<dyn EnvProvider>,
        time: Arc<dyn TimeProvider>,
    ) -> Env {
        Env {
            runner,
            out,
            err,
            cwd,
            env,
            time,
        }
    }

    /// The production environment: real processes, stdout/stderr, the current directory (empty
    /// when it cannot be read), the process environment and the system clock.
    pub fn process() -> Env {
        todo!("Task 2")
    }

    /// Go `filepath.Abs(path)` against [`Env::cwd`]: an absolute `path` is cleaned, a relative
    /// one is joined to the working directory and cleaned; `""` is the working directory.
    pub fn abs(&self, path: &str) -> PathBuf {
        todo!("Task 2")
    }

    /// Go `os.TempDir()` on Unix: `$TMPDIR`, else `/tmp`.
    pub fn temp_dir(&self) -> PathBuf {
        todo!("Task 2")
    }

    /// Go `exec.Command(name, args...)`, its `LookPath` reading `PATH` from [`Env::env`].
    pub fn command(&self, name: &str, args: &[&str]) -> Cmd {
        todo!("Task 2")
    }
}

/// Runs the `gadk` command line (`args` includes the program name, as `std::env::args_os`) and
/// returns the process exit code: 0, or 1 when a command failed (Go `root.Execute`).
pub async fn run<I, T>(args: I, env: &Env) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    todo!("Task 2")
}
