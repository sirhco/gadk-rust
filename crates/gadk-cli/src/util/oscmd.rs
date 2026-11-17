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

//! Console helpers: the colour constants, the step [`Printer`],
//! [`log_start_stop`]'s banners, the [`ReprintableStream`] that prefixes child output, and
//! [`log_command`]. Everything is written to the given
//! [`SharedWriter`] (the command's `Env::out`), byte for byte as ADK's deploy tool writes it.

use std::fmt::{self, Display};
use std::io::{self, Write};

use crate::SharedWriter;
use crate::runner::{Cmd, CommandRunner};

pub const RESET: &str = "\x1b[0m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const BLUE: &str = "\x1b[34m";
pub const MAGENTA: &str = "\x1b[35m";
pub const CYAN: &str = "\x1b[36m";
pub const GRAY: &str = "\x1b[37m";
pub const WHITE: &str = "\x1b[97m";

/// The step printer `log_start_stop` builds. The arguments of one call arrive as one list, so a
/// line is `    `, a green `> ` and a reset, a space, then `[`, the arguments' text joined by
/// single spaces, and `]`.
#[derive(Clone, Debug)]
pub struct Printer {
    out: SharedWriter,
}

impl Printer {
    /// A printer writing to `out`.
    pub fn new(out: SharedWriter) -> Printer {
        Printer { out }
    }

    /// Prints one line. Each argument's [`Display`] must be its plain text (a `Cmd` displays
    /// as its command line, a `GoDuration` as its duration string such as `1m0s`).
    pub fn print(&self, a: &[&dyn Display]) {
        let args: Vec<String> = a.iter().map(|x| x.to_string()).collect();
        self.out
            .write_str(&format!("    {GREEN}> {RESET} [{}]\n", args.join(" ")));
    }

    /// The writer this printer prints to (the command's stdout).
    pub fn out(&self) -> SharedWriter {
        self.out.clone()
    }
}

/// Prints `<msg> : Starting`, runs `command` with a [`Printer`], prints an
/// empty line, then `<msg> : Finished successfully` or `<msg> : Finished with error` and
/// `Error: <err>`, and returns `command`'s result.
pub async fn log_start_stop<F>(out: &SharedWriter, msg: &str, command: F) -> Result<(), String>
where
    F: AsyncFnOnce(Printer) -> Result<(), String>,
{
    out.write_str(&format!("{msg} : {GREEN}Starting{RESET}\n"));
    let result = command(Printer::new(out.clone())).await;
    out.write_str("\n");
    match &result {
        Ok(()) => out.write_str(&format!("{msg} : {GREEN}Finished successfully{RESET}\n")),
        Err(err) => {
            out.write_str(&format!("{msg} : {RED}Finished with error{RESET}\n"));
            out.write_str(&format!("Error: {err}\n"));
        }
    }
    result
}

/// Forwards a child's output to `stream`, writing `prefix` first and
/// in place of every newline, so each output line is indented under the step.
pub struct ReprintableStream<W: Write> {
    prefix: Vec<u8>,
    clean: bool,
    stream: W,
}

impl<W: Write> fmt::Debug for ReprintableStream<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReprintableStream")
            .field("prefix", &String::from_utf8_lossy(&self.prefix))
            .field("clean", &self.clean)
            .finish()
    }
}

/// Builds a stream over `s`: the prefix is
/// `"\n       " + color + prefix + " > " + Reset`.
pub fn new_reprintable_stream<W: Write>(s: W, prefix: &str, color: &str) -> ReprintableStream<W> {
    ReprintableStream {
        prefix: format!("\n       {color}{prefix} > {RESET}").into_bytes(),
        clean: true,
        stream: s,
    }
}

