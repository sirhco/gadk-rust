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
use gadk_vertexai::reasoning_engine::ReasoningEngineOptions;

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
        let mut w = self.lock();
        let _ = w.write_all(s.as_bytes());
        let _ = w.flush();
    }

    fn lock(&self) -> MutexGuard<'_, dyn Write + Send + 'static> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.lock().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.lock().flush()
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
    /// How `gadk deploy agentengine` reaches Vertex AI (Go: the `aiplatform` client options;
    /// endpoint, credentials and HTTP client are injectable for tests). An unset
    /// [`ReasoningEngineOptions::env`] means [`Env::env`].
    pub vertex: ReasoningEngineOptions,
}

impl Env {
    /// An `Env` from its parts, with default Vertex options (tests set [`Env::vertex`] on the
    /// built value). Outside this file an `Env` is built only here, by [`Env::process`] or by
    /// the test builder, never by a struct literal (plan decision D3).
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
            vertex: ReasoningEngineOptions::default(),
        }
    }

    /// The production environment: real processes, stdout/stderr, the current directory (empty
    /// when it cannot be read), the process environment, the system clock and Vertex AI at
    /// its regional endpoint with Application Default Credentials.
    pub fn process() -> Env {
        Env::new(
            Arc::new(TokioRunner),
            SharedWriter::new(io::stdout()),
            SharedWriter::new(io::stderr()),
            std::env::current_dir().unwrap_or_default(),
            Arc::new(ProcessEnv),
            Arc::new(Utc::now),
        )
    }

    /// Go `filepath.Abs(path)` against [`Env::cwd`]: an absolute `path` is cleaned, a relative
    /// one is joined to the working directory and cleaned; `""` is the working directory.
    pub fn abs(&self, path: &str) -> PathBuf {
        if path.starts_with('/') {
            return PathBuf::from(go_path_clean(path));
        }
        let cwd = self.cwd.to_string_lossy();
        PathBuf::from(go_path_join(&[cwd.as_ref(), path]))
    }

    /// Go `os.TempDir()` on Unix: `$TMPDIR`, else `/tmp`.
    pub fn temp_dir(&self) -> PathBuf {
        match self.env.var("TMPDIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from("/tmp"),
        }
    }

    /// Go `exec.Command(name, args...)`, its `LookPath` reading `PATH` from [`Env::env`].
    pub fn command(&self, name: &str, args: &[&str]) -> Cmd {
        Cmd::new(self.env.as_ref(), &self.cwd, name, args)
    }
}

/// Runs the `gadk` command line (`args` includes the program name, as `std::env::args_os`) and
/// returns the process exit code: 0, or 1 when a command failed (Go `root.Execute`).
pub async fn run<I, T>(args: I, env: &Env) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    ExitCode::from(root::execute(args, env).await)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use pretty_assertions::assert_eq;

    use crate::testutil::TestEnv;

    #[test]
    fn abs_resolves_against_the_working_directory_like_go_filepath_abs() {
        let t = TestEnv::builder("/work/space").build();
        assert_eq!(t.env.abs(""), PathBuf::from("/work/space"));
        assert_eq!(t.env.abs("."), PathBuf::from("/work/space"));
        assert_eq!(t.env.abs("agent"), PathBuf::from("/work/space/agent"));
        assert_eq!(t.env.abs("../x/./y/"), PathBuf::from("/work/x/y"));
        assert_eq!(t.env.abs("/abs//p/../q"), PathBuf::from("/abs/q"));
    }

    #[test]
    fn temp_dir_is_go_os_temp_dir() {
        let t = TestEnv::builder("/w").build();
        assert_eq!(t.env.temp_dir(), PathBuf::from("/tmp"));
        let t = TestEnv::builder("/w").var("TMPDIR", "").build();
        assert_eq!(t.env.temp_dir(), PathBuf::from("/tmp"));
        let t = TestEnv::builder("/w").var("TMPDIR", "/var/tmp/x").build();
        assert_eq!(t.env.temp_dir(), PathBuf::from("/var/tmp/x"));
    }

    #[test]
    fn command_looks_the_program_up_through_the_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("gcloud");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        make_executable(&bin);
        let path = format!("/nonexistent:{}", dir.path().display());
        let t = TestEnv::builder("/w")
            .vars(HashMap::from([("PATH".to_string(), path)]))
            .build();
        let cmd = t.env.command("gcloud", &["run", "deploy"]);
        assert_eq!(cmd.path, bin.display().to_string());
        assert_eq!(cmd.err, None);
        assert_eq!(cmd.to_string(), format!("{} run deploy", bin.display()));
    }

    #[cfg(unix)]
    fn make_executable(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_: &Path) {}

    /// `build.rs` stores the second word of `$RUSTC --version` (R5).
    #[test]
    fn build_script_captures_the_compiling_rustc_version() {
        let v = crate::target::COMPILED_RUSTC_VERSION;
        // e.g. `1.99.1` (from `rustc 1.99.1 (…)`) or `1.100.0-nightly`.
        assert!(
            v.starts_with(|c: char| c.is_ascii_digit()) && v.contains('.'),
            "unexpected rustc version {v:?}"
        );
    }

    /// Plan D3: `Env::new` (and so the test builder) starts from the default Vertex options:
    /// the regional endpoint, Application Default Credentials, reqwest, [`Env::env`].
    #[test]
    fn env_vertex_defaults_to_the_regional_endpoint_and_adc() {
        let t = TestEnv::builder("/w").build();
        let v = &t.env.vertex;
        assert_eq!(v.endpoint, None);
        assert!(v.credentials.is_none());
        assert!(!v.without_authentication);
        assert!(v.http_client.is_none());
        assert!(v.env.is_none());
    }

    #[test]
    fn shared_writer_clones_write_to_one_buffer() {
        use std::io::Write;
        let t = TestEnv::builder("/w").build();
        let mut a = t.env.out.clone();
        a.write_all(b"one ").unwrap();
        t.env.out.write_str("two");
        assert_eq!(t.out.text(), "one two");
        assert_eq!(t.err.text(), "");
    }
}
