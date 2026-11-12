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

//! Go `cmd/launcher/console`: runs an agent interactively, reading user messages line by line
//! from stdin and printing the agent's answers to stdout.
//!
//! Divergences (ledger 6): stdin is read on a dedicated thread (Go: a goroutine), which stays
//! blocked on stdin after `run` returns, as Go's does; tokio's Ctrl-C handler stays installed
//! after `run` returns (Go's `NotifyContext` stop restores the default). A cancellation during a
//! turn ends `run` at once (Go lets the run report its context error first). A stdin read error
//! returns an error with Go's `log.Fatal` text instead of exiting the process.

pub(crate) mod hitl;

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::StreamExt;
use gadk_agent::{RunConfig, StreamingMode};
use gadk_engine::runner::{RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, Part, ROLE_USER};
use gadk_platform::GoDuration;
use gadk_platform::Platform;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use self::hitl::{
    PendingInterrupt, build_interrupt_response, collect_pending_interrupts,
    render_interrupt_prompt, render_output,
};
use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::launcher::{Launcher, SubLauncher};
use crate::telemetry;
use crate::universal::error_on_unparsed_args;

/// Line source for the console (default: stdin read on a blocking task).
pub type InputLines = Box<dyn Iterator<Item = std::io::Result<String>> + Send>;
/// Output sink for the console's `fmt.Print*` text (default: stdout).
pub type Output = std::sync::Arc<std::sync::Mutex<dyn std::io::Write + Send>>;

/// The console's user and app; Go: "not important at this moment, we can just use any".
const USER_ID: &str = "console_user";
const APP_NAME: &str = "console_app";

/// Go `consoleLauncher` with its `consoleConfig`.
pub struct ConsoleLauncher {
    /// Go `flags`; a mutex because a `FlagSet` is not `Sync` (its func flags are `FnMut`).
    flags: Mutex<FlagSet>,
    streaming_mode_string: Flag<String>,
    shutdown_timeout: Flag<GoDuration>,
    otel_to_cloud: Flag<bool>,
    /// Go `consoleConfig.streamingMode`, set by `parse`; `None` is Go's empty mode (decided by
    /// whether stdout is a character device).
    streaming_mode: Option<StreamingMode>,
    /// Injected input; `None` reads stdin. Taken by the first `run`.
    input: Mutex<Option<InputLines>>,
    output: Output,
    /// Injected answer to Go's `ModeCharDevice` check on stdout; `None` checks stdout.
    stdout_is_char_device: Option<bool>,
}

/// Go `console.NewLauncher()`.
pub fn new() -> ConsoleLauncher {
    let mut fs = FlagSet::new("console");
    let streaming_mode_string =
        fs.string("streaming_mode", "", "defines streaming mode (none|sse)");
    let shutdown_timeout = fs.duration(
        "shutdown-timeout",
        GoDuration(2_000_000_000),
        "Console shutdown timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for waiting for active requests to finish during shutdown",
    );
    let otel_to_cloud = fs.bool(
        "otel_to_cloud",
        false,
        "Enables OpenTelemetry export to GCP: telemetry.googleapis.com. When not set, a telemetry.WithOtelToCloud option in launcher.Config.TelemetryOptions decides. See adk-go/telemetry package for details about supported options, credentials and environment variables.",
    );
    ConsoleLauncher {
        flags: Mutex::new(fs),
        streaming_mode_string,
        shutdown_timeout,
        otel_to_cloud,
        streaming_mode: None,
        input: Mutex::new(None),
        output: Arc::new(Mutex::new(std::io::stdout())),
        stdout_is_char_device: None,
    }
}

impl ConsoleLauncher {
    /// Replaces stdin/stdout and the char-device check (tests, replay).
    pub fn with_io(
        self,
        input: InputLines,
        output: Output,
        stdout_is_char_device: bool,
    ) -> ConsoleLauncher {
        ConsoleLauncher {
            input: Mutex::new(Some(input)),
            output,
            stdout_is_char_device: Some(stdout_is_char_device),
            ..self
        }
    }

