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

//! Prompt routes: `/prompts`, `/prompts/{name}` and the label and rollout writes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::Form;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use gadk_analytics::AnalyticsSource;
use gadk_analytics::cost::CostSum;
use gadk_analytics::prompts::VersionMetrics;
use gadk_eval_monitor::MonitorError;
use gadk_prompts::{Label, PromptSnapshot, Rollout, RolloutSnapshot, StoreError};
use http::StatusCode;
use maud::{Markup, html};
use serde::Deserialize;

use super::AppState;
use super::guard::check_csrf;
use super::pages::error::{banner, error_page, store_error, store_status, store_text};
use super::pages::layout::{self, Nav};
use super::pages::prompt::{FeedbackBody, MetricsBody, MetricsRow, PassColumn, ProductionMetrics};
use super::pages::{prompt, prompts};
use crate::evals::ReportCache;
use crate::prices::{PriceTable, cost_of};
use crate::prompts::{diff_view, prompt_detail, prompt_rows, version_view};
use crate::traces::TimeRange;

const NO_STORE: &str = "No prompt store configured (start with --store)";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/prompts", get(list))
        .route("/prompts/{name}", get(detail))
        .route("/prompts/{name}/labels", post(set_label))
        .route("/prompts/{name}/labels/{label}/delete", post(delete_label))
        .route(
            "/prompts/{name}/labels/{label}/rollout",
            post(start_rollout),
        )
        .route(
            "/prompts/{name}/labels/{label}/rollout/promote",
            post(promote_rollout),
        )
        .route(
            "/prompts/{name}/labels/{label}/rollout/delete",
            post(abort_rollout),
        )
}

fn no_store_404() -> Response {
    error_page(StatusCode::NOT_FOUND, Nav::Prompts, NO_STORE, None)
}

async fn list(State(state): State<AppState>) -> Response {
    let Some(store) = &state.store else {
        let body = html! {
            h1 { "Prompts" }
            p class="empty" { (NO_STORE) }
        };
        return layout::html(StatusCode::OK, layout::page("Prompts", Nav::Prompts, body));
    };
    match store.load_all().await {
        Ok(snap) => layout::html(StatusCode::OK, prompts::render(&prompt_rows(&snap))),
        Err(e) => store_error(&e),
    }
}

/// `?v=`, `?a=`, `?b=` and `?range=` (the production-metrics range); empty values count as
/// absent.
#[derive(Debug, Default, Deserialize)]
struct DetailQuery {
    v: Option<String>,
    a: Option<String>,
    b: Option<String>,
    range: Option<String>,
}

fn bad_request(text: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, Nav::Prompts, text, None)
}

/// An absent or empty parameter is `Ok(None)`; anything but a u32 is a 400 page.
#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
fn version_param(key: &str, raw: Option<&str>) -> Result<Option<u32>, Response> {
    match raw.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(|_| bad_request(&format!("{key}: not a version number"))),
    }
}

/// What the detail page shows besides the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    /// The template and config of one version (`None`: the latest).
    Version(Option<u32>),
    /// The diff of `a` against `b`.
    Diff(u32, u32),
}

async fn detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DetailQuery>,
) -> Response {
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let show = match parse_show(&q) {
        Ok(show) => show,
        Err(resp) => return resp,
    };
    let range = metrics_range(q.range.as_deref());
    match store.load_prompt(&name).await {
        Ok((snap, rollouts)) => {
            let metrics = if prompt_detail(&snap, &rollouts, &name).is_some() {
                Some(production_metrics(&state, &name, range).await)
            } else {
                None
            };
            detail_page(
                &state,
                (&snap, &rollouts),
                &name,
                show,
                StatusCode::OK,
                None,
                metrics.as_ref(),
            )
        }
        Err(e) => store_error(&e),
    }
}

#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
fn parse_show(q: &DetailQuery) -> Result<Show, Response> {
    let v = version_param("v", q.v.as_deref())?;
    let a = version_param("a", q.a.as_deref())?;
    let b = version_param("b", q.b.as_deref())?;
    match (a, b) {
        (Some(a), Some(b)) => Ok(Show::Diff(a, b)),
        (None, None) => Ok(Show::Version(v)),
        _ => Err(bad_request("a diff needs both a and b")),
    }
}

/// Renders the detail page of the loaded prompts and rollouts with `status`; an unknown
/// prompt or version is a 404 page. `metrics`, when given, is the production-metrics section.
fn detail_page(
    state: &AppState,
    (snap, rollouts): (&PromptSnapshot, &RolloutSnapshot),
    name: &str,
    show: Show,
    status: StatusCode,
    error: Option<Markup>,
    metrics: Option<&ProductionMetrics>,
) -> Response {
    let not_found = |what: String| error_page(StatusCode::NOT_FOUND, Nav::Prompts, &what, None);
    let Some(d) = prompt_detail(snap, rollouts, name) else {
        return not_found(format!("prompt {name:?} not found"));
    };
    let (view, diff) = match show {
        Show::Version(v) => {
            let v = v.unwrap_or(d.latest);
            match version_view(snap, name, v) {
                Some(view) => (Some(view), None),
                None => return not_found(format!("prompt {name:?} has no version {v}")),
            }
        }
        Show::Diff(a, b) => match diff_view(snap, name, a, b) {
            Some(diff) => (None, Some(diff)),
            None => return not_found(format!("prompt {name:?} has no version {a} or {b}")),
        },
    };
    layout::html(
        status,
        prompt::render(
            &d,
            view.as_ref(),
            diff.as_ref(),
            &state.csrf,
            error,
            metrics,
            state.guards.is_some(),
        ),
    )
}

/// The `range` of the production metrics on `/prompts/{name}` and
/// `/api/v1/prompts/{name}/versions`: an unknown or missing value is 7d.
pub(crate) fn metrics_range(raw: Option<&str>) -> TimeRange {
    raw.and_then(TimeRange::parse).unwrap_or(TimeRange::Days7)
}

/// The newest monitor reports read for the eval pass-rate column.
const PASS_RATE_REPORTS: usize = 50;
/// The most invocations mapped to prompt versions for the pass-rate column.
const PASS_RATE_IDS: usize = 5_000;

/// One monitored run for the pass-rate column: invocation id, start, passed.
type EvalRun = (String, DateTime<Utc>, bool);

/// The production-metrics section of prompt `name` over the `range` before now.
async fn production_metrics(state: &AppState, name: &str, range: TimeRange) -> ProductionMetrics {
    let body = match &state.usage {
        None => MetricsBody::NeedsBigQuery,
        Some(source) => {
            let end = (state.now)();
            let start = end - range.duration();
            match source.version_metrics(name, start, end).await {
                Err(e) => MetricsBody::Failed(e.to_string(), e.hint()),
                Ok(m) => {
                    let (pass, counts) =
                        pass_rates(state.reports.as_deref(), source, name, start, end).await;
                    let feedback = match source.version_feedback(name, start, end).await {
                        Ok(f) => FeedbackBody::Rows {
                            rows: f.rows,
                            truncated: f.truncated,
                        },
                        Err(e) => FeedbackBody::Failed(e.to_string(), e.hint()),
                    };
                    MetricsBody::Rows {
                        rows: metric_rows(&m, state.prices.as_deref(), &counts),
                        priced: state.prices.is_some(),
                        pass,
                        truncated: m.truncated,
                        feedback,
                    }
                }
            }
        }
    };
    ProductionMetrics { range, body }
}