impl<W: Write> Write for ReprintableStream<W> {
    /// Reports all of `p` written once the pieces are.
    fn write(&mut self, p: &[u8]) -> io::Result<usize> {
        if self.clean {
            self.stream.write_all(&self.prefix)?;
            self.clean = false;
        }
        let mut start = 0;
        for (i, &c) in p.iter().enumerate() {
            if c == b'\n' {
                self.stream.write_all(&p[start..i])?;
                self.stream.write_all(&self.prefix)?;
                start = i + 1;
            }
        }
        if start < p.len() {
            self.stream.write_all(&p[start..])?;
        }
        Ok(p.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Prints `Running : <dir> <cmd>` (yellow `Running : `, the directory, a reset, a space and the
/// command), then runs `cmd` with its stdout and stderr each through a
/// [`ReprintableStream`] onto the printer's writer (both go to the command's stdout). A failed
/// start is its error text; a non-zero exit is `exit status <n>`.
pub async fn log_command(runner: &dyn CommandRunner, cmd: &Cmd, p: &Printer) -> Result<(), String> {
    p.print(&[&"Running : ", &YELLOW, &cmd.dir, &RESET, &" ", cmd]);
    let mut stdout = new_reprintable_stream(p.out(), "  ", YELLOW);
    let mut stderr = new_reprintable_stream(p.out(), "  ", YELLOW);
    let exit = runner.run(cmd, &mut stdout, &mut stderr).await?;
    if exit.success() {
        Ok(())
    } else {
        Err(exit.go_error())
    }
}

/// `p` without its extension, which must be
/// `expected`.
pub fn strip_extension(p: &str, expected: &str) -> Result<String, String> {
    let ex = path_ext(p);
    if ex.is_empty() {
        return Err(format!("Cannot find extension in '{p}'"));
    }
    if ex != expected {
        return Err(format!(
            "Unexpected extension. Found '{ex}' instead of '{expected}'"
        ));
    }
    Ok(p[..p.len() - ex.len()].to_string())
}

/// The suffix from the final `.` of the last slash-separated element.
fn path_ext(p: &str) -> &str {
    for (i, c) in p.bytes().enumerate().rev() {
        if c == b'/' {
            break;
        }
        if c == b'.' {
            return &p[i..];
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::runner::Exit;
    use crate::testutil::{Canned, RecordingRunner, TestEnv};

    #[test]
    fn colour_constants_are_go_ansi_codes() {
        assert_eq!(
            [RESET, RED, GREEN, YELLOW, BLUE, MAGENTA, CYAN, GRAY, WHITE],
            [
                "\x1b[0m", "\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[34m", "\x1b[35m", "\x1b[36m",
                "\x1b[37m", "\x1b[97m"
            ]
        );
    }

    /// Expected bytes are Go's `fmt.Println("    "+Green+"> "+Reset, a)` for the same
    /// arguments (Go 1.27).
    #[test]
    fn printer_prints_the_argument_slice_like_go_println() {
        let t = TestEnv::builder("/w").build();
        let p = Printer::new(t.env.out.clone());
        p.print(&[&"Using temp dir:", &"/tmp/x"]);
        p.print(&[]);
        p.print(&[&"a", &3, &gadk_platform::GoDuration(1_000_000_000)]);
        assert_eq!(
            t.out.text(),
            "    \x1b[32m> \x1b[0m [Using temp dir: /tmp/x]\n\
             \x20   \x1b[32m> \x1b[0m []\n\
             \x20   \x1b[32m> \x1b[0m [a 3 1s]\n"
        );
    }

    #[tokio::test]
    async fn log_start_stop_prints_go_banners_on_success() {
        let t = TestEnv::builder("/w").build();
        let got = log_start_stop(&t.env.out, "Preparing Dockerfile", async |p: Printer| {
            p.print(&[&"Writing:", &"/tmp/x/Dockerfile"]);
            Ok(())
        })
        .await;
        assert_eq!(got, Ok(()));
        assert_eq!(
            t.out.text(),
            "Preparing Dockerfile : \x1b[32mStarting\x1b[0m\n\
             \x20   \x1b[32m> \x1b[0m [Writing: /tmp/x/Dockerfile]\n\
             \n\
             Preparing Dockerfile : \x1b[32mFinished successfully\x1b[0m\n"
        );
        assert_eq!(t.err.text(), "");
    }

    #[tokio::test]
    async fn log_start_stop_prints_go_banners_on_error() {
        let t = TestEnv::builder("/w").build();
        let got = log_start_stop(&t.env.out, "Cleaning temp", async |_p: Printer| {
            Err("failed to clean temp directory /x: boom".to_string())
        })
        .await;
        assert_eq!(
            got,
            Err("failed to clean temp directory /x: boom".to_string())
        );
        assert_eq!(
            t.out.text(),
            "Cleaning temp : \x1b[32mStarting\x1b[0m\n\
             \n\
             Cleaning temp : \x1b[31mFinished with error\x1b[0m\n\
             Error: failed to clean temp directory /x: boom\n"
        );
    }

    const PREFIX: &str = "\n       \x1b[33m   > \x1b[0m";

    /// Go `reprintableStream.Write`: the prefix before the first byte and in place of each
    /// newline, across several writes.
    #[test]
    fn reprintable_stream_prefixes_every_line() {
        let mut buf = Vec::new();
        {
            let mut s = new_reprintable_stream(&mut buf, "  ", YELLOW);
            assert_eq!(s.write(b"one\ntw").unwrap(), 6);
            assert_eq!(s.write(b"o\n").unwrap(), 2);
            assert_eq!(s.write(b"").unwrap(), 0);
            assert_eq!(s.write(b"\nthree").unwrap(), 6);
        }
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            format!("{PREFIX}one{PREFIX}two{PREFIX}{PREFIX}three")
        );
    }

    #[test]
    fn reprintable_stream_writes_the_prefix_even_for_an_empty_first_write() {
        let mut buf = Vec::new();
        {
            let mut s = new_reprintable_stream(&mut buf, "  ", YELLOW);
            s.write_all(b"").unwrap();
        }
        // `write_all` of nothing makes no `write` call, so nothing is written (as Go's
        // `io.Copy`, which never writes an empty chunk).
        assert_eq!(buf, b"");
    }

    #[tokio::test]
    async fn log_command_prints_running_and_streams_child_output() {
        let runner = RecordingRunner::with_responder(|_| Canned {
            stdout: b"Deploying...\nDone.\n".to_vec(),
            stderr: b"warning\n".to_vec(),
            result: Ok(Exit::Code(0)),
        });
        let t = TestEnv::builder("/w").runner(runner).build();
        let cmd = t
            .env
            .command("gcloud", &["run", "deploy", "svc"])
            .with_dir("/tmp/t");
        let p = Printer::new(t.env.out.clone());
        let got = log_command(t.env.runner.as_ref(), &cmd, &p).await;
        assert_eq!(got, Ok(()));
        // PATH is unset, so the lookup fails and Go's `Cmd.String()` is the name and args.
        assert_eq!(
            t.out.text(),
            format!(
                "    \x1b[32m> \x1b[0m [Running :  \x1b[33m /tmp/t \x1b[0m   gcloud run deploy svc]\n\
                 {PREFIX}Deploying...{PREFIX}Done.{PREFIX}{PREFIX}warning{PREFIX}"
            )
        );
        assert_eq!(t.runner.calls(), vec![cmd]);
    }

    #[tokio::test]
    async fn log_command_reports_go_errors() {
        let runner = RecordingRunner::with_responder(|c| {
            if c.args[1] == "exit" {
                Canned::exit(2)
            } else {
                Canned::start_error(r#"exec: "gcloud": executable file not found in $PATH"#)
            }
        });
        let t = TestEnv::builder("/w").runner(runner).build();
        let p = Printer::new(t.env.out.clone());
        let exit = t.env.command("gcloud", &["exit"]);
        assert_eq!(
            log_command(t.env.runner.as_ref(), &exit, &p).await,
            Err("exit status 2".to_string())
        );
        let start = t.env.command("gcloud", &["start"]);
        assert_eq!(
            log_command(t.env.runner.as_ref(), &start, &p).await,
            Err(r#"exec: "gcloud": executable file not found in $PATH"#.to_string())
        );
    }

    #[test]
    fn strip_extension_is_go_strip_extension() {
        assert_eq!(strip_extension("main.go", ".go"), Ok("main".to_string()));
        assert_eq!(
            strip_extension("cmd/agent/main.go", ".go"),
            Ok("cmd/agent/main".to_string())
        );
        assert_eq!(
            strip_extension("a.b/main", ".go"),
            Err("Cannot find extension in 'a.b/main'".to_string())
        );
        assert_eq!(
            strip_extension("main.rs", ".go"),
            Err("Unexpected extension. Found '.rs' instead of '.go'".to_string())
        );
        assert_eq!(
            strip_extension("archive.tar.gz", ".gz"),
            Ok("archive.tar".to_string())
        );
        assert_eq!(strip_extension(".go", ".go"), Ok(String::new()));
    }
}