    /// Replaces the flag set's usage and error output (Go `fs.SetOutput`; default stderr).
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> ConsoleLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// Go `consoleLauncher.Execute` over the telemetry [`telemetry::Globals`] given: validate,
    /// parse, refuse leftovers, run.
    async fn execute_in(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
        globals: telemetry::Globals,
    ) -> Result<(), Error> {
        config.validate()?;
        let remaining = self
            .parse(args)
            .map_err(|e| wrapped("cannot parse args", e))?;
        error_on_unparsed_args(&remaining)
            .map_err(|e| wrapped("cannot parse all the arguments", e))?;
        self.run_in(cancel, config, globals).await
    }

    /// Go `Execute` with the global subscriber installation injected (`install` returns `false`
    /// when one is already set) over an empty environment, leaving the global OpenTelemetry
    /// providers alone: tests never touch the process-global dispatcher, providers or
    /// environment.
    #[cfg(test)]
    pub(crate) async fn execute_with(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
        install: impl FnOnce(tracing::Dispatch) -> bool + Send + 'static,
    ) -> Result<(), Error> {
        self.execute_in(cancel, config, args, telemetry::Globals::injected(install))
            .await
    }

    /// Go `Run` with the global subscriber installation injected, as
    /// [`ConsoleLauncher::execute_with`].
    #[cfg(test)]
    pub(crate) async fn run_with(
        &self,
        cancel: CancellationToken,
        config: Config,
        install: impl FnOnce(tracing::Dispatch) -> bool + Send + 'static,
    ) -> Result<(), Error> {
        self.run_in(cancel, config, telemetry::Globals::injected(install))
            .await
    }

