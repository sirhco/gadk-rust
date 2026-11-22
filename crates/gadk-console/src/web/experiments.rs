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

//! Experiments routes: `/experiments`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use gadk_eval_monitor::{ExperimentReport, MonitorError};
use http::StatusCode;

use super::AppState;
use super::evals::{LIST_LIMIT, NO_REPORTS};
use super::pages::error::error_page;
use super::pages::experiments;
use super::pages::layout::{self, Nav};
use crate::experiments::ExperimentCache;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/experiments", get(list))
}

fn not_found(text: &str) -> Response {
    error_page(StatusCode::NOT_FOUND, Nav::Experiments, text, None)
}

fn bad_gateway(e: &MonitorError) -> Response {
    error_page(
        StatusCode::BAD_GATEWAY,
        Nav::Experiments,
        &e.to_string(),
        None,
    )
}

async fn list(State(state): State<AppState>) -> Response {
    let Some(cache) = &state.experiments else {
        return not_found(NO_REPORTS);
    };
    match load(cache).await {
        Ok(found) => layout::html(StatusCode::OK, experiments::render(&found)),
        Err(e) => bad_gateway(&e),
    }
}

/// The newest `LIST_LIMIT` experiments, newest first. A listed name that is gone by the time it
/// is read is skipped; any other read error fails the page.
async fn load(
    cache: &ExperimentCache,
) -> Result<Vec<(String, Arc<ExperimentReport>)>, MonitorError> {
    let names = cache.names(LIST_LIMIT).await?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(x) = cache.find(&name).await? {
            out.push((name, x));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;
    use gadk_eval_monitor::ExperimentReport;
    use http::StatusCode;
    use http::header::CONTENT_SECURITY_POLICY;

    use super::super::evals::{LIST_LIMIT, NO_REPORTS};
    use crate::evals::CACHE_CAP;
    use crate::web::AppState;
    use crate::web::guard::CSP;
    use crate::web::testutil::{
        FakeSource, experiment, experiment_cache, get, report, state, state_with_experiments,
    };

    async fn with(experiments: &[ExperimentReport]) -> (tempfile::TempDir, AppState, Vec<String>) {
        let (dir, cache, names) = experiment_cache(experiments, CACHE_CAP).await;
        (dir, state_with_experiments(cache), names)
    }

    /// One variant `v1` with one passing run of case `c1`.
    fn one(name: &str, secs: i64) -> ExperimentReport {
        experiment(name, secs, &[("v1", &[("c1", &[Outcome::Pass])])])
    }

    #[tokio::test]
    async fn without_a_report_store_the_list_is_404() {
        let (status, headers, body) =
            get(state(Arc::new(FakeSource::new()), None), "/experiments").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains(NO_REPORTS), "{body}");
        assert!(
            body.contains(r#"<a class="active" href="/experiments">Experiments</a>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_empty_store_lists_nothing() {
        let (_dir, st, _) = with(&[]).await;
        let (status, headers, body) = get(st, "/experiments").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("No experiments yet"), "{body}");
    }

    #[tokio::test]
    async fn the_list_shows_experiments_newest_first_and_no_monitor_reports() {
        let (dir, st, _) = with(&[
            one("a", 1_800_000_000),
            one("b", 1_800_003_600),
            one("a", 1_800_007_200),
        ])
        .await;
        let store =
            gadk_eval_monitor::ReportStore::open(&dir.path().display().to_string()).unwrap();
        store
            .put(&report("monitor", 1_800_000_000, &[&[Outcome::Pass]]))
            .await
            .unwrap();
        let (status, _, body) = get(st, "/experiments").await;
        assert_eq!(status, StatusCode::OK);
        let rows: Vec<usize> = [
            "a-20270115T100000Z.json",
            "b-20270115T090000Z.json",
            "a-20270115T080000Z.json",
        ]
        .iter()
        .map(|n| {
            body.find(&format!(r#"<a href="/experiments/{n}">"#))
                .expect(n)
        })
        .collect();
        assert!(rows[0] < rows[1] && rows[1] < rows[2], "{body}");
        assert!(!body.contains("monitor"), "{body}");
    }

    #[tokio::test]
    async fn the_list_is_capped_at_the_newest_100() {
        let all: Vec<ExperimentReport> = (0..LIST_LIMIT as i64 + 1)
            .map(|i| one("exp", 1_800_000_000 + i * 60))
            .collect();
        let (_dir, st, names) = with(&all).await;
        let (status, _, body) = get(st, "/experiments").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains(names[0].as_str()), "the 101st newest is cut");
        assert!(body.contains(names[1].as_str()), "{body}");
        assert_eq!(
            body.matches(r#"<td class="mono"><a href="/experiments/exp-"#)
                .count(),
            LIST_LIMIT
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_listed_experiment_that_is_gone_is_skipped() {
        let (dir, st, _) = with(&[one("a", 1_800_000_000)]).await;
        // A dangling link: listed by name, missing when read.
        let sub = dir.path().join("experiments");
        std::os::unix::fs::symlink(sub.join("nowhere"), sub.join("b-20270115T090000Z.json"))
            .unwrap();
        let (status, _, body) = get(st, "/experiments").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("/experiments/a-20270115T080000Z.json"),
            "{body}"
        );
        assert!(!body.contains("b-20270115T090000Z"), "{body}");
    }

    #[tokio::test]
    async fn a_corrupt_experiment_fails_the_list_with_502() {
        let (dir, st, _) = with(&[]).await;
        let sub = dir.path().join("experiments");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a-20270115T080000Z.json"), "{").unwrap();
        let (status, headers, body) = get(st, "/experiments").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("report store: "), "{body}");
        assert!(
            body.contains(r#"<a class="active" href="/experiments">Experiments</a>"#),
            "{body}"
        );
    }
}
