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

//! Go's default `log` logger (`log.Printf`, `log.Println`), writing to an injectable sink.
//!
//! Every line is `YYYY/MM/DD HH:MM:SS ` (local time, Go `log.LstdFlags`) followed by the message
//! and a newline when the message does not end with one.

use std::io::Write;
use std::sync::{Arc, Mutex};

type Sink = Arc<Mutex<Box<dyn Write + Send>>>;
type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// Go's default `log` logger: `YYYY/MM/DD HH:MM:SS ` (local time) + message + `\n` if missing.
#[derive(Clone)]
pub struct Logger {
    sink: Sink,
    clock: Clock,
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logger").finish_non_exhaustive()
    }
}

/// Go `log.LstdFlags` prefix for the current local time.
fn local_prefix() -> String {
    chrono::Local::now()
        .format("%Y/%m/%d %H:%M:%S ")
        .to_string()
}

impl Logger {
    /// Writes to the process's stderr, as Go's default logger does.
    pub fn stderr() -> Logger {
        Logger::to_writer(std::io::stderr())
    }

    /// Writes to `w` (tests, replay).
    pub fn to_writer(w: impl Write + Send + 'static) -> Logger {
        Logger {
            sink: Arc::new(Mutex::new(Box::new(w))),
            clock: Arc::new(local_prefix),
        }
    }

    /// Replaces the timestamp source (returns the full `YYYY/MM/DD HH:MM:SS ` prefix).
    pub fn with_clock(self, clock: impl Fn() -> String + Send + Sync + 'static) -> Logger {
        Logger {
            sink: self.sink,
            clock: Arc::new(clock),
        }
    }

    /// Go `log.Printf`/`log.Print` with the message already formatted: one write of the prefix,
    /// the message and a trailing newline when `msg` lacks one. Write errors are ignored, as
    /// Go's `log.Printf` ignores them.
    pub fn print(&self, msg: &str) {
        let mut line = (self.clock)();
        line.push_str(msg);
        if !msg.ends_with('\n') {
            line.push('\n');
        }
        let mut sink = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        let _ = sink.write_all(line.as_bytes());
        let _ = sink.flush();
    }

    /// Go `log.Println()` with no arguments (prefix + `\n`).
    pub fn println_empty(&self) {
        self.print("\n");
    }
}

/// A shared in-memory sink for tests: `(Logger, handle)` where `handle.contents()` returns the
/// bytes written so far.
pub fn capture() -> (Logger, Captured) {
    let captured = Captured(Arc::new(Mutex::new(Vec::new())));
    (Logger::to_writer(captured.clone()), captured)
}

/// The buffer behind [`capture`].
#[derive(Clone, Debug, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// The bytes written so far, as text (invalid UTF-8 replaced).
    pub fn contents(&self) -> String {
        let buf = self.0.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(logger: Logger) -> Logger {
        logger.with_clock(|| "2009/11/10 23:00:00 ".to_owned())
    }

    #[test]
    fn print_adds_the_prefix_and_a_missing_newline() {
        let (logger, out) = capture();
        let logger = fixed(logger);
        logger.print("Web servers starts on http://localhost:8080");
        logger.print("already ends\n");
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 Web servers starts on http://localhost:8080\n\
             2009/11/10 23:00:00 already ends\n"
        );
    }

    #[test]
    fn println_empty_and_empty_print_write_a_bare_prefix_line() {
        let (logger, out) = capture();
        let logger = fixed(logger);
        logger.println_empty();
        logger.print("");
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 \n2009/11/10 23:00:00 \n"
        );
    }

    #[test]
    fn a_multi_line_message_is_one_entry_with_one_prefix() {
        let (logger, out) = capture();
        fixed(logger).print("Run failed: boom\n\nArguments:");
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 Run failed: boom\n\nArguments:\n"
        );
    }

    #[test]
    fn clones_share_the_sink() {
        let (logger, out) = capture();
        let a = fixed(logger);
        let b = a.clone();
        a.print("a");
        b.print("b");
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 a\n2009/11/10 23:00:00 b\n"
        );
    }

    #[test]
    fn the_default_clock_is_go_lstdflags_local_time() {
        let (logger, out) = capture();
        logger.print("x");
        let text = out.contents();
        let (prefix, rest) = text.split_at(20);
        assert_eq!(rest, "x\n");
        let parsed = chrono::NaiveDateTime::parse_from_str(prefix, "%Y/%m/%d %H:%M:%S ");
        assert!(
            parsed.is_ok(),
            "prefix {prefix:?} is not YYYY/MM/DD HH:MM:SS "
        );
        let now = chrono::Local::now().naive_local();
        let delta = now - parsed.unwrap_or(now);
        assert!(
            delta.num_seconds().abs() < 60,
            "prefix {prefix:?} is not local now"
        );
    }
}
