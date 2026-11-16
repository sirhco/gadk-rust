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

pub(crate) mod guard;
pub(crate) mod pages;
mod prompts;
#[cfg(test)]
pub(crate) mod testutil;
mod traces;

#[derive(Clone)]
pub(crate) struct AppState {
    pub source: Arc<dyn TraceSource>,
    pub store: Option<Arc<dyn gadk_prompts::PromptStore>>,
    pub csrf: Arc<str>,                             // 64 hex chars
    pub port: u16,                                  // the bound port
    pub now: fn() -> chrono::DateTime<chrono::Utc>, // Utc::now in production; fixed in tests
}

const CONSOLE_CSS: &str = include_str!("assets/console.css");
const WATERFALL_JS: &str = include_str!("assets/waterfall.js");

/// GET / → 303 /traces; GET /assets/console.css and /assets/waterfall.js (include_str!, with
/// text/css and text/javascript; charset=utf-8 content types); merges traces::routes() and
/// prompts::routes(); wraps everything in guard::layer (Host 421, POST Origin 403) and adds the
/// R11 CSP header to every response; unknown paths → error page 404.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/assets/console.css", get(console_css))
        .route("/assets/waterfall.js", get(waterfall_js))
        .merge(traces::routes())
        .merge(prompts::routes())
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
    use http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, LOCATION};
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
        }
        let req = Request::get("/")
            .header(HOST, "other:1")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = send(fresh(), req).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(headers[CONTENT_SECURITY_POLICY], expected);
    }
}
