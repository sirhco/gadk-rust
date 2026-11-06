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

//! Port of ADK-Go `internal/llminternal/compaction_concurrent_test.go` (`package llminternal_test`,
//! mapped external→internal). Go runs these under `-race`; Rust runs them on a multi-thread runtime.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::compaction::{Config, SummarizeResult, Summarizer};
use gadk_session::{Event, GetRequest};
use parking_lot::Mutex;
use tokio::sync::{Barrier, Notify};

use super::internal_llminternal_compaction_processor::{
    run_compaction_processor, tail_retention_fixture,
};
use crate::compaction::apply;

/// Go recordingSummarizer: remembers every invocation it was handed.
#[allow(non_camel_case_types)]
#[derive(Default)]
struct recordingSummarizer {
    seen: Mutex<HashSet<String>>,
}

impl recordingSummarizer {
    fn summarized(&self, invocation_id: &str) -> bool {
        self.seen.lock().contains(invocation_id)
    }
}

#[async_trait]
impl Summarizer for recordingSummarizer {
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        let mut seen = self.seen.lock();
        for ev in events {
            seen.insert(ev.invocation_id.clone());
        }
        Ok(SummarizeResult {
            content: Some(Content::from_text("summary", ROLE_MODEL)),
            usage: None,
        })
    }
}

/// Go blockingSummarizer: signals `entered`, then waits for `release`. (Go also returns on
/// `ctx.Done()`; a Rust summarizer is cancelled by dropping its future, so that arm has no port.)
#[allow(non_camel_case_types)]
#[derive(Default)]
struct blockingSummarizer {
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl Summarizer for blockingSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(SummarizeResult {
            content: Some(Content::from_text("summary", ROLE_MODEL)),
            usage: None,
        })
    }
}

fn tail_cfg(summarizer: Arc<dyn Summarizer>) -> Config {
    Config {
        token_threshold: 100,
        event_retention_size: 2,
        summarizer: Some(summarizer),
        ..Default::default()
    }
}

fn get_req() -> GetRequest {
    GetRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: "s".into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_compaction_survives_a_concurrent_writer() {
    let (base, sess) = tail_retention_fixture(6).await;
    let summarizer = Arc::new(blockingSummarizer::default());

    // Created with a timestamp inside the range the compaction is about to record, and stored
    // after the window was chosen.
    let mut straggler = Event::new(&Platform::default(), "concurrent-inv");
    straggler.author = "user".into();
    straggler.timestamp = Utc.timestamp_opt(4, 0).unwrap();
    straggler.response.content = Some(Content::from_text("from the other writer", ROLE_USER));
    let straggler_inv = straggler.invocation_id.clone();

    let writer = {
        let (base, sess, summarizer) = (base.clone(), sess.clone(), summarizer.clone());
        tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                summarizer.entered.notified(),
            )
            .await
            .expect("the summarizer was never entered within 5 s: compaction did not run");
            base.append_event(&*sess, &mut straggler)
                .await
                .expect("the concurrent writer could not append");
            summarizer.release.notify_one();
        })
    };

    let res = run_compaction_processor(base.clone(), sess, tail_cfg(summarizer), None).await;
    writer.await.expect("writer task panicked");
    res.expect("CompactionRequestProcessor failed");

    let stored = base.get(get_req()).await.expect("Get() error").events();
    // Deliberately not asserting that a summary landed (either defense may fire).
    let summaries = stored
        .iter()
        .filter(|ev| apply::has_usable_summary(ev))
        .count();
    // The invariant: every event is summarized, named as a hole, or still present raw.
    assert!(
        apply::apply(&stored)
            .iter()
            .any(|ev| ev.invocation_id == straggler_inv),
        "an event appended by a concurrent writer during compaction reached no prompt and no summary: it was lost.\nstored events: {}, summaries kept: {summaries}",
        stored.len()
    );
}

/// Go `attempts`: how many interleavings one run samples.
const ATTEMPTS: usize = 200;

/// Go concurrentAppendAttempt: one interleaving; the invocation ID of a lost event, if any.
async fn concurrent_append_attempt() -> Option<String> {
    const WRITERS: usize = 4;
    let (base, sess) = tail_retention_fixture(6).await;
    let summarizer = Arc::new(recordingSummarizer::default());
    let platform = Platform::default();
    let appended: Vec<Event> = (0..WRITERS)
        .map(|i| {
            let mut ev = Event::new(&platform, format!("writer-{i}"));
            ev.author = "user".into();
            ev.timestamp = Utc.timestamp_opt(3 + i as i64, 0).unwrap();
            ev.response.content = Some(Content::from_text("concurrent", ROLE_USER));
            ev
        })
        .collect();

    let start = Arc::new(Barrier::new(WRITERS + 1));
    let mut tasks = Vec::new();
    for ev in appended.iter().cloned() {
        let (base, sess, start) = (base.clone(), sess.clone(), start.clone());
        tasks.push(tokio::spawn(async move {
            let mut ev = ev;
            start.wait().await;
            base.append_event(&*sess, &mut ev)
                .await
                .expect("concurrent append failed");
        }));
    }
    {
        let (base, sess, start, s) = (
            base.clone(),
            sess.clone(),
            start.clone(),
            summarizer.clone(),
        );
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            // A failure is a legitimate outcome; a lost event is not (checked below).
            let _ = run_compaction_processor(base, sess, tail_cfg(s), None).await;
        }));
    }
    for t in tasks {
        t.await.expect("task panicked");
    }

    let stored = base.get(get_req()).await.expect("Get() error").events();
    let in_prompt: HashSet<String> = apply::apply(&stored)
        .into_iter()
        .map(|ev| ev.invocation_id)
        .collect();
    appended
        .iter()
        .map(|ev| ev.invocation_id.clone())
        .find(|id| !in_prompt.contains(id) && !summarizer.summarized(id))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_compaction_under_concurrent_appends_loses_nothing() {
    for attempt in 0..ATTEMPTS {
        if let Some(lost) = concurrent_append_attempt().await {
            panic!(
                "attempt {attempt}: event from {lost} reached no prompt and was never handed to a summarizer: it was lost"
            );
        }
    }
}
