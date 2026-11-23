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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use chrono::{DateTime, TimeZone, Utc};
use gadk_eval::{
    CaseRun, EvalReport, MeasuredReport, Outcome, RunMetrics, RunTranscript, ScoreResult,
    TokenCounts, TurnRecord, Variant, VariantReport,
};
use gadk_eval_monitor::{
    Action, ExperimentReport, ExportContext, ExportSource, GuardReport, MonitorError,
    MonitorReport, MonitoredRun, ProductionRun, ReportStore, ScoreFilter, TraceInvocation,
    TranscriptQuery, Window,
};
use gadk_prompts::PromptStore;
use http::header::{CONTENT_TYPE, HOST, ORIGIN};
use http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;

use super::{AppState, router};
use crate::evals::ReportCache;
use crate::experiments::ExperimentCache;
use crate::guards::GuardCache;
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

/// State with csrf `CSRF`, port `PORT`, clock `fixed_now`, no report store and no export source.
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
        experiments: None,
        guards: None,
        csrf: Arc::from(CSRF),
        port: PORT,
        now: fixed_now,
        prices: None,
        usage: None,
        author: AUTHOR.to_owned(),
        datasets: None,
        export: None,
    }
}

/// [`state`] with an experiment cache (and no monitor-report cache).
pub(crate) fn state_with_experiments(experiments: Arc<ExperimentCache>) -> AppState {
    AppState {
        experiments: Some(experiments),
        ..state(Arc::new(FakeSource::new()), None)
    }
}

/// [`state`] with a guard-report cache (and no monitor-report or experiment cache).
pub(crate) fn state_with_guards(guards: Arc<GuardCache>) -> AppState {
    AppState {
        guards: Some(guards),
        ..state(Arc::new(FakeSource::new()), None)
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

/// One run of [`experiment`]: the case id and its score outcomes.
pub(crate) type ExpRun<'a> = (&'a str, &'a [Outcome]);
/// One variant of [`experiment`]: its name and its runs.
pub(crate) type ExpVariant<'a> = (&'a str, &'a [ExpRun<'a>]);

/// The model every run of [`experiment`] calls.
pub(crate) const EXP_MODEL: &str = "gemini-3.8-flash";

/// An experiment `name` over the eval set `set`, generated at `at(secs)`, with one variant per
/// entry of `variants` (`(variant name, runs)`, built with `Variant::new`, so no prompt). Each run
/// is `(case id, outcomes)`: run numbers count from 1 per case; the transcript is one turn `hi` →
/// `"{variant} reply to {case}"` without error; one `contains` score per outcome with rationale
/// `r{j}` and no score. The `i`-th run of a variant (from 0) took `100 × (i + 1)` ms and called
/// [`EXP_MODEL`] with 1,000 input, 200 cached, 300 output and 50 reasoning tokens.
pub(crate) fn experiment(name: &str, secs: i64, variants: &[ExpVariant<'_>]) -> ExperimentReport {
    let mut reports = Vec::new();
    let mut metrics = Vec::new();
    for (variant, runs) in variants {
        let mut case_runs: Vec<CaseRun> = Vec::new();
        let mut variant_metrics = Vec::new();
        for (i, (case, outcomes)) in runs.iter().enumerate() {
            let run = 1 + case_runs.iter().filter(|r| r.case == *case).count() as u32;
            case_runs.push(CaseRun {
                case: (*case).to_owned(),
                run,
                transcript: RunTranscript {
                    turns: vec![TurnRecord {
                        user: "hi".to_owned(),
                        reply: format!("{variant} reply to {case}"),
                        calls: Vec::new(),
                    }],
                    error: None,
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
            });
            variant_metrics.push(RunMetrics::new(
                100 * (i as u64 + 1),
                BTreeMap::from([(EXP_MODEL.to_owned(), TokenCounts::new(1_000, 200, 300, 50))]),
            ));
        }
        reports.push(VariantReport {
            name: (*variant).to_owned(),
            runs: case_runs,
        });
        metrics.push(variant_metrics);
    }
    let built: Vec<Variant> = variants.iter().map(|(v, _)| Variant::new(*v)).collect();
    let measured = MeasuredReport::new(
        EvalReport {
            set: "set".to_owned(),
            variants: reports,
        },
        metrics,
    );
    ExperimentReport::new(name, at(secs), &built, measured)
}

/// A temp-dir report store holding `experiments` (stored in order with
/// `ReportStore::put_experiment`) behind an experiment cache of `cap` entries; returns the dir
/// guard, the cache and the stored names.
pub(crate) async fn experiment_cache(
    experiments: &[ExperimentReport],
    cap: usize,
) -> (tempfile::TempDir, Arc<ExperimentCache>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    let mut names = Vec::new();
    for x in experiments {
        names.push(store.put_experiment(x).await.unwrap());
    }
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    (dir, Arc::new(ExperimentCache::new(store, cap)), names)
}

/// A guard report of spec `spec` generated at `at(secs)` over the hour before it, from source
/// `bigquery:p.d.t`, for label `production` of `prompt` with baseline v1 and rollout v2 at
/// `percent`%, and `action` (`applied` or not). The verdict follows the action: `advance` for a
/// ramp or a promote, `abort` for an abort, `wait` otherwise. Two checks:
/// - `error_rate` (rate): baseline 0.00% (n=5), rollout 100.00% (n=6), harm +100.00pt, interval
///   [+50.00pt, +100.00pt], `worse` with reason `rollout errors`;
/// - `latency_p95` (ms): baseline 1200 ms (n=5), rollout — (n=0), no harm, no interval,
///   `insufficient` with reason `too few calls`.
///
/// No note. `GuardReport` is `#[non_exhaustive]`, so it is built from its JSON.
pub(crate) fn guard(
    spec: &str,
    secs: i64,
    prompt: &str,
    percent: u8,
    action: Action,
    applied: bool,
) -> GuardReport {
    let verdict = match action {
        Action::Ramp(_) | Action::Promote => "advance",
        Action::Abort => "abort",
        _ => "wait",
    };
    let json = serde_json::json!({
        "spec": spec,
        "generated_at": at(secs),
        "window": { "from": at(secs - 3_600), "to": at(secs) },
        "source": "bigquery:p.d.t",
        "prompt": prompt,
        "label": "production",
        "baseline": 1,
        "rollout": 2,
        "percent": percent,
        "checks": [
            {
                "name": "error_rate",
                "unit": "rate",
                "baseline": { "value": 0.0, "n": 5 },
                "rollout": { "value": 1.0, "n": 6 },
                "harm": 1.0,
                "interval": [0.5, 1.0],
                "status": "worse",
                "reason": "rollout errors"
            },
            {
                "name": "latency_p95",
                "unit": "millis",
                "baseline": { "value": 1200.0, "n": 5 },
                "rollout": { "value": null, "n": 0 },
                "harm": null,
                "interval": null,
                "status": "insufficient",
                "reason": "too few calls"
            }
        ],
        "verdict": verdict,
        "action": action,
        "applied": applied
    });
    GuardReport::from_json(&json.to_string()).unwrap()
}

/// A temp-dir report store holding `guards` (stored in order with `ReportStore::put_guard`)
/// behind a guard cache of `cap` entries; returns the dir guard, the cache and the stored names.
pub(crate) async fn guard_cache(
    guards: &[GuardReport],
    cap: usize,
) -> (tempfile::TempDir, Arc<GuardCache>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    let mut names = Vec::new();
    for g in guards {
        names.push(store.put_guard(g).await.unwrap());
    }
    let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
    (dir, Arc::new(GuardCache::new(store, cap)), names)
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
