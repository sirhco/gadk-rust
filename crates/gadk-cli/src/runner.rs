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

//! The process seam: [`Cmd`] is a command with its program looked up in `PATH` and a text form;
//! a [`CommandRunner`] starts it. Production
//! uses [`TokioRunner`]; tests inject a recording runner, so no test spawns `gcloud` or `cargo`.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use gadk_platform::env::EnvProvider;
use gadk_platform::{go_path_join, go_quote};
use tokio::io::{AsyncRead, AsyncReadExt};

/// The error text for a program not found in `PATH`, on Unix.
const ERR_NOT_FOUND: &str = "executable file not found in $PATH";
/// The error text for an executable found relative to the current directory.
const ERR_DOT: &str = "cannot run executable found relative to current directory";

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this status.
    Code(i32),
    /// A signal (this number) terminated it.
    Signal(i32),
}

impl Exit {
    /// Whether the child exited with code 0.
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }

    /// The error text of a failed exit: `exit status 1` for an exit code,
    /// `signal: killed` for a signal; the texts match ADK's deploy tool.
    pub fn go_error(self) -> String {
        match self {
            Exit::Code(code) => format!("exit status {code}"),
            Exit::Signal(sig) => format!("signal: {}", signal_name(sig)),
        }
    }
}

/// The name of a signal, for the signals a deploy can meet; others are `signal <n>`.
// Only the common signals are named and a core dump is never reported.
fn signal_name(sig: i32) -> String {
    let name = match sig {
        1 => "hangup",
        2 => "interrupt",
        3 => "quit",
        4 => "illegal instruction",
        #[cfg(target_os = "linux")]
        6 => "aborted",
        #[cfg(not(target_os = "linux"))]
        6 => "abort trap",
        8 => "floating point exception",
        9 => "killed",
        11 => "segmentation fault",
        13 => "broken pipe",
        14 => "alarm clock",
        15 => "terminated",
        _ => return format!("signal {sig}"),
    };
    name.to_string()
}

/// A finished child's exit and captured output (including the stderr kept for a failed
/// exit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A command: the fields the deploy commands use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    /// The program that `PATH` lookup resolved, or the name as given.
    pub path: String,
    /// The name as given, then the arguments.
    pub args: Vec<String>,
    /// The child's working directory; empty for the caller's.
    pub dir: String,
    /// The `PATH` lookup failure, returned when run without starting anything.
    pub err: Option<String>,
}

impl Cmd {
    /// A command for `name`: a `name` without a `/` is looked up in
    /// `PATH` as `env` holds it (an empty `PATH` element is `.`; relative elements are
    /// checked against `cwd` and fail with the relative-executable error).
    pub fn new(env: &dyn EnvProvider, cwd: &Path, name: &str, args: &[&str]) -> Cmd {
        let mut cmd = Cmd {
            path: name.to_string(),
            args: std::iter::once(name)
                .chain(args.iter().copied())
                .map(str::to_string)
                .collect(),
            dir: String::new(),
            err: None,
        };
        if !name.contains('/') {
            let path_var = env.var("PATH").unwrap_or_default();
            let (lp, err) = look_path(&path_var, cwd, name);
            if !lp.is_empty() {
                cmd.path = lp;
            }
            cmd.err = err;
        }
        cmd
    }

    /// Sets the working directory.
    pub fn with_dir(mut self, dir: impl Into<String>) -> Cmd {
        self.dir = dir.into();
        self
    }
}

/// The command's text: the resolved path and the arguments, or (when the lookup failed) the
/// name as given and the arguments, space-joined.
impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.err.is_some() {
            return f.write_str(&self.args.join(" "));
        }
        f.write_str(&self.path)?;
        for a in self.args.iter().skip(1) {
            write!(f, " {a}")?;
        }
        Ok(())
    }
}

