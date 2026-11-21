// Copyright 2026 Chris Olson
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

//! Test helpers for the web layer: a scripted trace source, a fixed state, and in-process
//! requests through the full router (guards and CSP included).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use chrono::{DateTime, TimeZone, Utc};
use gadk_eval::{Outcome, RunTranscript, ScoreResult, TurnRecord};
use gadk_eval_monitor::{
    ExportContext, ExportSource, MonitorError, MonitorReport, MonitoredRun, ProductionRun,
    ReportStore, ScoreFilter, TraceInvocation, TranscriptQuery, Window,
};
use gadk_prompts::PromptStore;
use http::header::{CONTENT_TYPE, HOST, ORIGIN};
use http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;

use super::{AppState, router};
use crate::evals::ReportCache;
use crate::traces::{SourceError, TraceDetail, TraceId, TracePage, TraceQuery, TraceSource};

/// The port every test state claims to be bound to.
pub(crate) const PORT: u16 = 8765;
/// The CSRF token every test state carries (64 × 'a').
pub(crate) const CSRF: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The score author every test state carries.
pub(crate) const AUTHOR: &str = "tester";

/// 2026-11-16T12:00:00Z.
pub(crate) fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
}

/// Scripted TraceSource: returns clones of the stored results and counts calls.
pub(crate) struct FakeSource {
    pub list: Mutex<Result<TracePage, SourceError>>,
    pub get: Mutex<Result<TraceDetail, SourceError>>,
    pub calls: AtomicUsize,
}