/// The pass-rate column and `(passed, runs)` per version: the runs of the newest reports that
/// started in `[start, end)`, mapped to versions by their invocations' `LLM_REQUEST` rows.
async fn pass_rates(
    reports: Option<&ReportCache>,
    source: &AnalyticsSource,
    name: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> (PassColumn, BTreeMap<u32, (u64, u64)>) {
    let Some(reports) = reports else {
        return (PassColumn::Off, BTreeMap::new());
    };
    let runs = match eval_runs(reports, start, end).await {
        Ok(runs) => runs,
        Err(e) => return (PassColumn::Failed(e.to_string()), BTreeMap::new()),
    };
    let (ids, capped) = eval_ids(&runs, PASS_RATE_IDS);
    if ids.is_empty() {
        return (
            PassColumn::On {
                capped,
                truncated: false,
            },
            BTreeMap::new(),
        );
    }
    match source
        .versions_for_invocations(name, &ids, start, end)
        .await
    {
        Ok(mapping) => (
            PassColumn::On {
                capped,
                truncated: mapping.truncated,
            },
            pass_counts(&runs, &ids, &mapping.pairs),
        ),
        Err(e) => (PassColumn::Failed(e.to_string()), BTreeMap::new()),
    }
}

/// The runs that started in `[start, end)` of the newest [`PASS_RATE_REPORTS`] reports whose
/// window overlaps that range, newest first.
async fn eval_runs(
    reports: &ReportCache,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<EvalRun>, MonitorError> {
    let mut runs = Vec::new();
    for name in reports.names(PASS_RATE_REPORTS).await? {
        let Some(report) = reports.find(&name).await? else {
            continue;
        };
        if report.window.from >= end || report.window.to <= start {
            continue;
        }
        for r in &report.runs {
            if r.run.started_at >= start && r.run.started_at < end {
                runs.push((r.run.invocation_id.clone(), r.run.started_at, r.passed()));
            }
        }
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.1));
    Ok(runs)
}

/// The distinct invocation ids of `runs` in order, at most `cap`, and whether more existed.
fn eval_ids(runs: &[EvalRun], cap: usize) -> (Vec<String>, bool) {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for (id, _, _) in runs {
        if seen.insert(id.as_str()) {
            ids.push(id.clone());
        }
    }
    let capped = ids.len() > cap;
    ids.truncate(cap);
    (ids, capped)
}

/// `(passed, runs)` per version: each run whose invocation is in `ids` counts once for every
/// version that invocation used.
fn pass_counts(
    runs: &[EvalRun],
    ids: &[String],
    mapping: &[(String, u32)],
) -> BTreeMap<u32, (u64, u64)> {
    let kept: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut versions: HashMap<&str, BTreeSet<u32>> = HashMap::new();
    for (id, version) in mapping {
        versions.entry(id.as_str()).or_default().insert(*version);
    }
    let mut out: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    for (id, _, passed) in runs {
        if !kept.contains(id.as_str()) {
            continue;
        }
        for version in versions.get(id.as_str()).into_iter().flatten() {
            let entry = out.entry(*version).or_insert((0, 0));
            entry.1 += 1;
            if *passed {
                entry.0 += 1;
            }
        }
    }
    out
}

/// The table rows: stats per version, the mean cost per priced call from the version's
/// per-model tokens, and the pass counts.
fn metric_rows(
    m: &VersionMetrics,
    prices: Option<&PriceTable>,
    pass: &BTreeMap<u32, (u64, u64)>,
) -> Vec<MetricsRow> {
    m.stats
        .iter()
        .map(|s| {
            let mut cost = CostSum::default();
            let mut priced_calls = 0u64;
            for vm in m.models.iter().filter(|vm| vm.version == s.version) {
                let c = cost_of(prices, &vm.calls.model, &vm.calls.usage);
                if c.is_some() {
                    priced_calls += vm.calls.calls;
                }
                cost.add(c);
            }
            MetricsRow {
                version: s.version,
                calls: s.calls,
                mean_input: s.mean_input,
                mean_output: s.mean_output,
                mean_cost: (priced_calls > 0).then(|| cost.usd / priced_calls as f64),
                cost_partial: cost.partial(),
                p50_ms: s.p50_ms,
                p95_ms: s.p95_ms,
                pass: pass.get(&s.version).copied(),
            }
        })
        .collect()
}

/// After a failed label or rollout write: the detail page with the error banner and the
/// status, or the plain error page when the prompt itself cannot be shown. An `Http` body in
/// the banner is cut to an excerpt of at most 1,024 bytes.
async fn write_failed(state: &AppState, name: &str, e: &StoreError) -> Response {
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let Ok((snap, rollouts)) = store.load_prompt(name).await else {
        return store_error(e);
    };
    if prompt_detail(&snap, &rollouts, name).is_none() {
        return store_error(e);
    }
    let error = banner(&store_text(e), None);
    detail_page(
        state,
        (&snap, &rollouts),
        name,
        Show::Version(None),
        store_status(e),
        Some(error),
        None,
    )
}

fn written(name: &str) -> Response {
    Redirect::to(&prompt::href(name)).into_response()
}

/// The set-label form; missing fields are empty strings.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SetLabelForm {
    label: String,
    version: String,
    expected: String,
    csrf: String,
}

/// `version` (a u32) and `expected` (empty: the label must be unset; a u32: it must point
/// there), as `set_label` arguments.
fn set_args(form: &SetLabelForm) -> Result<(Label, u32, Option<Option<u32>>), StoreError> {
    let label = Label::new(&form.label)?;
    let version = form
        .version
        .parse()
        .map_err(|_| StoreError::Invalid(format!("version {:?}: not a number", form.version)))?;
    let expected = if form.expected.is_empty() {
        None
    } else {
        Some(form.expected.parse().map_err(|_| {
            StoreError::Invalid(format!("expected {:?}: not a number", form.expected))
        })?)
    };
    Ok((label, version, Some(expected)))
}

async fn set_label(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<SetLabelForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let result = match set_args(&form) {
        Ok((label, version, expected)) => store.set_label(&name, &label, version, expected).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => written(&name),
        Err(e) => write_failed(&state, &name, &e).await,
    }
}

/// The delete form: only the token.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeleteForm {
    csrf: String,
}

async fn delete_label(
    State(state): State<AppState>,
    Path((name, label)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let result = match Label::new(&label) {
        Ok(label) => store.remove_label(&name, &label).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => written(&name),
        Err(e) => write_failed(&state, &name, &e).await,
    }
}

/// The redirect back to the prompt page after a write, or [`write_failed`].
async fn write_result(state: &AppState, name: &str, result: Result<(), StoreError>) -> Response {
    match result {
        Ok(()) => written(name),
        Err(e) => write_failed(state, name, &e).await,
    }
}

/// Form field `key` as a number; anything else is `Invalid`.
fn number<T: std::str::FromStr>(key: &str, raw: &str) -> Result<T, StoreError> {
    raw.parse()
        .map_err(|_| StoreError::Invalid(format!("{key} {raw:?}: not a number")))
}

/// The start-or-change rollout form; missing fields are empty strings.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RolloutForm {
    version: String,
    percent: String,
    csrf: String,
}

