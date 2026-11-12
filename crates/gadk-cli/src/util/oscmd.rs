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

//! Port of ADK-Go `internal/cli/util/oscmd.go`: the colour constants, the step [`Printer`],
//! [`log_start_stop`]'s banners, the [`ReprintableStream`] that prefixes child output, and
//! [`log_command`]. Go prints to `os.Stdout`; here everything goes to the given
//! [`SharedWriter`] (the command's `Env::out`), byte for byte as Go writes it.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

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

/// Go `util.Printer` as `LogStartStop` builds it:
/// `func(a ...any) { fmt.Println("    "+Green+"> "+Reset, a) }`. The arguments arrive as one
/// `[]any`, so a line is the green marker, a space and Go's `%v` of the slice: `[` + the
/// arguments' `%v` joined by single spaces + `]`.
#[derive(Clone, Debug)]
pub struct Printer {
    out: SharedWriter,
}

impl Printer {
    /// A printer writing to `out`.
    pub fn new(out: SharedWriter) -> Printer {
        Printer { out }
    }

    /// Prints one line. Each argument's [`Display`] must be its Go `%v` (a `Cmd` displays
    /// as Go's `Cmd.String()`, a `GoDuration` as `Duration.String()`).
    pub fn print(&self, a: &[&dyn Display]) {
        todo!("Task 2")
    }

    /// The writer this printer prints to (Go `os.Stdout`).
    pub fn out(&self) -> SharedWriter {
        self.out.clone()
    }
}

/// Go `LogStartStop`: prints `<msg> : Starting`, runs `command` with a [`Printer`], prints an
/// empty line, then `<msg> : Finished successfully` or `<msg> : Finished with error` and
/// `Error: <err>`, and returns `command`'s result.
pub async fn log_start_stop<F>(out: &SharedWriter, msg: &str, command: F) -> Result<(), String>
where
    F: AsyncFnOnce(Printer) -> Result<(), String>,
{
    todo!("Task 2")
}

/// Go `reprintableStream`: forwards a child's output to `stream`, writing `prefix` first and
/// in place of every newline, so each output line is indented under the step.
pub struct ReprintableStream<W: Write> {
    prefix: Vec<u8>,
    clean: bool,
    stream: W,
}

impl<W: Write> fmt::Debug for ReprintableStream<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("Task 2")
    }
}

/// Go `newReprintableStream(s, prefix, color)`: the prefix is
/// `"\n       " + color + prefix + " > " + Reset`.
pub fn new_reprintable_stream<W: Write>(s: W, prefix: &str, color: &str) -> ReprintableStream<W> {
    todo!("Task 2")
}

impl<W: Write> Write for ReprintableStream<W> {
    /// Go `reprintableStream.Write`: reports all of `p` written once the pieces are.
    fn write(&mut self, p: &[u8]) -> io::Result<usize> {
        todo!("Task 2")
    }

    fn flush(&mut self) -> io::Result<()> {
        todo!("Task 2")
    }
}

/// Go `LogCommand`: prints `Running : <dir> <cmd>` (Go `p("Running : ", Yellow, c.Dir, Reset,
/// " ", c)`), then runs `cmd` with its stdout and stderr each through a
/// [`ReprintableStream`] onto the printer's writer (Go sends both to `os.Stdout`). A failed
/// start is its error text; a non-zero exit is `exit status <n>`.
pub async fn log_command(runner: &dyn CommandRunner, cmd: &Cmd, p: &Printer) -> Result<(), String> {
    todo!("Task 2")
}

/// Go `StripExtension(p, expected)`: `p` without its extension (Go `path.Ext`), which must be
/// `expected`.
pub fn strip_extension(p: &str, expected: &str) -> Result<String, String> {
    todo!("Task 2")
}

/// Go `path.Ext`: the suffix from the final `.` of the last slash-separated element.
fn path_ext(p: &str) -> &str {
    todo!("Task 2")
}