    /// Go `Run` over the telemetry [`telemetry::Globals`] given: Ctrl-C cancels a child token
    /// (Go `signal.NotifyContext(ctx, os.Interrupt)`), telemetry starts, then the session,
    /// runner and interaction loop; telemetry is shut down under `-shutdown-timeout`.
    pub(crate) async fn run_in(
        &self,
        cancel: CancellationToken,
        mut config: Config,
        globals: telemetry::Globals,
    ) -> Result<(), Error> {
        let ctx = cancel.child_token();
        // Go `defer cancel()`: ends the Ctrl-C watcher when `run` returns.
        let _stop = ctx.clone().drop_guard();
        {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::select! {
                    r = tokio::signal::ctrl_c() => if r.is_ok() { ctx.cancel() },
                    _ = ctx.cancelled() => {}
                }
            });
        }

        let telemetry = telemetry::init_with(&mut config, self.otel_to_cloud.get(), globals)
            .await
            .map_err(|e| wrapped("telemetry initialization failed", e))?;

        let result = self.run_loop(&ctx, &config).await;

        // Go's deferred `telemetry.Shutdown` under `-shutdown-timeout`.
        let timeout = self.shutdown_timeout.get().to_std().unwrap_or_default();
        telemetry.shutdown_logged(timeout, &config.log).await;
        result
    }

    /// Go `Run` after telemetry is up: session, runner and the read-eval-print loop on `ctx`.
    async fn run_loop(&self, ctx: &CancellationToken, config: &Config) -> Result<(), Error> {
        let session_service: Arc<dyn SessionService> = match &config.session_service {
            Some(s) => s.clone(),
            None => Arc::new(InMemorySessionService::new()),
        };
        let sess = session_service
            .create(CreateRequest {
                app_name: APP_NAME.to_owned(),
                user_id: USER_ID.to_owned(),
                ..Default::default()
            })
            .await
            .map_err(|e| wrapped("failed to create the session service", e))?;
        let session_id = sess.id().to_owned();

        let runner = Runner::new(RunnerConfig {
            app_name: APP_NAME.to_owned(),
            agent: config.agent_loader.root_agent(),
            session_service,
            artifact_service: config.artifact_service.clone(),
            memory_service: config.memory_service.clone(),
            plugin_config: config.plugin_config.clone(),
            auto_create_session: false,
            platform: Platform::default(),
            compaction: config.compaction.clone(),
        })
        .map_err(|e| wrapped("failed to create runner", e))?;

        let input = take_or(&self.input, || {
            input_lines(std::io::BufReader::new(std::io::stdin()))
        });
        let mut lines = spawn_reader(input)?;

        // Print an initial newline to work around PTY/exec buffering issues in some environments.
        self.print("\n");
        self.print("\nUser -> ");

        // Resolve the unset streaming mode once per session (stdout's kind does not change).
        let default_streaming_mode = self.streaming_mode.unwrap_or_else(|| {
            if self
                .stdout_is_char_device
                .unwrap_or_else(stdout_is_char_device)
            {
                StreamingMode::Sse
            } else {
                StreamingMode::None
            }
        });

        // Human-input prompts the agent emitted on the previous turn. While non-empty, the next
        // line answers its head; once every prompt has an answer the assembled function
        // responses are sent as the next user message.
        let mut pending_interrupts: Vec<PendingInterrupt> = Vec::new();
        let mut pending_responses: Vec<Part> = Vec::new();

        loop {
            let item = tokio::select! {
                _ = ctx.cancelled() => return Ok(()),
                item = lines.recv() => item,
            };
            let user_input = match item {
                None => {
                    self.print("\nEOF detected, exiting...\n");
                    return Ok(());
                }
                // Go `log.Fatal(err)`: the error's text, returned instead of exiting the
                // process (the caller's `main` prints it, as Go's examples print `Run failed`).
                Some(Err(e)) => return Err(Error::Serve(e.to_string())),
                Some(Ok(line)) => line,
            };
            // Drop the line terminator, so the message matches what the web UI submits.
            let user_input = user_input.trim_end_matches(['\r', '\n']);

            let user_msg = if pending_interrupts.is_empty() {
                Content::from_text(user_input, ROLE_USER)
            } else {
                // Answer the head of the queue; loop back while more prompts remain.
                let current = pending_interrupts.remove(0);
                pending_responses.push(build_interrupt_response(&current, user_input));
                if let Some(next) = pending_interrupts.first() {
                    let mut out = self.out();
                    render_interrupt_prompt(&mut *out, next);
                    let _ = out.flush();
                    continue;
                }
                // Every answer is in: one user content carrying every function response.
                Content::from_parts(std::mem::take(&mut pending_responses), ROLE_USER)
            };

            let streaming_mode = self.streaming_mode.unwrap_or(default_streaming_mode);

            self.print("\nAgent -> ");
            let mut prev_text = String::new();
            let mut printed_content = false;
            let mut final_output: Option<Value> = None;
            let mut collected_events: Vec<Event> = Vec::new();
            let mut events = runner.run(
                USER_ID,
                &session_id,
                Some(user_msg),
                RunConfig {
                    streaming_mode,
                    ..Default::default()
                },
                RunOptions::default(),
            );
            loop {
                let item = tokio::select! {
                    _ = ctx.cancelled() => return Ok(()),
                    item = events.next() => item,
                };
                let Some(item) = item else {
                    break;
                };
                let event = match item {
                    Err(e) if e.is_compaction() => {
                        // A compaction failure is not this turn's failure: it runs after the
                        // answer is stored, so all it costs is a larger prompt next time.
                        config
                            .log
                            .print(&format!("adk: context compaction failed: {e}"));
                        continue;
                    }
                    Err(e) => {
                        self.print(&format!("\nAGENT_ERROR: {e}\n"));
                        continue;
                    }
                    Ok(event) => event,
                };
                let Some(content) = &event.response.content else {
                    // Function/terminal nodes carry their result in `output`, not model content;
                    // keep the latest so a content-less turn still surfaces a result.
                    if event.output.is_some() {
                        final_output = event.output.clone();
                    }
                    collected_events.push(event);
                    continue;
                };
                let text: String = content.parts.iter().map(|p| p.text.as_str()).collect();
                if !text.is_empty() {
                    printed_content = true;
                }
                if streaming_mode != StreamingMode::Sse {
                    self.print(&text);
                } else if !event.is_final_response() {
                    // SSE: print partial responses as they come and remember them.
                    self.print(&text);
                    prev_text.push_str(&text);
                } else {
                    // Print the final response only when it differs from the partials.
                    if text != prev_text {
                        self.print(&text);
                    }
                    prev_text.clear();
                }
                collected_events.push(event);
            }

            // A turn paused on long-running interrupts: prompt for the first one; the next line
            // is its answer.
            pending_interrupts = collect_pending_interrupts(&collected_events);
            if let Some(first) = pending_interrupts.first() {
                let mut out = self.out();
                let _ = out.write_all(b"\n");
                render_interrupt_prompt(&mut *out, first);
                let _ = out.flush();
                continue;
            }
            // A workflow whose terminal node returns a value rather than model content streams no
            // text; surface that result so the turn is not silent.
            if !printed_content && let Some(output) = &final_output {
                self.print(&render_output(output));
            }
            self.print("\nUser -> ");
        }
    }

    /// Go `fmt.Print(s)` to the console's stdout (errors ignored, as Go's are).
    fn print(&self, s: &str) {
        let mut out = self.out();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }

    fn out(&self) -> MutexGuard<'_, dyn Write + Send + 'static> {
        self.output.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait::async_trait]
