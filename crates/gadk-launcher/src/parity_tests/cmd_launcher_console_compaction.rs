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

//! Port of ADK-Go `cmd/launcher/console/compaction_test.go`.
//!
//! Go swaps `os.Stdin`/`os.Stdout` for pipes; the Rust console takes its input and output through
//! `ConsoleLauncher::with_io`, and Go's `log` output through `Config::log`.

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_agent::{AgentError, BeforeAgentCallback, SingleLoader};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::Content;
use gadk_session::compaction::{Config as CompactionConfig, SummarizeResult, Summarizer};
use gadk_session::{Event, InMemorySessionService};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::console::{self, input_lines};
use crate::golog::{self, Captured};
use crate::launcher::SubLauncher;

/// Go `countingSummarizer`: records that compaction reached the summarizer, and can be made to
/// fail.
struct CountingSummarizer {
    calls: AtomicUsize,
    err: Option<&'static str>,
}

#[async_trait::async_trait]
impl Summarizer for CountingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.err {
            return Err(e.into());
        }
        Ok(SummarizeResult {
            content: Some(Content::from_text("a summary", "model")),
            usage: None,
        })
    }
}

impl CountingSummarizer {
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// Go `runConsole`: drives the console launcher over a scripted stdin and returns what it
/// printed. The end of the input ends the loop the way EOF does in a terminal.
async fn run_console(config: Config, input: &str) -> String {
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut l = console::new().with_io(
        input_lines(Cursor::new(input.as_bytes().to_vec())),
        out.clone(),
        false,
    );
    l.parse(Vec::new()).expect("Parse()");
    let run = l.run(CancellationToken::new(), config).await;
    let printed = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    if let Err(e) = run {
        panic!("Run() error = {e}. Output:\n{printed}");
    }
    printed
}

/// Go `testConfig`: an `echo` agent whose before-agent callback answers with the user's message,
/// over an in-memory session service, with `cfg` as the compaction config. Also returns the
/// captured `log` output.
fn test_config(cfg: CompactionConfig) -> (Config, Captured) {
    let echo: BeforeAgentCallback = Arc::new(|ctx| {
        let content = ctx.user_content().cloned();
        Box::pin(async move { Ok::<_, AgentError>(content) })
    });
    let agent = gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "echo".into(),
        before_agent_callbacks: vec![echo],
        ..Default::default()
    })
    .expect("llmagent.New()");
    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.session_service = Some(Arc::new(InMemorySessionService::new()));
    config.compaction = Some(cfg);
    let (log, captured) = golog::capture();
    config.log = log;
    (config, captured)
}

fn compaction(summarizer: &Arc<CountingSummarizer>) -> CompactionConfig {
    CompactionConfig {
        compaction_interval: 1,
        summarizer: Some(summarizer.clone()),
        ..Default::default()
    }
}

/// Go: the compaction config this surface accepts reaches the runner it builds.
#[tokio::test]
async fn test_console_actually_runs_compaction() {
    let summarizer = Arc::new(CountingSummarizer {
        calls: AtomicUsize::new(0),
        err: None,
    });
    let (config, _log) = test_config(compaction(&summarizer));

    run_console(config, "hello\n").await;

    assert!(
        summarizer.count() > 0,
        "the summarizer was never called, so the console accepts a compaction config and does nothing with it"
    );
}

/// Go: a compaction failure does not surface as a failed turn.
// parity: adapted: also checks the failure is logged as Go's `adk: context compaction failed: …`
#[tokio::test]
async fn test_console_survives_a_compaction_failure() {
    let summarizer = Arc::new(CountingSummarizer {
        calls: AtomicUsize::new(0),
        err: Some("summarizer unavailable"),
    });
    let (config, log) = test_config(compaction(&summarizer));

    let out = run_console(config, "hello\n").await;

    assert!(
        summarizer.count() > 0,
        "the summarizer was never called, so this test is not exercising the failure path"
    );
    assert!(
        !out.contains("AGENT_ERROR"),
        "a compaction failure was reported as a failed turn:\n{out}"
    );
    let logged = log.contents();
    assert!(
        logged.contains("adk: context compaction failed: ")
            && logged.contains("summarizer unavailable"),
        "{logged}"
    );
}
