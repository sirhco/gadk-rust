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

//! Guards routes: `/guards` and `/guards/{report}`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use gadk_eval_monitor::{GuardReport, MonitorError, ReportStore};
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::evals::{LIST_LIMIT, NO_REPORTS};
use super::pages::error::error_page;
use super::pages::layout::{self, Nav};
use super::pages::{guard, guards};
use crate::guards::GuardCache;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/guards", get(list))
        .route("/guards/{report}", get(report))
}

/// `?prompt=<name>` shows only that prompt's rollouts; empty means no filter.
#[derive(Debug, Deserialize)]
struct ListQuery {
    prompt: Option<String>,
}

fn not_found(text: &str) -> Response {
    error_page(StatusCode::NOT_FOUND, Nav::Guards, text, None)
}

fn bad_request(text: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, Nav::Guards, text, None)
}

fn bad_gateway(e: &MonitorError) -> Response {
    error_page(StatusCode::BAD_GATEWAY, Nav::Guards, &e.to_string(), None)
}

async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let Some(cache) = &state.guards else {
        return not_found(NO_REPORTS);
    };
    let prompt = q.prompt.filter(|p| !p.is_empty());
    match load(cache).await {
        Ok(mut found) => {
            if let Some(prompt) = &prompt {
                found.retain(|(_, r)| &r.prompt == prompt);
            }
            layout::html(StatusCode::OK, guards::render(prompt.as_deref(), &found))
        }
        Err(e) => bad_gateway(&e),
    }
}

/// The newest `LIST_LIMIT` guard reports, newest first. A listed name that is gone by the time
/// it is read is skipped; any other read error fails the page.
async fn load(cache: &GuardCache) -> Result<Vec<(String, Arc<GuardReport>)>, MonitorError> {
    let names = cache.names(LIST_LIMIT).await?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(r) = cache.find(&name).await? {
            out.push((name, r));
        }
    }
    Ok(out)
}