impl SubLauncher for ConsoleLauncher {
    fn keyword(&self) -> String {
        "console".to_owned()
    }

    /// Go `Parse`: the console flags, then the remaining arguments.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let remaining = {
            let fs = self.flags.get_mut().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = fs.parse(&args) {
                return Err(Error::Parse(format!("failed to parse flags: {e}")));
            }
            fs.args()
        };
        let mode = self.streaming_mode_string.get();
        self.streaming_mode = match mode.as_str() {
            "" => None,
            "none" => Some(StreamingMode::None),
            "sse" => Some(StreamingMode::Sse),
            _ => {
                return Err(Error::Parse(format!(
                    "invalid streaming_mode: {mode}. Should be (none|sse)"
                )));
            }
        };
        Ok(remaining)
    }

    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    fn simple_description(&self) -> String {
        "runs an agent in console mode.".to_owned()
    }

    /// Go `Run` over the process's telemetry globals (see [`ConsoleLauncher::run_in`]).
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        self.run_in(cancel, config, telemetry::Globals::process())
            .await
    }
}

#[async_trait::async_trait]
impl Launcher for ConsoleLauncher {
    /// Go `consoleLauncher.Execute`: validate, parse, refuse leftovers, run.
    async fn execute(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
    ) -> Result<(), Error> {
        self.execute_in(cancel, config, args, telemetry::Globals::process())
            .await
    }

    fn command_line_syntax(&self) -> String {
        SubLauncher::command_line_syntax(self)
    }
}

/// Go's `bufio.Reader.ReadString('\n')` loop as [`InputLines`]: each item is a line with its
/// `\n`; a last line without one is dropped, as Go's console treats the `io.EOF` that comes with
/// it as the end of input; a read error ends the lines after it is yielded.
pub fn input_lines(reader: impl BufRead + Send + 'static) -> InputLines {
    let mut reader = reader;
    let mut done = false;
    Box::new(std::iter::from_fn(move || {
        if done {
            return None;
        }
        let mut buf = Vec::new();
        match reader.read_until(b'\n', &mut buf) {
            Ok(_) if buf.ends_with(b"\n") => Some(Ok(String::from_utf8_lossy(&buf).into_owned())),
            Ok(_) => {
                done = true;
                None
            }
            Err(e) => {
                done = true;
                Some(Err(e))
            }
        }
    }))
}

/// Go's reader goroutine: moves `input` onto a thread feeding a one-slot channel (Go: an
/// unbuffered channel and a buffered error channel); the channel closes at end of input.
fn spawn_reader(
    input: InputLines,
) -> Result<tokio::sync::mpsc::Receiver<std::io::Result<String>>, Error> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    std::thread::Builder::new()
        .name("gadk-console-stdin".to_owned())
        .spawn(move || {
            for item in input {
                let stop = item.is_err();
                if tx.blocking_send(item).is_err() || stop {
                    return;
                }
            }
        })
        .map_err(|e| wrapped("failed to start reading stdin", e))?;
    Ok(rx)
}

/// Go's `os.Stdout.Stat()` and `ModeCharDevice` check (on Unix the file type; elsewhere whether
/// stdout is a terminal).
fn stdout_is_char_device() -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        use std::os::unix::fs::FileTypeExt;
        std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map(std::fs::File::from)
            .and_then(|f| f.metadata())
            .is_ok_and(|m| m.file_type().is_char_device())
    }
    #[cfg(not(unix))]
    {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    }
}