/// The `version` and `percent` form fields as a [`Rollout`]: `percent` parses as a `u32`, and
/// [`Rollout::new`] checks the bounds.
fn rollout_args(version: &str, percent: &str) -> Result<Rollout, StoreError> {
    let version = number("version", version)?;
    let percent: u32 = number("percent", percent)?;
    let percent = u8::try_from(percent).map_err(|_| {
        StoreError::Invalid(format!(
            "rollout percent must be between 1 and 99, got {percent}"
        ))
    })?;
    Rollout::new(version, percent)
}

async fn start_rollout(
    State(state): State<AppState>,
    Path((name, label)): Path<(String, String)>,
    Form(form): Form<RolloutForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let args =
        Label::new(&label).and_then(|l| Ok((l, rollout_args(&form.version, &form.percent)?)));
    let result = match args {
        Ok((label, rollout)) => store.set_rollout(&name, &label, Some(rollout)).await,
        Err(e) => Err(e),
    };
    write_result(&state, &name, result).await
}

/// The promote form: the rollout version and percent and the label version the page showed.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PromoteForm {
    version: String,
    percent: String,
    expected: String,
    csrf: String,
}

/// Moves the label to the rollout version if it still points at `expected` and its rollout is
/// still `version` at `percent`; the store removes the rollout with the move, in one write.
async fn promote_rollout(
    State(state): State<AppState>,
    Path((name, label)): Path<(String, String)>,
    Form(form): Form<PromoteForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let args = Label::new(&label).and_then(|l| {
        let read = rollout_args(&form.version, &form.percent)?;
        let expected: u32 = number("expected", &form.expected)?;
        Ok((l, read, expected))
    });
    let result = match args {
        Ok((label, read, expected)) => {
            store
                .promote_rollout_if(&name, &label, expected, read)
                .await
        }
        Err(e) => Err(e),
    };
    write_result(&state, &name, result).await
}

