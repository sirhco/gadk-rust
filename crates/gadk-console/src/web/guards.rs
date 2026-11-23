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

//! Guards routes: `/guards/{report}`.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use gadk_eval_monitor::{MonitorError, ReportStore};
use http::StatusCode;

use super::AppState;
use super::evals::NO_REPORTS;
use super::pages::error::error_page;
use super::pages::guard;
use super::pages::layout::{self, Nav};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/guards/{report}", get(report))
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

    use gadk_eval_monitor::{Action, GuardReport};
    use http::StatusCode;
    use http::header::CONTENT_SECURITY_POLICY;

    use super::super::evals::NO_REPORTS;
    use crate::evals::CACHE_CAP;
    use crate::web::AppState;
    use crate::web::guard::CSP;
    use crate::web::testutil::{FakeSource, get, guard, guard_cache, state, state_with_guards};

    const ACTIVE: &str = r#"<a class="active" href="/guards">Guards</a>"#;

    async fn with(guards: &[GuardReport]) -> (tempfile::TempDir, AppState, Vec<String>) {
        let (dir, cache, names) = guard_cache(guards, CACHE_CAP).await;
        (dir, state_with_guards(cache), names)
    }

    #[tokio::test]
    async fn without_a_report_store_the_guard_pages_are_404() {
        for path in ["/guards/g-20270115T080000Z.json", "/guards/x"] {
            let (status, headers, body) = get(state(Arc::new(FakeSource::new()), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
            assert!(body.contains(NO_REPORTS), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
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
    async fn a_corrupt_guard_report_is_502() {
        let (dir, st, _) = with(&[]).await;
        let sub = dir.path().join("guards");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("g-20270115T080000Z.json"), "{").unwrap();
        let (status, headers, body) = get(st, "/guards/g-20270115T080000Z.json").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains("report store: "), "{body}");
        assert!(body.contains(ACTIVE), "{body}");
    }
}
