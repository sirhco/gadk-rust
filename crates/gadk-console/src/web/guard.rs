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

//! Request guards (R11): the Host and Origin checks, the CSRF token check and the security
//! response headers.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, HOST, ORIGIN, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderValue, Method, StatusCode};

use super::AppState;
use super::pages::error::error_page;
use super::pages::layout::Nav;

/// The R11 Content-Security-Policy, sent on every response.
pub(crate) const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; form-action 'self'";

/// Loopback hosts the console answers to.
const LOOPBACK: [&str; 2] = ["127.0.0.1", "localhost"];

/// True when `value` is `{prefix}{host}:{port}` for a loopback host, or, on port 80 only,
/// the port-less `{prefix}{host}` that browsers send for the default port.
fn loopback_match(value: &HeaderValue, prefix: &str, port: u16) -> bool {
    LOOPBACK.iter().any(|host| {
        value == format!("{prefix}{host}:{port}").as_str()
            || (port == 80 && value == format!("{prefix}{host}").as_str())
    })
}

/// axum middleware (from_fn_with_state): Host must be "127.0.0.1:{port}" or "localhost:{port}"
/// → else 421; for POST, Origin must be "http://127.0.0.1:{port}" or "http://localhost:{port}"
/// → else 403. On port 80 the port-less forms are accepted as well.
pub(crate) async fn check(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let port = state.port;
    let host_ok = req
        .headers()
        .get(HOST)
        .is_some_and(|h| loopback_match(h, "", port));
    if !host_ok {
        return error_page(
            StatusCode::MISDIRECTED_REQUEST,
            Nav::Traces,
            "misdirected request: this console answers only on its loopback address",
            None,
        );
    }
    if req.method() == Method::POST {
        let origin_ok = req
            .headers()
            .get(ORIGIN)
            .is_some_and(|o| loopback_match(o, "http://", port));
        if !origin_ok {
            return error_page(
                StatusCode::FORBIDDEN,
                Nav::Traces,
                "forbidden: cross-origin request",
                None,
            );
        }
    }
    next.run(req).await
}

/// axum middleware (map_response): sets the R11 CSP header, `X-Content-Type-Options: nosniff`
/// and `Cache-Control: no-store` on every response, guard rejections and assets included.
pub(crate) async fn csp(mut resp: Response) -> Response {
    let headers = resp.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Constant-time compare of a submitted csrf token with state.csrf; Err is a 403 error page.
#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
pub(crate) fn check_csrf(state: &AppState, token: &str) -> Result<(), Response> {
    let want = state.csrf.as_bytes();
    let got = token.as_bytes();
    let diff = want
        .iter()
        .zip(got)
        .fold(u8::from(want.len() != got.len()), |acc, (a, b)| {
            acc | (a ^ b)
        });
    if diff == 0 {
        Ok(())
    } else {
        Err(error_page(
            StatusCode::FORBIDDEN,
            Nav::Prompts,
            "forbidden: missing or invalid csrf token",
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::StatusCode;

    use super::check_csrf;
    use crate::web::testutil::{CSRF, FakeSource, state};

    #[test]
    fn csrf_accepts_only_the_process_token() {
        let st = state(Arc::new(FakeSource::new()), None);
        assert!(check_csrf(&st, CSRF).is_ok());

        let wrong = "b".repeat(64);
        let err = check_csrf(&st, &wrong).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);

        let err = check_csrf(&st, "").unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);

        let prefix = &CSRF[..63];
        assert!(check_csrf(&st, prefix).is_err());
    }
}