async fn report(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(cache) = &state.guards else {
        return not_found(NO_REPORTS);
    };
    if !ReportStore::is_report_name(&name) {
        return bad_request("invalid guard report name");
    }
    match cache.find(&name).await {
        Ok(Some(r)) => layout::html(StatusCode::OK, guard::render(&name, &r)),
        Ok(None) => not_found("guard report not found"),
        Err(e) => bad_gateway(&e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;
    use gadk_eval_monitor::{Action, GuardReport, ReportStore};
    use http::StatusCode;
    use http::header::CONTENT_SECURITY_POLICY;

    use super::super::evals::{LIST_LIMIT, NO_REPORTS};
    use crate::evals::CACHE_CAP;
    use crate::web::AppState;
    use crate::web::guard::CSP;
    use crate::web::testutil::{
        FakeSource, get, guard, guard_cache, report, state, state_with_guards,
    };

    const ACTIVE: &str = r#"<a class="active" href="/guards">Guards</a>"#;

    async fn with(guards: &[GuardReport]) -> (tempfile::TempDir, AppState, Vec<String>) {
        let (dir, cache, names) = guard_cache(guards, CACHE_CAP).await;
        (dir, state_with_guards(cache), names)
    }

    #[tokio::test]
    async fn without_a_report_store_the_guard_pages_are_404() {
        for path in [
            "/guards",
            "/guards?prompt=a",
            "/guards/g-20270115T080000Z.json",
            "/guards/x",
        ] {
            let (status, headers, body) = get(state(Arc::new(FakeSource::new()), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
            assert!(body.contains(NO_REPORTS), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
    }

    #[tokio::test]
    async fn an_empty_store_lists_nothing() {
        let (_dir, st, _) = with(&[]).await;
        let (status, headers, body) = get(st, "/guards").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("No guard runs yet"), "{body}");
        assert!(body.contains(ACTIVE), "{body}");
    }

    #[tokio::test]
    async fn the_list_shows_rollouts_and_reports_and_no_other_reports() {
        let (dir, st, names) = with(&[
            guard("ga", 1_800_000_000, "a", 50, Action::Ramp(75), false),
            guard("ga", 1_800_003_600, "a", 50, Action::Ramp(75), true),
            guard("gb", 1_800_007_200, "b", 10, Action::Abort, true),
        ])
        .await;
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        let monitor_name = store
            .put(&report("monitor", 1_800_000_000, &[&[Outcome::Pass]]))
            .await
            .unwrap();
        let (status, headers, body) = get(st, "/guards").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("<h1>Guards</h1>"), "{body}");
        let b_card = body.find(">b/production v1 → v2</a></h2>").expect(&body);
        let a_card = body.find(">a/production v1 → v2</a></h2>").expect(&body);
        assert!(b_card < a_card, "{body}");
        assert!(body.contains("2 reports, 1 applied"), "{body}");
        assert!(
            body.contains("<p class=\"meta\">50% → ramped 75%</p>"),
            "{body}"
        );
        assert!(body.contains("1 report, 1 applied"), "{body}");
        let rows: Vec<usize> = names
            .iter()
            .rev()
            .map(|n| {
                body.find(&format!(r#"<tr><td class="mono"><a href="/guards/{n}">"#))
                    .expect(n)
            })
            .collect();
        assert!(rows.windows(2).all(|w| w[0] < w[1]), "{body}");
        assert!(!body.contains(&monitor_name), "{body}");
    }

    #[tokio::test]
    async fn a_prompt_filter_lists_only_that_prompt() {
        let (_dir, st, _) = with(&[
            guard("ga", 1_800_000_000, "a", 50, Action::Ramp(75), false),
            guard("gb", 1_800_003_600, "b", 10, Action::Abort, true),
        ])
        .await;
        let (status, headers, body) = get(st.clone(), "/guards?prompt=a").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("<h1>Guards: a</h1>"), "{body}");
        assert!(
            body.contains(r#"<a href="/guards">all guards</a>"#),
            "{body}"
        );
        assert!(body.contains(">a/production v1 → v2</a></h2>"), "{body}");
        assert!(body.contains("ga-20270115T080000Z.json"), "{body}");
        assert!(!body.contains("b/production"), "{body}");
        assert!(!body.contains("gb-20270115T090000Z.json"), "{body}");

        let (status, _, body) = get(st.clone(), "/guards?prompt=zzz").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h1>Guards: zzz</h1>"), "{body}");
        assert!(body.contains("No guard runs yet"), "{body}");

        // An empty filter is no filter.
        let (_, _, body) = get(st, "/guards?prompt=").await;
        assert!(body.contains("<h1>Guards</h1>"), "{body}");
        assert!(body.contains("b/production"), "{body}");
    }

    #[tokio::test]
    async fn the_list_is_capped_at_the_newest_100() {
        let all: Vec<GuardReport> = (0..LIST_LIMIT as i64 + 1)
            .map(|i| {
                guard(
                    "g",
                    1_800_000_000 + i * 60,
                    "a",
                    10,
                    Action::Ramp(25),
                    false,
                )
            })
            .collect();
        let (_dir, st, names) = with(&all).await;
        let (status, _, body) = get(st, "/guards").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains(names[0].as_str()), "the 101st newest is cut");
        assert!(body.contains(names[1].as_str()), "{body}");
        assert_eq!(
            body.matches(r#"<tr><td class="mono"><a href="/guards/g-"#)
                .count(),
            LIST_LIMIT
        );
        assert!(body.contains("100 reports, 0 applied"), "{body}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_listed_guard_report_that_is_gone_is_skipped() {
        let (dir, st, _) =
            with(&[guard("g", 1_800_000_000, "a", 10, Action::Ramp(25), false)]).await;
        // A dangling link: listed by name, missing when read.
        let sub = dir.path().join("guards");
        std::os::unix::fs::symlink(sub.join("nowhere"), sub.join("h-20270115T090000Z.json"))
            .unwrap();
        let (status, _, body) = get(st, "/guards").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("/guards/g-20270115T080000Z.json"), "{body}");
        assert!(!body.contains("h-20270115T090000Z"), "{body}");
    }

    #[tokio::test]
    async fn the_guard_page_shows_the_report() {
        let (_dir, st, names) = with(&[guard(
            "g",
            1_800_000_000,
            "support",
            50,
            Action::Abort,
            true,
        )])
        .await;
        assert_eq!(names, ["g-20270115T080000Z.json"]);
        let (status, headers, body) = get(st, "/guards/g-20270115T080000Z.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>guard g</h1>"), "{body}");
        assert!(body.contains("support/production v1 → v2 (50%)"), "{body}");
        assert!(
            body.contains(r#"<span class="chip status-worse">worse</span>: rollout errors"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<p>action: aborted <span class="chip">applied</span></p>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn invalid_names_are_400_and_unknown_reports_404() {
        let (_dir, st, _) = with(&[guard(
            "g",
            1_800_000_000,
            "support",
            50,
            Action::Abort,
            true,
        )])
        .await;
        for path in ["/guards/..%2Fx", "/guards/x", "/guards/g.json"] {
            let (status, headers, body) = get(st.clone(), path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
            assert!(body.contains("invalid guard report name"), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
        let (status, headers, body) = get(st, "/guards/g-20270115T090000Z.json").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("guard report not found"), "{body}");
        assert!(body.contains(ACTIVE), "{body}");
    }

    #[tokio::test]
    async fn a_corrupt_guard_report_fails_its_page_and_the_list_with_502() {
        let (dir, st, _) = with(&[]).await;
        let sub = dir.path().join("guards");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("g-20270115T080000Z.json"), "{").unwrap();
        for path in ["/guards/g-20270115T080000Z.json", "/guards"] {
            let (status, headers, body) = get(st.clone(), path).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
            assert!(body.contains("report store: "), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
    }
}