/// Looks `file` up in `path_var` like a Unix shell, returning the `(path, err)` pair: `err` is
/// the lookup error text (`exec: "<file>": <reason>`), and `path` is set when the match is found
/// relative to the current directory.
fn look_path(path_var: &str, cwd: &Path, file: &str) -> (String, Option<String>) {
    let exec_error = |e: &str| Some(format!("exec: {}: {e}", go_quote(file)));
    if matches!(file, "" | "." | "..") {
        return (String::new(), exec_error(ERR_NOT_FOUND));
    }
    // An empty `PATH` has no elements.
    let dirs: Vec<&str> = if path_var.is_empty() {
        Vec::new()
    } else {
        path_var.split(':').collect()
    };
    for dir in dirs {
        let dir = if dir.is_empty() { "." } else { dir };
        let path = go_path_join(&[dir, file]);
        if is_executable(&cwd.join(&path)) {
            if !path.starts_with('/') {
                return (path, exec_error(ERR_DOT));
            }
            return (path, None);
        }
    }
    (String::new(), exec_error(ERR_NOT_FOUND))
}

/// Whether `p` is a non-directory with an execute permission bit.
fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if meta.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Starts processes. `Err` is the text of the error from starting the child (the lookup
/// failure for a failed lookup, else the spawn failure); a started child that fails is an `Ok`
/// [`Exit`].
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Runs `cmd` to completion with stdin
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
        if let Some(err) = &cmd.err {
            return Err(err.clone());
        }
        let mut c = tokio::process::Command::new(&cmd.path);
        c.args(cmd.args.iter().skip(1)).stdin(Stdio::null());
        if !cmd.dir.is_empty() {
            c.current_dir(&cmd.dir);
        }
        Ok(c)
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
        let mut c = TokioRunner::command(cmd)?;
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        // A spawn failure other than the lookup reports Rust's `io::Error` text.
        let mut child = c.spawn().map_err(|e| e.to_string())?;
        let (out_pipe, err_pipe) = (child.stdout.take(), child.stderr.take());
        let (copied_out, copied_err) =
            tokio::join!(copy_to(out_pipe, stdout), copy_to(err_pipe, stderr));
        let status = child.wait().await.map_err(|e| e.to_string())?;
        copied_out.map_err(|e| e.to_string())?;
        copied_err.map_err(|e| e.to_string())?;
        Ok(exit_of(status))
    }

    async fn output(&self, cmd: &Cmd) -> Result<CmdOutput, String> {
        let mut c = TokioRunner::command(cmd)?;
        let out = c.output().await.map_err(|e| e.to_string())?;
        Ok(CmdOutput {
            exit: exit_of(out.status),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// Copies a child pipe to `w` until end of file.
async fn copy_to<R: AsyncRead + Unpin>(
    pipe: Option<R>,
    w: &mut (dyn Write + Send),
) -> std::io::Result<()> {
    let Some(mut pipe) = pipe else {
        return Ok(());
    };
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n])?;
        w.flush()?;
    }
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    if let Some(code) = status.code() {
        return Exit::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Exit::Signal(sig);
        }
    }
    Exit::Code(-1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use gadk_platform::env::MapEnv;
    use pretty_assertions::assert_eq;

    use super::*;

    fn env_with_path(path: &str) -> MapEnv {
        MapEnv(HashMap::from([("PATH".to_string(), path.to_string())]))
    }

    fn executable(dir: &Path, name: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.display().to_string()
    }

    #[test]
    fn exit_texts_are_go_process_state_strings() {
        assert_eq!(Exit::Code(1).go_error(), "exit status 1");
        assert_eq!(Exit::Code(127).go_error(), "exit status 127");
        assert_eq!(Exit::Signal(9).go_error(), "signal: killed");
        assert_eq!(Exit::Signal(2).go_error(), "signal: interrupt");
        assert_eq!(Exit::Signal(15).go_error(), "signal: terminated");
        assert_eq!(Exit::Signal(64).go_error(), "signal: signal 64");
        assert!(Exit::Code(0).success());
        assert!(!Exit::Code(2).success());
        assert!(!Exit::Signal(9).success());
    }

    #[test]
    fn command_resolves_the_first_executable_on_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // A non-executable file and a directory of the same name are skipped, as in Go.
        std::fs::write(a.path().join("gcloud"), "").unwrap();
        std::fs::create_dir(b.path().join("gcloud")).unwrap();
        let c = tempfile::tempdir().unwrap();
        let want = executable(c.path(), "gcloud");
        let path = format!(
            "{}:{}:{}",
            a.path().display(),
            b.path().display(),
            c.path().display()
        );
        let cmd = Cmd::new(
            &env_with_path(&path),
            Path::new("/"),
            "gcloud",
            &["run", "x"],
        )
        .with_dir("/tmp/d");
        assert_eq!(cmd.path, want);
        assert_eq!(cmd.args, ["gcloud", "run", "x"]);
        assert_eq!(cmd.dir, "/tmp/d");
        assert_eq!(cmd.err, None);
        assert_eq!(cmd.to_string(), format!("{want} run x"));
    }

    #[test]
    fn a_failed_lookup_keeps_the_name_and_prints_the_args() {
        let cmd = Cmd::new(
            &env_with_path("/nonexistent"),
            Path::new("/"),
            "gcloud",
            &["a"],
        );
        assert_eq!(cmd.path, "gcloud");
        assert_eq!(
            cmd.err.as_deref(),
            Some(r#"exec: "gcloud": executable file not found in $PATH"#)
        );
        assert_eq!(cmd.to_string(), "gcloud a");
        // An unset PATH is Go's empty `filepath.SplitList`.
        let cmd = Cmd::new(&MapEnv::default(), Path::new("/"), "gcloud", &[]);
        assert_eq!(
            cmd.err.as_deref(),
            Some(r#"exec: "gcloud": executable file not found in $PATH"#)
        );
    }

    #[test]
    fn a_relative_path_entry_is_go_err_dot() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join("bin")).unwrap();
        executable(&cwd.path().join("bin"), "tool");
        let cmd = Cmd::new(&env_with_path("bin"), cwd.path(), "tool", &["x"]);
        assert_eq!(cmd.path, "bin/tool");
        assert_eq!(
            cmd.err.as_deref(),
            Some(r#"exec: "tool": cannot run executable found relative to current directory"#)
        );
        assert_eq!(cmd.to_string(), "tool x");
        // An empty element is ".".
        executable(cwd.path(), "dot");
        let cmd = Cmd::new(&env_with_path(":/nonexistent"), cwd.path(), "dot", &[]);
        assert_eq!(cmd.path, "dot");
        assert!(cmd.err.as_deref().unwrap_or_default().contains("relative"));
    }

    #[test]
    fn a_name_with_a_slash_is_not_looked_up() {
        let cmd = Cmd::new(&MapEnv::default(), Path::new("/"), "./gadk", &["x"]);
        assert_eq!((cmd.path.as_str(), cmd.err), ("./gadk", None));
        let cmd = Cmd::new(&MapEnv::default(), Path::new("/"), "", &[]);
        assert_eq!(
            cmd.err.as_deref(),
            Some(r#"exec: "": executable file not found in $PATH"#)
        );
    }

    /// A real child (`/bin/sh`, no lookup): output is streamed to the two writers, the working
    /// directory and the exit status are honoured, stdin is the null device.
    #[cfg(unix)]
    #[tokio::test]
    async fn tokio_runner_streams_output_and_reports_the_exit() {
        let dir = tempfile::tempdir().unwrap();
        let dir_s = dir.path().canonicalize().unwrap().display().to_string();
        let script = "pwd; printf err >&2; read x || printf eof; exit 3";
        let cmd = Cmd::new(
            &MapEnv::default(),
            Path::new("/"),
            "/bin/sh",
            &["-c", script],
        )
        .with_dir(dir_s.clone());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let got = TokioRunner.run(&cmd, &mut out, &mut err).await;
        assert_eq!(got, Ok(Exit::Code(3)));
        assert_eq!(String::from_utf8(out).unwrap(), format!("{dir_s}\neof"));
        assert_eq!(String::from_utf8(err).unwrap(), "err");

        let got = TokioRunner.output(&cmd).await.unwrap();
        assert_eq!(got.exit, Exit::Code(3));
        assert_eq!(
            String::from_utf8(got.stdout).unwrap(),
            format!("{dir_s}\neof")
        );
        assert_eq!(got.stderr, b"err");
    }

    #[tokio::test]
    async fn tokio_runner_returns_the_lookup_error_without_starting() {
        let cmd = Cmd::new(&MapEnv::default(), Path::new("/"), "gcloud", &[]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let got = TokioRunner.run(&cmd, &mut out, &mut err).await;
        assert_eq!(
            got,
            Err(r#"exec: "gcloud": executable file not found in $PATH"#.to_string())
        );
        assert_eq!(
            TokioRunner.output(&cmd).await,
            Err(r#"exec: "gcloud": executable file not found in $PATH"#.to_string())
        );
    }
}