/// Go `fmt.Errorf("<context>: %v", err)`.
fn wrapped(context: &str, e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Wrapped {
        context: context.to_owned(),
        source: Box::new(e),
    }
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn take_or(slot: &Mutex<Option<InputLines>>, default: impl FnOnce() -> InputLines) -> InputLines {
    lock(slot).take().unwrap_or_else(default)
}

/// The console against the Go probe's console fixtures (`testdata/console`, format in
/// `scripts/gen-go-launcher/SCENARIO.md`): the probe's agent and scripted model
/// (`test_support::probe_agent`) give the same stdout bytes and no log output.
#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gadk_agent::SingleLoader;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::golog;
    use crate::test_support as ts;

    /// Runs one fixture through `ConsoleLauncher::execute` (the `console` keyword dropped, as the
    /// universal launcher does) with the probe's agent, and returns (stdout, log output).
    async fn replay(fixture: &Value) -> (String, String) {
        let args = ts::args(fixture, 0);
        let args = match args.first().map(String::as_str) {
            Some("console") => args[1..].to_vec(),
            _ => args,
        };
        let stdin = fixture["stdin"].as_str().unwrap().to_owned();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (log, logged) = golog::capture();
        let mut config = Config::new(Arc::new(SingleLoader::new(ts::probe_agent())));
        config.log = log;
        let mut l = new().with_io(
            input_lines(Cursor::new(stdin.into_bytes())),
            buf.clone(),
            false,
        );
        l.execute_with(CancellationToken::new(), config, args, |_| true)
            .await
            .unwrap();
        let stdout = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        (stdout, logged.contents())
    }

    #[tokio::test]
    async fn console_fixtures_replay_byte_for_byte() {
        let names = ts::fixture_names("console");
        assert_eq!(names.len(), 15);
        for name in names {
            let fixture = ts::fixture("console", &name);
            let (stdout, stderr) = replay(&fixture).await;
            let name = fixture["name"].as_str().unwrap();
            assert_eq!(
                stdout,
                fixture["stdout"].as_str().unwrap(),
                "{name}: stdout"
            );
            assert_eq!(
                stderr,
                fixture["stderr"].as_str().unwrap(),
                "{name}: stderr"
            );
        }
    }

    #[test]
    fn input_lines_keep_terminators_and_drop_a_last_partial_line() {
        let lines: Vec<String> = input_lines(Cursor::new(b"a\r\nb\nc".to_vec()))
            .map(Result::unwrap)
            .collect();
        assert_eq!(lines, vec!["a\r\n".to_owned(), "b\n".to_owned()]);
        assert_eq!(input_lines(Cursor::new(Vec::new())).count(), 0);
    }

    #[test]
    fn parse_validates_streaming_mode() {
        let mut l = new();
        let err = l
            .parse(vec!["-streaming_mode".into(), "bidi".into()])
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid streaming_mode: bidi. Should be (none|sse)"
        );

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut l = new().with_flag_output(out.clone());
        let err = l.parse(vec!["-nope".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to parse flags: flag provided but not defined: -nope"
        );
        assert!(
            String::from_utf8(out.lock().unwrap().clone())
                .unwrap()
                .starts_with("flag provided but not defined: -nope\nUsage of console:\n")
        );

        let mut l = new();
        assert_eq!(
            l.parse(vec!["-streaming_mode".into(), "sse".into(), "rest".into()])
                .unwrap(),
            vec!["rest".to_owned()]
        );
        assert_eq!(l.streaming_mode, Some(StreamingMode::Sse));
    }

    #[tokio::test]
    async fn execute_refuses_unparsed_arguments() {
        let mut l = new().with_io(
            input_lines(Cursor::new(Vec::new())),
            Arc::new(Mutex::new(Vec::<u8>::new())),
            false,
        );
        let agent = ts::probe_agent();
        let err = l
            .execute_with(
                CancellationToken::new(),
                Config::new(Arc::new(SingleLoader::new(agent))),
                vec!["extra".into()],
                |_| true,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot parse all the arguments: cannot parse following arguments: [extra]"
        );
    }

    /// Go `log.Fatal(err)` on a stdin read error other than EOF: here `run` returns the error's
    /// text (nothing logged, the process is not exited).
    #[tokio::test]
    async fn a_stdin_read_error_is_returned_with_gos_text() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let input: InputLines = Box::new(
            vec![Err(std::io::Error::other(
                "read /dev/stdin: input/output error",
            ))]
            .into_iter(),
        );
        let mut l = new().with_io(input, buf.clone(), false);
        let agent = ts::probe_agent();
        let (log, logged) = golog::capture();
        let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
        config.log = log;
        let err = l
            .execute_with(CancellationToken::new(), config, Vec::new(), |_| true)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Serve(_)), "{err:?}");
        assert_eq!(err.to_string(), "read /dev/stdin: input/output error");
        assert_eq!(logged.contents(), "");
        assert_eq!(
            String::from_utf8(buf.lock().unwrap().clone()).unwrap(),
            "\n\nUser -> "
        );
    }

    /// The probe's model, except that `transfer` asks for two `transfer_funds` calls at once (a
    /// turn paused on two confirmations).
    struct TwoTransfers;

    impl gadk_model::Llm for TwoTransfers {
        fn name(&self) -> &str {
            "two-transfers"
        }

        fn generate_content(
            &self,
            req: gadk_model::LlmRequest,
            _stream: bool,
        ) -> futures::stream::BoxStream<
            'static,
            Result<gadk_model::LlmResponse, gadk_model::ModelError>,
        > {
            let answered = req
                .contents
                .last()
                .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()));
            let parts = if answered {
                vec![gadk_genai::Part::from_text("Transfer done.")]
            } else {
                (1..=2)
                    .map(|n| gadk_genai::Part {
                        function_call: Some(gadk_genai::FunctionCall {
                            id: format!("call-{n}"),
                            name: "transfer_funds".into(),
                            args: serde_json::json!({"amount": n})
                                .as_object()
                                .cloned()
                                .unwrap(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect()
            };
            let resp = gadk_model::LlmResponse {
                content: Some(gadk_genai::Content::from_parts(
                    parts,
                    gadk_genai::ROLE_MODEL,
                )),
                finish_reason: Some(gadk_genai::FinishReason::Stop),
                ..Default::default()
            };
            Box::pin(futures::stream::iter([Ok(resp)]))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Records the buffer length at every flush.
    #[derive(Default)]
    struct FlushRecorder {
        buf: Vec<u8>,
        flushed_at: Vec<usize>,
    }

    impl Write for FlushRecorder {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushed_at.push(self.buf.len());
            Ok(())
        }
    }

    /// Go's stdout is unbuffered: every `User -> ` prompt, the second confirmation of a turn
    /// paused on two included, reaches the terminal before the next line is read.
    #[tokio::test]
    async fn every_prompt_is_flushed_before_reading_its_answer() {
        let rec = Arc::new(Mutex::new(FlushRecorder::default()));
        let agent = ts::probe_agent_with(Arc::new(TwoTransfers));
        let mut l = new().with_io(
            input_lines(Cursor::new(b"transfer\nyes\nyes\n".to_vec())),
            rec.clone(),
            false,
        );
        l.execute_with(
            CancellationToken::new(),
            Config::new(Arc::new(SingleLoader::new(agent))),
            Vec::new(),
            |_| true,
        )
        .await
        .unwrap();
        let rec = rec.lock().unwrap();
        let out = String::from_utf8(rec.buf.clone()).unwrap();
        assert_eq!(
            out.matches("Agent -> Confirm transfer_funds?").count(),
            2,
            "{out}"
        );
        let prompts: Vec<usize> = out
            .match_indices("User -> ")
            .map(|(i, p)| i + p.len())
            .collect();
        assert_eq!(prompts.len(), 4, "{out}");
        for end in prompts {
            assert!(
                rec.flushed_at.contains(&end),
                "no flush after the prompt ending at {end}:\n{out}"
            );
        }
    }

    #[test]
    fn describes_itself_as_go_does() {
        let l = new();
        assert_eq!(l.keyword(), "console");
        assert_eq!(l.simple_description(), "runs an agent in console mode.");
    }

    /// `console -help` matches the probe's fixture (`testdata/cli/help_console.json`): the flag
    /// set's usage output, the returned error, and the syntax the universal launcher prints under
    /// `Details:`.
    #[test]
    fn help_matches_the_cli_fixture() {
        let fixture = ts::cli("help_console");
        let stderr = fixture["stderr"].as_str().unwrap();

        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut l = new().with_flag_output(out.clone());
        let err = l.parse(vec!["-help".into()]).unwrap_err();
        assert_eq!(err.to_string(), fixture["error"].as_str().unwrap());
        let usage = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert!(
            stderr.starts_with(&format!("{usage}<ts> Run failed: ")),
            "{stderr}"
        );
        let syntax = SubLauncher::command_line_syntax(&l);
        assert_eq!(usage, format!("Usage of console:\n{syntax}"));
        assert!(
            stderr.contains(&format!("Details:\n  console\n{syntax}\n")),
            "{stderr}"
        );
    }
}