impl FakeSource {
    /// `list` answers an empty page; `get` answers `NotFound`.
    pub(crate) fn new() -> Self {
        Self {
            list: Mutex::new(Ok(TracePage::default())),
            get: Mutex::new(Err(SourceError::NotFound)),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl TraceSource for FakeSource {
    async fn list(&self, _query: &TraceQuery) -> Result<TracePage, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.list.lock().unwrap().clone()
    }

    async fn get(&self, _id: &TraceId) -> Result<TraceDetail, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.get.lock().unwrap().clone()
    }
}

/// Scripted ExportSource: the trace's invocations, the runs among `runs` that are asked for,
/// no history and the given contexts (or the error `fail`); counts calls.
#[derive(Default)]
pub(crate) struct FakeExport {
    pub invocations: Vec<TraceInvocation>,
    pub runs: Vec<ProductionRun>,
    pub contexts: HashMap<String, ExportContext>,
    pub fail: Option<String>,
    pub calls: AtomicUsize,
}

impl FakeExport {
    fn call(&self) -> Result<(), MonitorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.fail {
            Some(e) => Err(MonitorError::Source(e.clone())),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl ExportSource for FakeExport {
    async fn trace_invocations(
        &self,
        _traces: &[String],
    ) -> Result<Vec<TraceInvocation>, MonitorError> {
        self.call()?;
        Ok(self.invocations.clone())
    }

    async fn runs(
        &self,
        ids: &[String],
        _from: DateTime<Utc>,
    ) -> Result<Vec<ProductionRun>, MonitorError> {
        self.call()?;
        Ok(self
            .runs
            .iter()
            .filter(|r| ids.contains(&r.invocation_id))
            .cloned()
            .collect())
    }

    async fn histories(
        &self,
        _runs: &[ProductionRun],
    ) -> Result<HashMap<String, Vec<ProductionRun>>, MonitorError> {
        self.call()?;
        Ok(HashMap::new())
    }

    async fn contexts(
        &self,
        _ids: &[String],
        _from: DateTime<Utc>,
    ) -> Result<HashMap<String, ExportContext>, MonitorError> {
        self.call()?;
        Ok(self.contexts.clone())
    }

    async fn matching(
        &self,
        _filters: &[ScoreFilter],
        _q: &TranscriptQuery,
    ) -> Result<Vec<(String, DateTime<Utc>)>, MonitorError> {
        self.call()?;
        Ok(Vec::new())
    }
}

/// State with csrf `CSRF`, port `PORT`, clock `fixed_now` and no report store.
pub(crate) fn state(source: Arc<FakeSource>, store: Option<Arc<dyn PromptStore>>) -> AppState {
    state_with_reports(source, store, None)
}

/// [`state`] with a report cache.
pub(crate) fn state_with_reports(
    source: Arc<FakeSource>,
    store: Option<Arc<dyn PromptStore>>,
    reports: Option<Arc<ReportCache>>,
) -> AppState {
    AppState {
        source,
        store,
        reports,
        csrf: Arc::from(CSRF),
        port: PORT,
        now: fixed_now,
        prices: None,
        usage: None,
        author: AUTHOR.to_owned(),
        datasets: None,
    }
}

/// `secs` after the Unix epoch (`1_800_000_000` is 2027-01-15T08:00:00Z).
pub(crate) fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap()
}

/// A report of `spec` generated at `at(secs)` over the hour before it, from source
/// `bigquery:p.d.t`, with one run per entry of `runs`: run `i` is invocation `inv-{i}` in
/// session `session-{i}` of agent `agent`, started at `at(secs - 60)`, with one turn `hi` →
/// `hello` and no tool calls, and one `contains` score per outcome with rationale `r{j}`.
pub(crate) fn report(spec: &str, secs: i64, runs: &[&[Outcome]]) -> MonitorReport {
    MonitorReport {
        spec: spec.to_owned(),
        generated_at: at(secs),
        window: Window {
            from: at(secs - 3_600),
            to: at(secs),
        },
        source: "bigquery:p.d.t".to_owned(),
        runs: runs
            .iter()
            .enumerate()
            .map(|(i, outcomes)| MonitoredRun {
                run: ProductionRun {
                    invocation_id: format!("inv-{i}"),
                    session_id: format!("session-{i}"),
                    trace_id: None,
                    agent: "agent".to_owned(),
                    started_at: at(secs - 60),
                    transcript: RunTranscript {
                        turns: vec![TurnRecord {
                            user: "hi".to_owned(),
                            reply: "hello".to_owned(),
                            calls: Vec::new(),
                        }],
                        error: None,
                    },
                    content_complete: true,
                    tools_complete: true,
                    notes: Vec::new(),
                },
                scores: outcomes
                    .iter()
                    .enumerate()
                    .map(|(j, outcome)| ScoreResult {
                        kind: "contains".to_owned(),
                        outcome: *outcome,
                        score: None,
                        rationale: format!("r{j}"),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// A temp-dir report store holding `reports` (stored in order with `ReportStore::put`) behind a
/// cache of `cap` entries; returns the dir guard, the cache and the stored names.
pub(crate) async fn report_cache(
    reports: &[MonitorReport],
    cap: usize,
) -> (tempfile::TempDir, Arc<ReportCache>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    let mut names = Vec::new();
    for r in reports {
        names.push(store.put(r).await.unwrap());
    }
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    (dir, Arc::new(ReportCache::new(store, cap)), names)
}

/// The `Host` value the guard accepts for `PORT`.
fn host() -> String {
    format!("127.0.0.1:{PORT}")
}

/// Sends GET path with Host 127.0.0.1:PORT through router(state) via tower::ServiceExt::oneshot.
pub(crate) async fn get(state: AppState, path: &str) -> (StatusCode, HeaderMap, String) {
    let req = Request::get(path)
        .header(HOST, host())
        .body(Body::empty())
        .unwrap();
    send(state, req).await
}

/// POSTs an application/x-www-form-urlencoded body with Host 127.0.0.1:PORT and the given
/// Origin (None = no header).
pub(crate) async fn post_form(
    state: AppState,
    path: &str,
    body: &str,
    origin: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::post(path)
        .header(HOST, host())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(origin) = origin {
        req = req.header(ORIGIN, origin);
    }
    send(state, req.body(Body::from(body.to_owned())).unwrap()).await
}

/// Sends an arbitrary request (for Host tests).
pub(crate) async fn send(state: AppState, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}
