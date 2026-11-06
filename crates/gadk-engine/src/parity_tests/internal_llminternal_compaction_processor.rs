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

//! Port of ADK-Go `internal/llminternal/compaction_processor_test.go` (`package llminternal_test`,
//! mapped external→internal), including the package-shared fixture `tailRetentionFixture`,
//! `runCompactionProcessor` and `storedCompactions`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use gadk_agent::AgentError;
use gadk_genai::{Content, GenerateContentResponseUsageMetadata, ROLE_MODEL, ROLE_USER};
use gadk_model::LlmRequest;
use gadk_platform::Platform;
use gadk_session::compaction::{Config, SummarizeResult, Summarizer};
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest, Session,
    SessionError, SessionService,
};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::internal_llminternal_contents_processor::llm_agent;
use crate::compaction::{apply, runtime};
use crate::llm::LlmAgentConfig;
use crate::llm::agent::WrappedSession;
use crate::llm::flow::Flow;
use crate::llm::processors::compaction::compaction_request_processor;

fn get_req() -> GetRequest {
    GetRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: "s".into(),
        ..Default::default()
    }
}

/// Go fixedSummarizer: one canned summary.
#[allow(non_camel_case_types)]
#[derive(Default)]
pub(super) struct fixedSummarizer {
    calls: Mutex<usize>,
}

#[async_trait]
impl Summarizer for fixedSummarizer {
    async fn summarize_events(
        &self,
        _events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        *self.calls.lock() += 1;
        Ok(SummarizeResult {
            content: Some(Content::from_text("SUMMARY", ROLE_MODEL)),
            usage: None,
        })
    }
}

/// Go tailRetentionFixture: a stored session app/u/s holding `n` exchanges at 1 s spacing from
/// Unix 1 s; each answer reports a prompt token count of 5000.
pub(super) async fn tail_retention_fixture(n: i64) -> (Arc<dyn SessionService>, Arc<dyn Session>) {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let sess = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            state: None,
        })
        .await
        .expect("Create() error");
    let platform = Platform::default();
    let base: DateTime<Utc> = Utc.timestamp_opt(1, 0).unwrap();
    for i in 0..n {
        let mut q = Event::new(&platform, "inv");
        q.author = "user".into();
        q.timestamp = base + chrono::Duration::seconds(2 * i);
        q.response.content = Some(Content::from_text("question", ROLE_USER));
        svc.append_event(&*sess, &mut q)
            .await
            .expect("AppendEvent() error");

        let mut a = Event::new(&platform, "inv");
        a.author = "assistant".into();
        a.timestamp = base + chrono::Duration::seconds(2 * i + 1);
        a.response.content = Some(Content::from_text("answer", ROLE_MODEL));
        a.response.usage_metadata = Some(GenerateContentResponseUsageMetadata {
            prompt_token_count: 5000,
            ..Default::default()
        });
        svc.append_event(&*sess, &mut a)
            .await
            .expect("AppendEvent() error");
    }
    (svc, sess)
}

/// Go runCompactionProcessor: runs the processor for agent "assistant" over `sess` with a runtime
/// built from `cfg` and `svc`; fails if it yields an event; returns its error, if any.
pub(super) async fn run_compaction_processor(
    svc: Arc<dyn SessionService>,
    sess: Arc<dyn Session>,
    cfg: Config,
    token: Option<CancellationToken>,
) -> Result<(), AgentError> {
    let agent = llm_agent(LlmAgentConfig {
        name: "assistant".into(),
        ..Default::default()
    });
    let mut b = gadk_agent::InvocationContext::builder()
        .agent(agent)
        .session(sess);
    if let Some(t) = token {
        b = b.cancellation_token(t);
    }
    let ictx = runtime::attach(b, runtime::CompactionRuntime::new(Arc::new(cfg), Some(svc)))
        .build_invocation();
    let events =
        compaction_request_processor(&ictx, &mut LlmRequest::default(), &Flow::default()).await?;
    assert!(
        events.is_empty(),
        "CompactionRequestProcessor yielded an event, which it must not do"
    );
    Ok(())
}

/// Go storedCompactions: the stored events carrying a usable summary.
pub(super) async fn stored_compactions(svc: &dyn SessionService) -> Vec<Event> {
    let s = svc.get(get_req()).await.expect("Get() error");
    s.events()
        .into_iter()
        .filter(apply::has_usable_summary)
        .collect()
}

fn tail_cfg(summarizer: Arc<dyn Summarizer>) -> Config {
    Config {
        token_threshold: 100,
        event_retention_size: 2,
        summarizer: Some(summarizer),
        ..Default::default()
    }
}

// parity: adapted: Go's seedWrappedSession is a pass-through Unwrap() decorator; Rust's only unwrappable wrapper is WrappedSession, which also shows one seed user turn
#[tokio::test]
async fn test_compaction_processor_appends_through_a_session_wrapper() {
    let (svc, sess) = tail_retention_fixture(4).await;
    let summarizer = Arc::new(fixedSummarizer::default());
    let mut seed = Event::new(&Platform::default(), "seed-inv");
    seed.author = "user".into();
    seed.timestamp = Utc.timestamp_opt(100, 0).unwrap();
    seed.response.content = Some(Content::from_text("seed", ROLE_USER));
    let wrapped: Arc<dyn Session> = Arc::new(WrappedSession::new(sess, seed));

    run_compaction_processor(svc.clone(), wrapped, tail_cfg(summarizer.clone()), None)
        .await
        .expect("compaction through a wrapped session failed");
    assert!(
        *summarizer.calls.lock() > 0,
        "the summarizer never ran, so this test proved nothing"
    );
    let got = stored_compactions(&*svc).await.len();
    assert_eq!(
        got, 1,
        "stored {got} compaction events, want 1: the summary never reached the session"
    );
}