async fn abort_rollout(
    State(state): State<AppState>,
    Path((name, label)): Path<(String, String)>,
    Form(form): Form<DeleteForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(store) = &state.store else {
        return no_store_404();
    };
    let result = match Label::new(&label) {
        Ok(label) => store.set_rollout(&name, &label, None).await,
        Err(e) => Err(e),
    };
    write_result(&state, &name, result).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use gadk_prompts::{FileStore, Label, NewVersion, PromptStore, Rollout};
    use http::StatusCode;
    use http::header::LOCATION;
    use serde_json::json;

    use crate::evals::ReportCache;
    use crate::prices::PriceTable;
    use crate::traces::BigQueryTable;
    use crate::web::AppState;
    use crate::web::testutil::{CSRF, FakeSource, get, post_form, state};
    use crate::web::testutil::{fixed_now, guard_cache, report, report_cache, state_with_reports};
    use gadk_analytics::AnalyticsSource;
    use gadk_analytics::testutil::{FakeHttp, param_value, reply};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use gadk_eval::Outcome;
    use serde_json::Value;

    const ORIGIN: &str = "http://127.0.0.1:8765";
    const NO_STORE: &str = "No prompt store configured (start with --store)";

    /// `greet` v1..v2 (production → 1, staging → 2), `summarize` v1 (a `<b>` template),
    /// `team/greet` v1.
    async fn seeded() -> (tempfile::TempDir, Arc<FileStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(dir.path()));
        let versions = [
            ("greet", "Hello {{name}}\n", None),
            (
                "greet",
                "Hello {{name}}!\nBye\n",
                Some(json!({"temperature": 0.2})),
            ),
            ("summarize", "Summarize <b>{{text}}</b>\n", None),
            ("team/greet", "Hi\n", None),
        ];
        for (name, template, config) in versions {
            store
                .create_version(NewVersion {
                    name: name.into(),
                    template: template.into(),
                    config,
                    author: "chris".into(),
                    message: format!("{name} change"),
                })
                .await
                .unwrap();
        }
        store
            .set_label("greet", &Label::production(), 1, None)
            .await
            .unwrap();
        store
            .set_label("greet", &Label::staging(), 2, None)
            .await
            .unwrap();
        (dir, store)
    }

    fn st(store: &Arc<FileStore>) -> AppState {
        let store: Arc<dyn PromptStore> = store.clone();
        state(Arc::new(FakeSource::new()), Some(store))
    }

    fn no_store() -> AppState {
        state(Arc::new(FakeSource::new()), None)
    }

    async fn labels(store: &FileStore, name: &str) -> BTreeMap<String, u32> {
        let snap = store.load_all().await.unwrap();
        snap.prompts[name]
            .labels
            .iter()
            .map(|(l, v)| (l.as_str().to_owned(), *v))
            .collect()
    }

    fn set_body(label: &str, version: &str, expected: &str) -> String {
        format!("label={label}&version={version}&expected={expected}&csrf={CSRF}")
    }

    #[tokio::test]
    async fn list_shows_every_prompt_with_latest_and_labels() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(r#"<a href="/prompts/greet">greet</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/prompts/summarize">summarize</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/prompts/team%2Fgreet">team/greet</a>"#),
            "{body}"
        );
        assert!(body.contains(r#"<td class="num">v2</td>"#), "{body}");
        assert!(
            body.contains(r#"<span class="chip">production → v1</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<span class="chip">staging → v2</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a class="active" href="/prompts">"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn without_a_store_the_list_explains_and_the_rest_is_404() {
        let (status, _, body) = get(no_store(), "/prompts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(NO_STORE), "{body}");

        let (status, _, body) = get(no_store(), "/prompts/greet").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains(NO_STORE), "{body}");

        let (status, _, _) = post_form(
            no_store(),
            "/prompts/greet/labels",
            &set_body("canary", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = post_form(
            no_store(),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        for (path, body) in [
            (
                "/prompts/greet/labels/production/rollout",
                format!("version=2&percent=10&csrf={CSRF}"),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                format!("version=2&percent=10&expected=1&csrf={CSRF}"),
            ),
            (
                "/prompts/greet/labels/production/rollout/delete",
                format!("csrf={CSRF}"),
            ),
        ] {
            let (status, _, _) = post_form(no_store(), path, &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn detail_lists_versions_newest_first_with_labels() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        let v2 = body.find(r#"href="/prompts/greet?v=2""#).expect("v2 link");
        let v1 = body.find(r#"href="/prompts/greet?v=1""#).expect("v1 link");
        assert!(v2 < v1, "newest first: {body}");
        assert!(body.contains("greet change"), "{body}");
        assert!(body.contains("chris"), "{body}");
        assert!(
            body.contains(r#"<span class="chip">production</span>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<span class="chip">staging</span>"#),
            "{body}"
        );
        // No ?v= selects the latest version.
        assert!(body.contains("Hello {{name}}!"), "{body}");
    }

    #[tokio::test]
    async fn detail_links_the_guard_history_when_a_report_store_is_configured() {
        let (_dir, store) = seeded().await;
        let (_reports, guards, _) = guard_cache(&[], 4).await;
        let mut with_guards = st(&store);
        with_guards.guards = Some(guards);
        for (name, href) in [
            ("greet", "/guards?prompt=greet"),
            ("team%2Fgreet", "/guards?prompt=team%2Fgreet"),
            // `summarize` has no labels: the link follows `No labels.`.
            ("summarize", "/guards?prompt=summarize"),
        ] {
            let path = format!("/prompts/{name}");
            let (status, _, body) = get(with_guards.clone(), &path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(
                body.contains(&format!(r#"<p><a href="{href}">guard history</a></p>"#)),
                "{path}: {body}"
            );
            let (_, _, body) = get(st(&store), &path).await;
            assert!(!body.contains("guard history"), "{path}: {body}");
        }
    }

    #[tokio::test]
    async fn version_query_shows_template_and_config() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet?v=2").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h2>Version 2</h2>"), "{body}");
        assert!(
            body.contains("<pre class=\"template\">Hello {{name}}!\nBye\n</pre>"),
            "{body}"
        );
        assert!(
            body.contains("<pre class=\"config\">{\n  &quot;temperature&quot;: 0.2\n}</pre>"),
            "{body}"
        );

        let (status, _, body) = get(st(&store), "/prompts/greet?v=1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("<pre class=\"template\">Hello {{name}}\n</pre>"),
            "{body}"
        );
        assert!(body.contains("No config"), "{body}");
    }

    #[tokio::test]
    async fn diff_query_renders_tagged_rows() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet?a=1&b=2").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("v1 → v2"), "{body}");
        assert!(body.contains(r#"<table class="diff">"#), "{body}");
        assert!(
            body.contains(
                r#"<tr class="diff-delete"><td class="ln">1</td><td class="ln"></td><td>-</td><td>Hello {{name}}</td></tr>"#
            ),
            "{body}"
        );
        assert!(
            body.contains(
                r#"<tr class="diff-insert"><td class="ln"></td><td class="ln">1</td><td>+</td><td>Hello {{name}}!</td></tr>"#
            ),
            "{body}"
        );
        assert!(
            body.contains(
                r#"<tr class="diff-insert"><td class="ln"></td><td class="ln">2</td><td>+</td><td>Bye</td></tr>"#
            ),
            "{body}"
        );
        // The config diff: nothing in v1, three pretty-printed lines in v2.
        assert!(
            body.contains(r#"<td>+</td><td>  &quot;temperature&quot;: 0.2</td>"#),
            "{body}"
        );
        // The picker preselects the compared versions.
        assert!(
            body.contains(r#"<select name="a"><option value="2">v2</option><option value="1" selected>v1</option></select>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn unknown_or_partial_queries_are_rejected() {
        let (_dir, store) = seeded().await;
        for (path, want) in [
            ("/prompts/nope", StatusCode::NOT_FOUND),
            ("/prompts/greet?v=9", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=1&b=9", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=9&b=1", StatusCode::NOT_FOUND),
            ("/prompts/greet?a=1", StatusCode::BAD_REQUEST),
            ("/prompts/greet?b=2", StatusCode::BAD_REQUEST),
            ("/prompts/greet?v=two", StatusCode::BAD_REQUEST),
            ("/prompts/greet?a=x&b=1", StatusCode::BAD_REQUEST),
        ] {
            let (status, _, body) = get(st(&store), path).await;
            assert_eq!(status, want, "{path}: {body}");
            assert!(
                body.contains(r#"<div class="banner error">"#),
                "{path}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn nested_names_use_an_encoded_slash() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/team%2Fgreet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>team/greet</h1>"), "{body}");
        assert!(
            body.contains(r#"action="/prompts/team%2Fgreet/labels""#),
            "{body}"
        );

        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/team%2Fgreet/labels",
            &set_body("production", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/team%2Fgreet");
        assert_eq!(labels(&store, "team/greet").await["production"], 1);
    }

    #[tokio::test]
    async fn set_label_creates_a_new_label() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("canary", "2", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(labels(&store, "greet").await["canary"], 2);
    }

    #[tokio::test]
    async fn set_label_moves_when_expected_matches() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("production", "2", "1"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(labels(&store, "greet").await["production"], 2);
    }

    #[tokio::test]
    async fn stale_expected_is_409_on_the_detail_page() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("production", "2", "2"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        assert!(
            body.contains(
                r#"<div class="banner error"><p>conflict: expected v2, found v1</p></div>"#
            ),
            "{body}"
        );

        // An empty `expected` requires the label to be unset.
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("staging", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body.contains("conflict: expected unset, found v2"),
            "{body}"
        );
        let now = labels(&store, "greet").await;
        assert_eq!((now["production"], now["staging"]), (1, 2));
    }

    #[tokio::test]
    async fn invalid_label_or_version_is_400() {
        let (_dir, store) = seeded().await;
        for body in [
            set_body("latest", "1", ""),
            set_body("Bad+Label", "1", ""),
            set_body("canary", "two", ""),
            set_body("canary", "1", "one"),
        ] {
            let (status, _, html) =
                post_form(st(&store), "/prompts/greet/labels", &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(html.contains("<h1>greet</h1>"), "{html}");
            assert!(
                html.contains(r#"<div class="banner error"><p>invalid: "#),
                "{html}"
            );
        }
        assert_eq!(labels(&store, "greet").await.len(), 2);
    }

    #[tokio::test]
    async fn unknown_prompt_or_version_is_404() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/nope/labels",
            &set_body("canary", "1", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("not found: nope"), "{body}");

        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels",
            &set_body("canary", "9", ""),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        assert!(body.contains("not found: greet@v9"), "{body}");
    }

    #[tokio::test]
    async fn delete_removes_the_label() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        let now = labels(&store, "greet").await;
        assert!(!now.contains_key("staging"), "{now:?}");
        assert_eq!(now["production"], 1);

        // Gone already: 404 on the detail page; a bad name: 400.
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels/staging/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        let (status, _, _) = post_form(
            st(&store),
            "/prompts/greet/labels/Bad/delete",
            &format!("csrf={CSRF}"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn wrong_or_missing_csrf_is_403_and_changes_nothing() {
        let (_dir, store) = seeded_with_rollout().await;
        let wrong = "b".repeat(64);
        let cases = [
            (
                "/prompts/greet/labels",
                format!("label=canary&version=1&expected=&csrf={wrong}"),
            ),
            (
                "/prompts/greet/labels",
                "label=canary&version=1&expected=".to_owned(),
            ),
            (
                "/prompts/greet/labels/staging/delete",
                format!("csrf={wrong}"),
            ),
            ("/prompts/greet/labels/staging/delete", String::new()),
            (
                "/prompts/greet/labels/staging/rollout",
                format!("version=1&percent=10&csrf={wrong}"),
            ),
            (
                "/prompts/greet/labels/staging/rollout",
                "version=1&percent=10".to_owned(),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                format!("version=2&percent=10&expected=1&csrf={wrong}"),
            ),
            (
                "/prompts/greet/labels/production/rollout/delete",
                String::new(),
            ),
        ];
        for (path, body) in cases {
            let (status, _, html) = post_form(st(&store), path, &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path} {body}");
            assert!(html.contains("csrf"), "{html}");
        }
        let now = labels(&store, "greet").await;
        assert_eq!(now.len(), 2);
        assert_eq!((now["production"], now["staging"]), (1, 2));
        assert_eq!(rollout(&store, "greet", "production").await, Some((2, 10)));
        assert_eq!(rollout(&store, "greet", "staging").await, None);
    }

    #[tokio::test]
    async fn template_text_is_escaped() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/summarize?v=1").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Summarize &lt;b&gt;{{text}}&lt;/b&gt;"),
            "{body}"
        );
        assert!(!body.contains("<b>{{text}}"), "{body}");
    }

    #[tokio::test]
    async fn every_post_form_carries_csrf_and_a_confirmation() {
        let (_dir, store) = seeded_with_rollout().await;
        let (_, _, body) = get(st(&store), "/prompts/greet").await;
        let forms: Vec<&str> = body
            .split("<form ")
            .skip(1)
            .map(|f| &f[..f.find("</form>").unwrap()])
            .collect();
        let posts: Vec<&&str> = forms
            .iter()
            .filter(|f| f.starts_with(r#"method="post""#))
            .collect();
        // production and staging each have a move and a delete form; production (with a
        // rollout) has the percent, Promote and Abort forms, staging the Start rollout form;
        // plus the new-label form.
        assert_eq!(posts.len(), 9, "{body}");
        let hidden = format!(r#"<input type="hidden" name="csrf" value="{CSRF}">"#);
        for f in &posts {
            assert!(f.contains(&hidden), "{f}");
            assert!(f.contains("data-confirm=\""), "{f}");
        }
        for (action, confirm) in [
            // Moving or removing a label ends its rollout; only production has one.
            (
                "/prompts/greet/labels",
                "Move label production? This ends its rollout.",
            ),
            (
                "/prompts/greet/labels/production/delete",
                "Remove label production? This ends its rollout.",
            ),
            ("/prompts/greet/labels", "Move label staging?"),
            (
                "/prompts/greet/labels/staging/delete",
                "Remove label staging?",
            ),
            (
                "/prompts/greet/labels/production/rollout",
                "Change the rollout of production?",
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                "Promote v2 to production?",
            ),
            (
                "/prompts/greet/labels/production/rollout/delete",
                "Abort the rollout of production?",
            ),
            (
                "/prompts/greet/labels/staging/rollout",
                "Start a rollout on staging?",
            ),
        ] {
            assert!(
                body.contains(&format!(r#"action="{action}" data-confirm="{confirm}""#)),
                "{action}: {body}"
            );
        }
        // The move form for production carries its current version as `expected`.
        assert!(
            body.contains(r#"<input type="hidden" name="label" value="production"><input type="hidden" name="expected" value="1">"#),
            "{body}"
        );
        // The new-label form carries an empty `expected`.
        assert!(
            body.contains(r#"<input type="hidden" name="expected" value="">"#),
            "{body}"
        );
        // The diff picker and the production-metrics range picker are the only other forms,
        // GETs without the token.
        assert_eq!(forms.len(), posts.len() + 2, "{body}");
        assert!(
            body.contains(r#"<form method="get" action="/prompts/greet""#),
            "{body}"
        );
    }

    // ---- rollouts ----

    /// `seeded()` plus a rollout of v2 to 10% of `greet`'s `production` (at v1).
    async fn seeded_with_rollout() -> (tempfile::TempDir, Arc<FileStore>) {
        let (dir, store) = seeded().await;
        store
            .set_rollout(
                "greet",
                &Label::production(),
                Some(Rollout::new(2, 10).unwrap()),
            )
            .await
            .unwrap();
        (dir, store)
    }

    async fn rollout(store: &FileStore, name: &str, label: &str) -> Option<(u32, u8)> {
        let rollouts = store.load_rollouts().await.unwrap();
        rollouts
            .get(name, &Label::new(label).unwrap())
            .map(|r| (r.version, r.percent))
    }

    fn rollout_body(version: &str, percent: &str) -> String {
        format!("version={version}&percent={percent}&csrf={CSRF}")
    }

    fn promote_body(version: &str, expected: &str, percent: &str) -> String {
        format!("version={version}&percent={percent}&expected={expected}&csrf={CSRF}")
    }

    #[tokio::test]
    async fn start_and_change_a_rollout() {
        let (_dir, store) = seeded().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels/production/rollout",
            &rollout_body("2", "10"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(rollout(&store, "greet", "production").await, Some((2, 10)));

        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels/production/rollout",
            &rollout_body("2", "50"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(rollout(&store, "greet", "production").await, Some((2, 50)));
        // The label itself does not move.
        assert_eq!(labels(&store, "greet").await["production"], 1);
    }

    #[tokio::test]
    async fn rollout_page_shows_column_forms_and_chip() {
        let (_dir, store) = seeded_with_rollout().await;
        let (status, _, body) = get(st(&store), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK);
        // The Rollout column: production has one, staging none.
        assert!(
            body.contains(r#"<td class="num">v1</td><td>v2 at 10%</td>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<td class="num">v2</td><td>—</td>"#),
            "{body}"
        );
        // The chip on the rollout version.
        assert!(
            body.contains(r#"<span class="chip">production 10%</span>"#),
            "{body}"
        );
        // production: the percent, Promote and Abort forms.
        assert!(
            body.contains(&format!(
                r#"<form method="post" action="/prompts/greet/labels/production/rollout" data-confirm="Change the rollout of production?" class="inline"><input type="hidden" name="csrf" value="{CSRF}"><input type="hidden" name="version" value="2"><input type="number" name="percent" value="10" min="1" max="99" required>"#
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                r#"<form method="post" action="/prompts/greet/labels/production/rollout/promote" data-confirm="Promote v2 to production?" class="inline"><input type="hidden" name="csrf" value="{CSRF}"><input type="hidden" name="version" value="2"><input type="hidden" name="expected" value="1"><input type="hidden" name="percent" value="10">"#
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                r#"<form method="post" action="/prompts/greet/labels/production/rollout/delete" data-confirm="Abort the rollout of production?" class="inline"><input type="hidden" name="csrf" value="{CSRF}"><button type="submit">Abort</button></form>"#
            )),
            "{body}"
        );
        // staging: the Start rollout form.
        assert!(
            body.contains(&format!(
                r#"<form method="post" action="/prompts/greet/labels/staging/rollout" data-confirm="Start a rollout on staging?" class="inline"><input type="hidden" name="csrf" value="{CSRF}"><select name="version">"#
            )),
            "{body}"
        );
        // staging is at the latest version (v2), so the Start rollout form preselects v1.
        assert!(
            body.contains(r#"<select name="version"><option value="2">v2</option><option value="1" selected>v1</option></select> to "#),
            "{body}"
        );
        assert!(
            body.contains(r#"<input type="number" name="percent" placeholder="percent" min="1" max="99" required>"#),
            "{body}"
        );
        assert!(
            !body.contains("/prompts/greet/labels/staging/rollout/promote"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn invalid_rollout_input_is_400_and_unknown_targets_404() {
        let (_dir, store) = seeded().await;
        for (path, form) in [
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("2", "0"),
            ),
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("2", "100"),
            ),
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("2", "300"),
            ),
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("2", "ten"),
            ),
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("two", "10"),
            ),
            // The label's own version.
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("1", "10"),
            ),
            ("/prompts/greet/labels/Bad/rollout", rollout_body("2", "10")),
            (
                "/prompts/greet/labels/production/rollout/promote",
                promote_body("two", "1", "10"),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                promote_body("2", "", "10"),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                promote_body("2", "1", "ten"),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                promote_body("2", "1", "0"),
            ),
            (
                "/prompts/greet/labels/Bad/rollout/delete",
                format!("csrf={CSRF}"),
            ),
        ] {
            let (status, _, html) = post_form(st(&store), path, &form, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {form}");
            assert!(html.contains("<h1>greet</h1>"), "{html}");
            assert!(
                html.contains(r#"<div class="banner error"><p>invalid: "#),
                "{path} {form}: {html}"
            );
        }
        assert_eq!(rollout(&store, "greet", "production").await, None);
        assert_eq!(labels(&store, "greet").await["production"], 1);

        for (path, form, want) in [
            (
                "/prompts/greet/labels/production/rollout",
                rollout_body("9", "10"),
                "not found: greet@v9",
            ),
            (
                "/prompts/greet/labels/canary/rollout",
                rollout_body("2", "10"),
                "not found: greet@canary",
            ),
            (
                "/prompts/greet/labels/canary/rollout/delete",
                format!("csrf={CSRF}"),
                "not found: greet@canary",
            ),
        ] {
            let (status, _, html) = post_form(st(&store), path, &form, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(html.contains("<h1>greet</h1>"), "{html}");
            assert!(html.contains(want), "{path}: {html}");
        }
        let (status, _, html) = post_form(
            st(&store),
            "/prompts/nope/labels/production/rollout",
            &rollout_body("2", "10"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(html.contains("not found: nope"), "{html}");
    }

    #[tokio::test]
    async fn promote_moves_the_label_and_ends_the_rollout() {
        let (_dir, store) = seeded_with_rollout().await;
        let (status, headers, _) = post_form(
            st(&store),
            "/prompts/greet/labels/production/rollout/promote",
            &promote_body("2", "1", "10"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/prompts/greet");
        assert_eq!(labels(&store, "greet").await["production"], 2);
        assert_eq!(rollout(&store, "greet", "production").await, None);
    }

    #[tokio::test]
    async fn promote_with_a_changed_rollout_is_409_and_keeps_it() {
        let (_dir, store) = seeded_with_rollout().await;
        store
            .create_version(NewVersion {
                name: "greet".into(),
                template: "Hey {{name}}\n".into(),
                config: None,
                author: "chris".into(),
                message: "v3".into(),
            })
            .await
            .unwrap();
        // The page showed v2 at 10%; meanwhile the percent, then the version, changed.
        for (now, want) in [
            (
                (2, 50),
                "the v2 rollout changed (its percent); reload and retry",
            ),
            ((3, 10), "expected v2, found v3"),
        ] {
            store
                .set_rollout(
                    "greet",
                    &Label::production(),
                    Some(Rollout::new(now.0, now.1).unwrap()),
                )
                .await
                .unwrap();
            let (status, _, body) = post_form(
                st(&store),
                "/prompts/greet/labels/production/rollout/promote",
                &promote_body("2", "1", "10"),
                Some(ORIGIN),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{now:?}");
            assert!(body.contains("<h1>greet</h1>"), "{body}");
            assert!(
                body.contains(&format!(
                    r#"<div class="banner error"><p>conflict: {want}</p></div>"#
                )),
                "{now:?}: {body}"
            );
            assert_eq!(labels(&store, "greet").await["production"], 1);
            assert_eq!(rollout(&store, "greet", "production").await, Some(now));
        }
    }

    #[tokio::test]
    async fn promote_with_a_stale_expected_is_409_and_keeps_the_rollout() {
        let (_dir, store) = seeded_with_rollout().await;
        // `expected` says v2, but production is at v1.
        let (status, _, body) = post_form(
            st(&store),
            "/prompts/greet/labels/production/rollout/promote",
            &promote_body("2", "2", "10"),
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("<h1>greet</h1>"), "{body}");
        assert!(
            body.contains(
                r#"<div class="banner error"><p>conflict: expected v2, found v1</p></div>"#
            ),
            "{body}"
        );
        assert_eq!(labels(&store, "greet").await["production"], 1);
        assert_eq!(rollout(&store, "greet", "production").await, Some((2, 10)));
    }

    #[tokio::test]
    async fn abort_removes_the_rollout() {
        let (_dir, store) = seeded_with_rollout().await;
        for _ in 0..2 {
            // Aborting again is not an error: there is no rollout to remove.
            let (status, headers, _) = post_form(
                st(&store),
                "/prompts/greet/labels/production/rollout/delete",
                &format!("csrf={CSRF}"),
                Some(ORIGIN),
            )
            .await;
            assert_eq!(status, StatusCode::SEE_OTHER);
            assert_eq!(headers[LOCATION], "/prompts/greet");
            assert_eq!(rollout(&store, "greet", "production").await, None);
        }
        assert_eq!(labels(&store, "greet").await["production"], 1);
    }

    #[tokio::test]
    async fn error_pages_keep_the_rollout_column() {
        let (_dir, store) = seeded_with_rollout().await;
        for (path, form, want) in [
            (
                "/prompts/greet/labels/staging/rollout",
                rollout_body("1", "100"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "/prompts/greet/labels",
                set_body("staging", "1", "1"),
                StatusCode::CONFLICT,
            ),
        ] {
            let (status, _, html) = post_form(st(&store), path, &form, Some(ORIGIN)).await;
            assert_eq!(status, want, "{path}");
            assert!(html.contains(r#"<div class="banner error">"#), "{html}");
            assert!(
                html.contains(r#"<td class="num">v1</td><td>v2 at 10%</td>"#),
                "{path}: {html}"
            );
            assert!(
                html.contains(r#"<span class="chip">production 10%</span>"#),
                "{path}: {html}"
            );
        }
    }

    /// Loads from a `FileStore`; every label and rollout write fails with a large
    /// `StoreError::Http`.
    struct HttpFailStore {
        inner: Arc<FileStore>,
        body: String,
    }

    #[async_trait::async_trait]
    impl PromptStore for HttpFailStore {
        async fn load_all(&self) -> Result<gadk_prompts::PromptSnapshot, gadk_prompts::StoreError> {
            self.inner.load_all().await
        }
        async fn create_version(
            &self,
            new: NewVersion,
        ) -> Result<gadk_prompts::PromptVersion, gadk_prompts::StoreError> {
            self.inner.create_version(new).await
        }
        async fn set_label(
            &self,
            _name: &str,
            _label: &Label,
            _version: u32,
            _expected: Option<Option<u32>>,
        ) -> Result<(), gadk_prompts::StoreError> {
            Err(gadk_prompts::StoreError::Http {
                status: 500,
                body: self.body.clone(),
            })
        }
        async fn remove_label(
            &self,
            _name: &str,
            _label: &Label,
        ) -> Result<(), gadk_prompts::StoreError> {
            Err(gadk_prompts::StoreError::Http {
                status: 500,
                body: self.body.clone(),
            })
        }
        async fn load_all_with_rollouts(
            &self,
        ) -> Result<
            (gadk_prompts::PromptSnapshot, gadk_prompts::RolloutSnapshot),
            gadk_prompts::StoreError,
        > {
            self.inner.load_all_with_rollouts().await
        }
        async fn set_rollout(
            &self,
            _name: &str,
            _label: &Label,
            _rollout: Option<Rollout>,
        ) -> Result<(), gadk_prompts::StoreError> {
            Err(gadk_prompts::StoreError::Http {
                status: 500,
                body: self.body.clone(),
            })
        }
    }

    #[tokio::test]
    async fn store_http_error_banner_is_capped() {
        const MARKER: &str = "END-OF-UPSTREAM-BODY";
        let (_dir, inner) = seeded().await;
        let body = format!("{}{MARKER}", "x".repeat(2000));
        let store: Arc<dyn PromptStore> = Arc::new(HttpFailStore { inner, body });
        let state = state(Arc::new(FakeSource::new()), Some(store));
        for (path, form) in [
            ("/prompts/greet/labels", set_body("canary", "1", "")),
            (
                "/prompts/greet/labels/staging/delete",
                format!("csrf={CSRF}"),
            ),
            (
                "/prompts/greet/labels/staging/rollout",
                rollout_body("1", "10"),
            ),
            (
                "/prompts/greet/labels/production/rollout/promote",
                promote_body("2", "1", "10"),
            ),
            (
                "/prompts/greet/labels/production/rollout/delete",
                format!("csrf={CSRF}"),
            ),
        ] {
            let (status, _, html) = post_form(state.clone(), path, &form, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert!(!html.contains(MARKER), "{path}: {html}");
            assert!(html.contains("<h1>greet</h1>"), "{path}: the detail page");
            let start = html.find("<p>http 500: ").expect("http banner") + "<p>http 500: ".len();
            let len = html[start..].find("</p>").expect("banner end");
            assert!(len <= 1024, "{path}: banner body is {len} bytes");
            assert!(len > 0, "{path}");
        }
    }

    // ---- production metrics ----

    const PRICES: &str =
        "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

    fn bq_state(
        store: &Arc<FileStore>,
        fake: &Arc<FakeHttp>,
        reports: Option<Arc<ReportCache>>,
    ) -> AppState {
        let store: Arc<dyn PromptStore> = store.clone();
        let mut st = state_with_reports(Arc::new(FakeSource::new()), Some(store), reports);
        st.usage = Some(Arc::new(AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )));
        st.prices = Some(Arc::new(PriceTable::from_yaml(PRICES).unwrap()));
        st
    }

    fn stats_reply() -> Result<(u16, String), String> {
        reply(
            &[
                ("version", "INT64"),
                ("calls", "INT64"),
                ("mean_input", "FLOAT64"),
                ("mean_output", "FLOAT64"),
                ("p50_ms", "FLOAT64"),
                ("p95_ms", "FLOAT64"),
            ],
            vec![
                vec![
                    json!("2"),
                    json!("4"),
                    json!("1250"),
                    json!("300.5"),
                    json!("820"),
                    json!("1900"),
                ],
                vec![
                    json!("1"),
                    json!("2"),
                    json!("900"),
                    json!("200"),
                    json!("700"),
                    Value::Null,
                ],
            ],
            false,
        )
    }

    fn models_reply() -> Result<(u16, String), String> {
        reply(
            &[
                ("version", "INT64"),
                ("model", "STRING"),
                ("calls", "INT64"),
                ("input", "INT64"),
                ("cached", "INT64"),
                ("output", "INT64"),
                ("reasoning", "INT64"),
            ],
            vec![
                vec![
                    json!("2"),
                    json!("gemini-3.8-flash"),
                    json!("4"),
                    json!("5000"),
                    json!("1000"),
                    json!("1202"),
                    json!("0"),
                ],
                vec![
                    json!("1"),
                    json!("gemini-3.8-flash"),
                    json!("1"),
                    json!("900"),
                    json!("0"),
                    json!("200"),
                    json!("0"),
                ],
                vec![
                    json!("1"),
                    json!("other-model"),
                    json!("1"),
                    json!("900"),
                    json!("0"),
                    json!("200"),
                    json!("0"),
                ],
            ],
            false,
        )
    }

    fn ids_reply() -> Result<(u16, String), String> {
        reply(
            &[("invocation_id", "STRING"), ("version", "INT64")],
            vec![
                vec![json!("inv-0"), json!("2")],
                vec![json!("inv-1"), json!("2")],
                vec![json!("inv-2"), json!("1")],
            ],
            false,
        )
    }

    const FEEDBACK_FIELDS: [(&str, &str); 8] = [
        ("version", "INT64"),
        ("name", "STRING"),
        ("source", "STRING"),
        ("data_type", "STRING"),
        ("category", "STRING"),
        ("count", "INT64"),
        ("positive", "INT64"),
        ("total", "FLOAT64"),
    ];

    /// The feedback query's reply without rows.
    fn no_feedback_reply() -> Result<(u16, String), String> {
        reply(&FEEDBACK_FIELDS, vec![], false)
    }

    #[tokio::test]
    async fn production_metrics_show_calls_cost_latency_and_pass_rate_per_version() {
        let (_dir, store) = seeded().await;
        // One report, generated an hour before now, whose runs started in the 7d range:
        // inv-0 passes, inv-1 fails, inv-2 passes.
        let secs = fixed_now().timestamp() - 3_600;
        let r = report(
            "greet-monitor",
            secs,
            &[&[Outcome::Pass], &[Outcome::Fail], &[Outcome::Pass]],
        );
        let (_rdir, cache, _) = report_cache(&[r], 4).await;
        let fake = FakeHttp::new(vec![
            stats_reply(),
            models_reply(),
            ids_reply(),
            no_feedback_reply(),
        ]);
        let (status, _, body) = get(bq_state(&store, &fake, Some(cache)), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<h2>Production metrics</h2>"), "{body}");
        assert!(body.contains("<th>Eval pass rate</th>"), "{body}");
        assert!(
            body.contains(r#"<tr><td><a href="/prompts/greet?v=2">v2</a></td><td class="num">4</td><td class="num">1250.0</td><td class="num">300.5</td><td class="num">$0.001070</td><td class="num">820 ms</td><td class="num">1900 ms</td><td class="num">50.0% (1/2)</td></tr>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<tr><td><a href="/prompts/greet?v=1">v1</a></td><td class="num">2</td><td class="num">900.0</td><td class="num">200.0</td><td class="num">$0.000770*</td><td class="num">700 ms</td><td class="num">—</td><td class="num">100.0% (1/1)</td></tr>"#),
            "{body}"
        );
        assert!(
            body.contains("* Costs exclude calls to models without a price."),
            "{body}"
        );
        assert!(
            body.contains(r#"<option value="7d" selected>7d</option>"#),
            "{body}"
        );

        assert_eq!(fake.uris().len(), 4);
        let stats = fake.body(0);
        assert_eq!(
            param_value(&stats, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(
            param_value(&stats, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&stats, "end")["parameterValue"]["value"],
            json!("2026-11-16 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&fake.body(2), "ids")["parameterValue"],
            json!({ "arrayValues": [{ "value": "inv-0" }, { "value": "inv-1" }, { "value": "inv-2" }] })
        );
    }

    #[tokio::test]
    async fn production_metrics_take_the_range_and_skip_pass_rates_without_reports() {
        let (_dir, store) = seeded().await;
        let fake = FakeHttp::new(vec![stats_reply(), models_reply(), no_feedback_reply()]);
        let (status, _, body) =
            get(bq_state(&store, &fake, None), "/prompts/greet?range=24h").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("Eval pass rate"), "{body}");
        assert!(
            body.contains(r#"<td class="num">1900 ms</td></tr>"#),
            "{body}"
        );
        assert_eq!(
            fake.uris().len(),
            3,
            "stats, cost and feedback; no mapping query without --reports"
        );
        assert!(
            body.contains("No scores for calls of this prompt in the last 24h."),
            "{body}"
        );
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn production_metrics_failure_is_a_banner_with_the_hint() {
        let (_dir, store) = seeded().await;
        // The stats and cost queries run concurrently: both may be sent.
        let denied = || {
            Ok((
                403,
                json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
            ))
        };
        let fake = FakeHttp::new(vec![denied(), denied()]);
        let (status, _, body) = get(bq_state(&store, &fake, None), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK, "the rest of the page still renders");
        assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
        assert!(
            body.contains("needs roles/bigquery.dataViewer and roles/bigquery.jobUser"),
            "{body}"
        );
        assert!(body.contains("<h2>Labels</h2>"), "{body}");
    }

    #[tokio::test]
    async fn production_metrics_with_the_cloud_trace_source_are_a_note() {
        let (_dir, store) = seeded().await;
        let (status, _, body) = get(st(&store), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Production metrics need the BigQuery trace source"),
            "{body}"
        );
    }

    #[test]
    fn eval_ids_are_distinct_newest_first_and_capped() {
        let t = |s: i64| fixed_now() - chrono::TimeDelta::seconds(s);
        let runs = vec![
            ("a".to_owned(), t(1), true),
            ("b".to_owned(), t(2), false),
            ("a".to_owned(), t(3), true),
            ("c".to_owned(), t(4), true),
        ];
        assert_eq!(
            super::eval_ids(&runs, 5),
            (vec!["a".to_owned(), "b".to_owned(), "c".to_owned()], false)
        );
        assert_eq!(
            super::eval_ids(&runs, 2),
            (vec!["a".to_owned(), "b".to_owned()], true)
        );
    }

    #[test]
    fn pass_counts_count_each_run_for_every_version_it_used() {
        let now = fixed_now();
        let runs = vec![
            ("a".to_owned(), now, true),
            ("b".to_owned(), now, false),
            ("c".to_owned(), now, true),
        ];
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let mapping = vec![
            ("a".to_owned(), 1),
            ("a".to_owned(), 2),
            ("b".to_owned(), 2),
            ("c".to_owned(), 1),
        ];
        let counts = super::pass_counts(&runs, &ids, &mapping);
        assert_eq!(
            counts.into_iter().collect::<Vec<_>>(),
            vec![(1, (1, 1)), (2, (1, 2))],
            "c is past the id cap"
        );
    }

    #[tokio::test]
    async fn production_metrics_note_truncated_queries() {
        let (_dir, store) = seeded().await;
        let secs = fixed_now().timestamp() - 3_600;
        let r = report("greet-monitor", secs, &[&[Outcome::Pass]]);
        let (_rdir, cache, _) = report_cache(&[r], 4).await;
        let stats = reply(
            &[("version", "INT64"), ("calls", "INT64")],
            vec![vec![json!("2"), json!("4")]],
            true,
        );
        let ids = reply(
            &[("invocation_id", "STRING"), ("version", "INT64")],
            vec![vec![json!("inv-0"), json!("2")]],
            true,
        );
        let fake = FakeHttp::new(vec![stats, models_reply(), ids, no_feedback_reply()]);
        let (status, _, body) = get(bq_state(&store, &fake, Some(cache)), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(super::super::pages::prompt::METRICS_TRUNCATED),
            "{body}"
        );
        assert!(
            body.contains(super::super::pages::prompt::PASS_RATES_TRUNCATED),
            "{body}"
        );
        assert!(body.contains("100.0% (1/1)"), "{body}");
    }

    /// One row of `FEEDBACK_FIELDS`: version, name, source, data type, category, count,
    /// positive, total.
    fn feedback_cells(cells: [Value; 8]) -> Vec<Value> {
        cells.to_vec()
    }

    #[tokio::test]
    async fn production_metrics_show_feedback_per_version_name_and_source() {
        let (_dir, store) = seeded().await;
        let feedback = reply(
            &FEEDBACK_FIELDS,
            vec![
                feedback_cells([
                    json!("2"),
                    json!("quality"),
                    json!("app"),
                    json!("category"),
                    json!("helpful"),
                    json!("2"),
                    json!("0"),
                    Value::Null,
                ]),
                feedback_cells([
                    json!("2"),
                    json!("quality"),
                    json!("app"),
                    json!("category"),
                    json!("clear"),
                    json!("2"),
                    json!("0"),
                    Value::Null,
                ]),
                feedback_cells([
                    json!("2"),
                    json!("rating"),
                    json!("console"),
                    json!("number"),
                    Value::Null,
                    json!("2"),
                    json!("0"),
                    json!("7.5"),
                ]),
                feedback_cells([
                    json!("2"),
                    json!("thumbs"),
                    json!("app"),
                    json!("bool"),
                    Value::Null,
                    json!("3"),
                    json!("2"),
                    Value::Null,
                ]),
                feedback_cells([
                    json!("1"),
                    json!("thumbs"),
                    json!("app"),
                    json!("bool"),
                    Value::Null,
                    json!("1"),
                    json!("0"),
                    Value::Null,
                ]),
            ],
            true,
        );
        let fake = FakeHttp::new(vec![stats_reply(), models_reply(), feedback]);
        let (status, _, body) = get(bq_state(&store, &fake, None), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<h3>Feedback</h3>"), "{body}");
        for row in [
            r#"<tr><td><a href="/prompts/greet?v=2">v2</a></td><td>quality</td><td>app</td><td>category</td><td class="num">4</td><td>most common: clear</td></tr>"#,
            r#"<tr><td><a href="/prompts/greet?v=2">v2</a></td><td>rating</td><td>console</td><td>number</td><td class="num">2</td><td>mean 3.75</td></tr>"#,
            r#"<tr><td><a href="/prompts/greet?v=2">v2</a></td><td>thumbs</td><td>app</td><td>bool</td><td class="num">3</td><td>66.7% positive</td></tr>"#,
            r#"<tr><td><a href="/prompts/greet?v=1">v1</a></td><td>thumbs</td><td>app</td><td>bool</td><td class="num">1</td><td>0.0% positive</td></tr>"#,
        ] {
            assert!(body.contains(row), "{row}: {body}");
        }
        assert!(
            body.contains(super::super::pages::prompt::FEEDBACK_TRUNCATED),
            "{body}"
        );
        let f = fake.body(2);
        let sql = f["query"].as_str().unwrap();
        assert!(sql.contains("WHERE event_type = 'FEEDBACK'"), "{sql}");
        assert_eq!(
            param_value(&f, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(
            param_value(&f, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn a_failed_feedback_query_is_a_banner_under_the_metrics() {
        let (_dir, store) = seeded().await;
        let denied = Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ));
        let fake = FakeHttp::new(vec![stats_reply(), models_reply(), denied]);
        let (status, _, body) = get(bq_state(&store, &fake, None), "/prompts/greet").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("Feedback could not be loaded: BigQuery: HTTP 403: Access Denied"),
            "{body}"
        );
        assert!(
            body.contains(r#"<td class="num">1900 ms</td></tr>"#),
            "the metrics still show: {body}"
        );
    }
}
