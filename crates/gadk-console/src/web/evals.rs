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

//! Evals routes: `/evals` and `/evals/{name}`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use gadk_eval_monitor::{MonitorError, MonitorReport, ReportStore};
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::pages::error::error_page;
use super::pages::layout::{self, Nav};
use super::pages::{eval_report, evals};
use crate::evals::{ReportCache, spec_of};

/// The 404 text of every Evals route when the console has no report store.
pub(crate) const NO_REPORTS: &str = "no report store configured (start the console with --reports)";

/// How many reports `/evals` shows.
pub(crate) const LIST_LIMIT: usize = 100;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/evals", get(list))
        .route("/evals/{name}", get(report))
}

/// `?spec=`; an empty value counts as absent.
#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    spec: Option<String>,
}

fn not_found(text: &str) -> Response {
    error_page(StatusCode::NOT_FOUND, Nav::Evals, text, None)
}

fn bad_gateway(e: &MonitorError) -> Response {
    error_page(StatusCode::BAD_GATEWAY, Nav::Evals, &e.to_string(), None)
}

async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let Some(cache) = &state.reports else {
        return not_found(NO_REPORTS);
    };
    let spec = q.spec.filter(|s| !s.is_empty());
    match load(cache, spec.as_deref()).await {
        Ok(reports) => layout::html(StatusCode::OK, evals::render(spec.as_deref(), &reports)),
        Err(e) => bad_gateway(&e),
    }
}

/// The reports `/evals` shows, newest first: without `spec` the newest `LIST_LIMIT`; with it the
/// newest `LIST_LIMIT` whose name's spec part is `spec`. A listed name that is gone by the time
/// it is read is skipped; any other read error fails the page.
async fn load(
    cache: &ReportCache,
    spec: Option<&str>,
) -> Result<Vec<(String, Arc<MonitorReport>)>, MonitorError> {
    let names = match spec {
        None => cache.names(LIST_LIMIT).await?,
        Some(spec) => cache
            .names(usize::MAX)
            .await?
            .into_iter()
            .filter(|n| spec_of(n) == Some(spec))
            .take(LIST_LIMIT)
            .collect(),
    };
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(report) = cache.find(&name).await? {
            out.push((name, report));
        }
    }
    Ok(out)
}

