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

//! The HTTP layer: router, request guards and shared state.

use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use http::StatusCode;
use http::header::CONTENT_TYPE;

use crate::traces::TraceSource;

mod evals;
mod experiments;
pub(crate) mod guard;
mod guards;
pub(crate) mod pages;
mod prompts;
mod review;
#[cfg(test)]
pub(crate) mod testutil;
mod traces;
mod usage;

#[derive(Clone)]
pub(crate) struct AppState {
    pub source: Arc<dyn TraceSource>,
    pub store: Option<Arc<dyn gadk_prompts::PromptStore>>,
    pub reports: Option<Arc<crate::evals::ReportCache>>,
    pub experiments: Option<Arc<crate::experiments::ExperimentCache>>, // Some with --reports
    pub guards: Option<Arc<crate::guards::GuardCache>>,                // Some with --reports
    pub csrf: Arc<str>,                                                // 64 hex chars
    pub port: u16,                                                     // the bound port
    pub now: fn() -> chrono::DateTime<chrono::Utc>, // Utc::now in production; fixed in tests
    pub prices: Option<Arc<crate::prices::PriceTable>>, // the --prices file, if any
    pub usage: Option<Arc<crate::usage::UsageSource>>, // Some with --traces bigquery://… only
    pub author: String,                             // the author of console scores
    pub datasets: Option<Arc<crate::datasets::Datasets>>, // Some with --datasets and BigQuery
    // Some with BigQuery: the conversation text of `/sessions/{id}`, or why it is unavailable.
    #[allow(dead_code)] // read by the session page
    pub export: Option<Result<Arc<dyn gadk_eval_monitor::ExportSource>, String>>,
}

const CONSOLE_CSS: &str = include_str!("assets/console.css");
const WATERFALL_JS: &str = include_str!("assets/waterfall.js");

/// GET / → 303 /traces; GET /assets/console.css and /assets/waterfall.js (include_str!, with
/// text/css and text/javascript; charset=utf-8 content types); merges traces::routes(),
/// prompts::routes(), evals::routes(), experiments::routes(), guards::routes(),
/// review::routes() and usage::routes(); wraps everything in guard::layer (Host 421, POST
/// Origin 403) and adds the CSP header to every response; unknown paths → error page 404.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/assets/console.css", get(console_css))
        .route("/assets/waterfall.js", get(waterfall_js))
        .merge(traces::routes())
        .merge(prompts::routes())
        .merge(evals::routes())
        .merge(experiments::routes())
        .merge(guards::routes())
        .merge(review::routes())
        .merge(usage::routes())
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), guard::check))
        .layer(middleware::map_response(guard::csp))
        .with_state(state)
}

async fn root() -> Redirect {
    Redirect::to("/traces")
}

async fn console_css() -> Response {
    ([(CONTENT_TYPE, "text/css; charset=utf-8")], CONSOLE_CSS).into_response()
}

async fn waterfall_js() -> Response {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        WATERFALL_JS,
    )
        .into_response()
}