#[tokio::test]
async fn test_compaction_processor_skips_on_cancelled_context() {
    let (svc, sess) = tail_retention_fixture(4).await;
    let summarizer = Arc::new(fixedSummarizer::default());
    let token = CancellationToken::new();
    token.cancel();
    run_compaction_processor(svc.clone(), sess, tail_cfg(summarizer.clone()), Some(token))
        .await
        .expect("the processor never fails");
    let calls = *summarizer.calls.lock();
    assert_eq!(
        calls, 0,
        "summarizer ran {calls} time(s) on a canceled turn"
    );
    let got = stored_compactions(&*svc).await.len();
    assert_eq!(got, 0, "stored {got} compaction events on a canceled turn");
}

/// Go racingSummarizer: appends an event inside the range it is about to summarize, through a
/// separate handle on the same stored session.
#[allow(non_camel_case_types)]
struct racingSummarizer {
    svc: Arc<dyn SessionService>,
    raced: Mutex<Option<(String, DateTime<Utc>)>>,
}

#[async_trait]
impl Summarizer for racingSummarizer {
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        let other = self.svc.get(get_req()).await.expect("racing Get() error");
        let mut late = Event::new(&Platform::default(), "other-invocation");
        late.author = "user".into();
        late.timestamp = events[0].timestamp + chrono::Duration::milliseconds(1);
        late.response.content = Some(Content::from_text("CONCURRENT", ROLE_USER));
        self.svc
            .append_event(&*other, &mut late)
            .await
            .expect("racing AppendEvent() error");
        *self.raced.lock() = Some((late.invocation_id.clone(), late.timestamp));
        Ok(SummarizeResult {
            content: Some(Content::from_text("SUMMARY", ROLE_MODEL)),
            usage: None,
        })
    }
}

#[tokio::test]
async fn test_compaction_processor_discards_a_raced_summary() {
    let (svc, sess) = tail_retention_fixture(4).await;
    let summarizer = Arc::new(racingSummarizer {
        svc: svc.clone(),
        raced: Mutex::new(None),
    });
    run_compaction_processor(svc.clone(), sess, tail_cfg(summarizer.clone()), None)
        .await
        .expect("CompactionRequestProcessor failed");
    assert!(
        summarizer.raced.lock().is_some(),
        "the racing summarizer did not record what it appended"
    );
    let got = stored_compactions(&*svc).await.len();
    assert_eq!(
        got, 0,
        "stored {got} compaction events, want 0: a summary whose range was raced must be discarded"
    );
}

/// Go straggleOnAppend: lands an event inside the summary's range at the moment the summary itself
/// is stored, through a separate handle.
struct StraggleOnAppend {
    inner: Arc<dyn SessionService>,
    state: Mutex<(bool, Option<String>)>, // (done, straggler invocation id)
}

#[async_trait]
impl SessionService for StraggleOnAppend {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        self.inner.delete(req).await
    }
    async fn append_event(
        &self,
        session: &dyn Session,
        ev: &mut Event,
    ) -> Result<(), SessionError> {
        let fire = {
            let mut st = self.state.lock();
            let fire = !st.0 && ev.actions.compaction.is_some();
            if fire {
                st.0 = true;
            }
            fire
        };
        if fire {
            let start = ev
                .actions
                .compaction
                .as_ref()
                .map(|c| c.start_timestamp)
                .unwrap_or_default();
            let other = self
                .inner
                .get(get_req())
                .await
                .expect("straggler Get() error");
            let mut late = Event::new(&Platform::default(), "straggler-invocation");
            late.author = "user".into();
            late.timestamp = start + chrono::Duration::milliseconds(1);
            late.response.content = Some(Content::from_text("PLEASE DO NOT LOSE ME", ROLE_USER));
            self.inner
                .append_event(&*other, &mut late)
                .await
                .expect("straggler AppendEvent() error");
            self.state.lock().1 = Some(late.invocation_id.clone());
        }
        self.inner.append_event(session, ev).await
    }
}

#[tokio::test]
async fn test_compaction_processor_repairs_a_straggler_stored_during_the_append() {
    let (base, sess) = tail_retention_fixture(6).await;
    let svc = Arc::new(StraggleOnAppend {
        inner: base,
        state: Mutex::new((false, None)),
    });
    run_compaction_processor(
        svc.clone(),
        sess,
        tail_cfg(Arc::new(fixedSummarizer::default())),
        None,
    )
    .await
    .expect("CompactionRequestProcessor failed");
    let straggler = svc
        .state
        .lock()
        .1
        .clone()
        .expect("no straggler was landed, so this test is not exercising the window");

    let stored = svc.get(get_req()).await.expect("Get() error").events();
    // The straggler must survive into the assembled prompt: no summary describes it.
    assert!(
        apply::apply(&stored)
            .iter()
            .any(|ev| ev.invocation_id == straggler),
        "the event stored during the append was dropped from the prompt and no summary describes it"
    );
}