async fn report(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(cache) = &state.reports else {
        return not_found(NO_REPORTS);
    };
    if !ReportStore::is_report_name(&name) {
        return not_found("report not found");
    }
    match cache.find(&name).await {
        Ok(Some(r)) => layout::html(StatusCode::OK, eval_report::render(&name, &r)),
        Ok(None) => not_found("report not found"),
        Err(e) => bad_gateway(&e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;
    use gadk_eval_monitor::MonitorReport;
    use http::StatusCode;
    use http::header::CONTENT_SECURITY_POLICY;

    use super::NO_REPORTS;
    use crate::evals::CACHE_CAP;
    use crate::web::AppState;
    use crate::web::guard::CSP;
    use crate::web::testutil::{FakeSource, get, report, report_cache, state, state_with_reports};

    async fn with(
        reports: &[MonitorReport],
        cap: usize,
    ) -> (tempfile::TempDir, AppState, Vec<String>) {
        let (dir, cache, names) = report_cache(reports, cap).await;
        (
            dir,
            state_with_reports(Arc::new(FakeSource::new()), None, Some(cache)),
            names,
        )
    }

    #[tokio::test]
    async fn without_a_report_store_every_evals_route_is_404() {
        for path in [
            "/evals",
            "/evals?spec=a",
            "/evals/a-20270115T080000Z.json",
            "/evals/x",
        ] {
            let (status, headers, body) = get(state(Arc::new(FakeSource::new()), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
            assert!(body.contains(NO_REPORTS), "{path}: {body}");
            assert!(
                body.contains(r#"<a class="active" href="/evals">Evals</a>"#),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_store_lists_nothing() {
        let (_dir, st, _) = with(&[], CACHE_CAP).await;
        let (status, _, body) = get(st, "/evals").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("No reports yet"), "{body}");
    }

    #[tokio::test]
    async fn the_list_shows_reports_newest_first() {
        let (_dir, st, _) = with(
            &[
                report("a", 1_800_000_000, &[&[Outcome::Pass]]),
                report("b", 1_800_003_600, &[&[Outcome::Fail]]),
                report("a", 1_800_007_200, &[&[Outcome::Pass]]),
            ],
            CACHE_CAP,
        )
        .await;
        let (status, headers, body) = get(st, "/evals").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        let rows: Vec<usize> = [
            "a-20270115T100000Z.json",
            "b-20270115T090000Z.json",
            "a-20270115T080000Z.json",
        ]
        .iter()
        .map(|n| body.rfind(&format!("/evals/{n}")).expect(n))
        .collect();
        assert!(rows[0] < rows[1] && rows[1] < rows[2], "{body}");
        assert_eq!(
            body.matches(r#"<div class="eval-card">"#).count(),
            2,
            "{body}"
        );
    }

    #[tokio::test]
    async fn the_list_is_capped_and_a_spec_filter_finds_older_reports() {
        let mut reports = vec![report("old", 1_700_000_000, &[&[Outcome::Pass]])];
        for i in 0..super::LIST_LIMIT as i64 + 1 {
            reports.push(report("new", 1_800_000_000 + i * 60, &[&[Outcome::Pass]]));
        }
        let (_dir, st, names) = with(&reports, CACHE_CAP).await;
        let (old, oldest_new) = (&names[0], &names[1]);

        let (_, _, body) = get(st.clone(), "/evals").await;
        assert!(!body.contains(old.as_str()), "old is past the newest 100");
        assert!(
            !body.contains(oldest_new.as_str()),
            "the 101st newest is cut"
        );
        assert_eq!(
            body.matches(r#"<td class="mono"><a href="/evals/new-"#)
                .count(),
            super::LIST_LIMIT
        );

        let (status, _, body) = get(st, "/evals?spec=old").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&format!("/evals/{old}")), "{body}");
        assert!(!body.contains("/evals/new-"), "{body}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_listed_report_that_is_gone_is_skipped() {
        let (dir, st, _) = with(
            &[report("a", 1_800_000_000, &[&[Outcome::Pass]])],
            CACHE_CAP,
        )
        .await;
        // A dangling link: listed by name, missing when read.
        std::os::unix::fs::symlink(
            dir.path().join("nowhere"),
            dir.path().join("b-20270115T090000Z.json"),
        )
        .unwrap();
        let (status, _, body) = get(st, "/evals").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("/evals/a-20270115T080000Z.json"), "{body}");
        assert!(!body.contains("b-20270115T090000Z"), "{body}");
    }

    #[tokio::test]
    async fn a_corrupt_report_fails_the_list_and_its_page_with_502() {
        let (dir, st, _) = with(&[], CACHE_CAP).await;
        std::fs::write(dir.path().join("a-20270115T080000Z.json"), "{").unwrap();
        let (status, _, body) = get(st.clone(), "/evals").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            body.contains("report store: report a-20270115T080000Z.json: "),
            "{body}"
        );
        let (status, _, _) = get(st, "/evals/a-20270115T080000Z.json").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn the_report_page_and_its_404s() {
        let (_dir, st, names) = with(
            &[report("a", 1_800_000_000, &[&[Outcome::Pass]])],
            CACHE_CAP,
        )
        .await;
        let (status, _, body) = get(st.clone(), &format!("/evals/{}", names[0])).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#"<details class="eval-run">"#), "{body}");
        for path in [
            "/evals/a-20270115T090000Z.json",
            "/evals/x",
            "/evals/a.json",
            "/evals/%2E%2E%2Fetc",
        ] {
            let (status, _, body) = get(st.clone(), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(body.contains("report not found"), "{path}: {body}");
        }
    }

    #[tokio::test]
    async fn reports_are_read_once_and_evicted_at_the_cap() {
        let (dir, st, names) = with(
            &[
                report("a", 1_800_000_000, &[]),
                report("b", 1_800_000_000, &[]),
            ],
            1,
        )
        .await;
        let path = |n: &String| format!("/evals/{n}");
        assert_eq!(get(st.clone(), &path(&names[0])).await.0, StatusCode::OK);
        std::fs::remove_file(dir.path().join(&names[0])).unwrap();
        assert_eq!(
            get(st.clone(), &path(&names[0])).await.0,
            StatusCode::OK,
            "served from the cache"
        );
        assert_eq!(get(st.clone(), &path(&names[1])).await.0, StatusCode::OK);
        assert_eq!(
            get(st, &path(&names[0])).await.0,
            StatusCode::NOT_FOUND,
            "evicted, then gone"
        );
    }
}