async fn not_found() -> Response {
    pages::error::error_page(
        StatusCode::NOT_FOUND,
        pages::layout::Nav::Traces,
        "page not found",
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use http::header::{
        CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, LOCATION, ORIGIN,
        X_CONTENT_TYPE_OPTIONS,
    };
    use http::{Request, StatusCode};

    use super::guard::CSP;
    use super::testutil::{FakeSource, PORT, get, post_form, send, state};

    fn fresh() -> super::AppState {
        state(Arc::new(FakeSource::new()), None)
    }

    #[tokio::test]
    async fn root_redirects_to_traces() {
        let (status, headers, _) = get(fresh(), "/").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/traces");
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
    }

    #[tokio::test]
    async fn assets_have_fixed_content_types() {
        let (status, headers, body) = get(fresh(), "/assets/console.css").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/css; charset=utf-8");
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.contains(".span-row"), "{body}");
        assert!(!body.contains("http"), "no external URLs in the stylesheet");

        let (status, headers, _) = get(fresh(), "/assets/waterfall.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/javascript; charset=utf-8");
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
    }

    #[tokio::test]
    async fn unknown_path_is_a_404_page() {
        let (status, headers, body) = get(fresh(), "/nope/%3Cscript%3E").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
        assert!(body.contains("page not found"), "{body}");
        assert!(!body.contains("nope"), "the path is not echoed: {body}");
    }

    #[tokio::test]
    async fn wrong_host_is_421_and_localhost_is_accepted() {
        let req = Request::get("/")
            .header(HOST, "evil.example:8765")
            .body(Body::empty())
            .unwrap();
        let (status, headers, body) = send(fresh(), req).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
        assert!(!body.contains("evil.example"), "{body}");

        let req = Request::get("/")
            .header(HOST, "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(fresh(), req).await.0, StatusCode::MISDIRECTED_REQUEST);

        let req = Request::get("/").body(Body::empty()).unwrap();
        assert_eq!(send(fresh(), req).await.0, StatusCode::MISDIRECTED_REQUEST);

        for host in [
            format!("LOCALHOST:{PORT}"),
            format!("localhost.:{PORT}"),
            format!("127.0.0.1:{PORT}.evil"),
        ] {
            let req = Request::get("/")
                .header(HOST, host.as_str())
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                send(fresh(), req).await.0,
                StatusCode::MISDIRECTED_REQUEST,
                "{host}"
            );
        }

        let req = Request::get("/")
            .header(HOST, format!("localhost:{PORT}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(fresh(), req).await.0, StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn post_needs_a_loopback_origin() {
        let (status, headers, _) = post_form(fresh(), "/", "", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);

        let (status, _, _) = post_form(fresh(), "/", "", Some("http://evil.example")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        for origin in [
            "null".to_owned(),
            format!("http://127.0.0.1:{PORT}/"),
            format!("https://127.0.0.1:{PORT}"),
        ] {
            let (status, _, _) = post_form(fresh(), "/", "", Some(&origin)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        }

        let (status, _, _) = post_form(fresh(), "/", "", Some("http://127.0.0.1:9999")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Past the guard: routing answers (no POST routes yet), not the guard.
        let ok = format!("http://127.0.0.1:{PORT}");
        let (status, _, _) = post_form(fresh(), "/", "", Some(&ok)).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let ok = format!("http://localhost:{PORT}");
        let (status, _, _) = post_form(fresh(), "/nope", "", Some(&ok)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn csp_is_exact_on_every_status() {
        let expected = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; form-action 'self'";
        assert_eq!(CSP, expected);
        for path in ["/", "/assets/console.css", "/missing"] {
            let (_, headers, _) = get(fresh(), path).await;
            assert_eq!(headers.get_all(CONTENT_SECURITY_POLICY).iter().count(), 1);
            assert_eq!(headers[CONTENT_SECURITY_POLICY], expected, "{path}");
            assert_eq!(headers[X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
            assert_eq!(headers[CACHE_CONTROL], "no-store", "{path}");
        }
        let req = Request::get("/")
            .header(HOST, "other:1")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = send(fresh(), req).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], expected);
        assert_eq!(headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[CACHE_CONTROL], "no-store");
    }

    fn on_port(port: u16) -> super::AppState {
        super::AppState { port, ..fresh() }
    }

    fn get_with_host(host: &str) -> Request<Body> {
        Request::get("/")
            .header(HOST, host)
            .body(Body::empty())
            .unwrap()
    }

    fn post_with(host: &str, origin: &str) -> Request<Body> {
        Request::post("/")
            .header(HOST, host)
            .header(ORIGIN, origin)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn port_80_accepts_port_less_host_and_origin() {
        for host in ["127.0.0.1", "localhost", "127.0.0.1:80", "localhost:80"] {
            let (status, _, _) = send(on_port(80), get_with_host(host)).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{host}");
        }
        let (status, _, _) = send(on_port(80), get_with_host("127.0.0.1:8765")).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);

        for origin in [
            "http://127.0.0.1",
            "http://localhost",
            "http://127.0.0.1:80",
        ] {
            // Past the guard: routing answers (no POST route at /).
            let (status, _, _) = send(on_port(80), post_with("127.0.0.1", origin)).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{origin}");
        }
        let (status, _, _) = send(on_port(80), post_with("127.0.0.1", "http://evil")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn port_less_host_and_origin_are_rejected_off_port_80() {
        for host in ["127.0.0.1", "localhost"] {
            let (status, _, _) = send(fresh(), get_with_host(host)).await;
            assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}");
        }
        let host = format!("127.0.0.1:{PORT}");
        for origin in ["http://127.0.0.1", "http://localhost"] {
            let (status, _, _) = send(fresh(), post_with(&host, origin)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        }
    }

    #[tokio::test]
    async fn state_with_reports_carries_the_cache() {
        let (_dir, cache, _) = super::testutil::report_cache(&[], 4).await;
        let st =
            super::testutil::state_with_reports(Arc::new(FakeSource::new()), None, Some(cache));
        assert!(st.reports.is_some());
        assert!(fresh().reports.is_none());
    }

    #[test]
    fn test_states_have_no_export_source() {
        assert!(fresh().export.is_none());
    }

    #[tokio::test]
    async fn state_with_guards_carries_the_cache() {
        let (_dir, cache, _) = super::testutil::guard_cache(&[], 4).await;
        let st = super::testutil::state_with_guards(cache);
        assert!(st.guards.is_some());
        assert!(st.reports.is_none() && st.experiments.is_none());
        assert!(fresh().guards.is_none());
    }
}
