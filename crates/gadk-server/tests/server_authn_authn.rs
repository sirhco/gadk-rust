// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Ports of ADK-Go `server/authn/authn_test.go`. Go's request context carrying a `*Caller`
//! (`WithCaller` / `CallerFromContext`) is the request's [`Caller`] extension in gadk, which
//! [`middleware`] inserts.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use gadk_server::authn::{Authenticator, AuthnError, Caller, custom, header, middleware, noop};
use http::{Request, StatusCode};
use pretty_assertions::assert_eq;
use tower::{Layer as _, ServiceExt as _};

/// Go `userIDFromContext`: the user ID of the request's caller, empty when it has none.
fn user_id_from_extensions(ext: &http::Extensions) -> String {
    ext.get::<Caller>()
        .map(|c| c.user_id.clone())
        .unwrap_or_default()
}

/// The response of `Middleware(auth)(next)` to `req`, where `next` echoes the caller's user ID
/// (Go `echoUserID`) and sets `called`.
async fn serve(
    auth: Arc<dyn Authenticator>,
    req: Request<Body>,
    called: Arc<AtomicBool>,
) -> (StatusCode, String) {
    let next = tower::service_fn(move |req: Request<Body>| {
        let called = called.clone();
        async move {
            called.store(true, Ordering::SeqCst);
            Ok::<_, Infallible>(axum::response::Response::new(Body::from(
                user_id_from_extensions(req.extensions()),
            )))
        }
    });
    let resp = middleware(auth).layer(next).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn get_with_bearer(path: &str) -> Request<Body> {
    Request::get(path)
        .header("Authorization", "Bearer x")
        .body(Body::empty())
        .unwrap()
}

// parity: adapted: the caller travels as a request extension that middleware inserts, not a context value
#[tokio::test]
async fn test_context_round_trip() {
    let req = Request::get("/")
        .header("X-User", "alice")
        .body(Body::empty())
        .unwrap();
    let (status, body) = serve(header("X-User"), req, Arc::new(AtomicBool::new(false))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "alice", "UserIDFromContext()");

    let mut ext = http::Extensions::new();
    ext.insert(Caller {
        user_id: "alice".into(),
        claims: None,
    });
    assert_eq!(
        ext.get::<Caller>().map(|c| c.user_id.as_str()),
        Some("alice")
    );
    assert_eq!(user_id_from_extensions(&ext), "alice");
}

// parity: adapted: a request without a Caller extension stands in for an empty (or nil) context
#[test]
fn test_context_absent() {
    let req = get("/");
    assert!(
        req.extensions().get::<Caller>().is_none(),
        "CallerFromContext(empty) ok = true"
    );
    assert_eq!(user_id_from_extensions(req.extensions()), "");
}

// parity: adapted: Go's WithCaller(ctx, nil) is not inserting an extension; nothing is stored
#[test]
fn test_with_caller_nil_is_no_op() {
    let mut ext = http::Extensions::new();
    let caller: Option<Caller> = None;
    if let Some(c) = caller {
        ext.insert(c);
    }
    assert!(
        ext.get::<Caller>().is_none(),
        "CallerFromContext() ok = true after WithCaller(nil)"
    );
}

#[tokio::test]
async fn test_middleware_rejects_missing_credentials() {
    let called = Arc::new(AtomicBool::new(false));
    let (status, body) = serve(header("non-existing-header"), get("/"), called.clone()).await;
    assert!(
        !called.load(Ordering::SeqCst),
        "next handler ran on an unauthenticated request"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "unauthorized\n");
}

#[tokio::test]
async fn test_middleware_internal_error_is500() {
    let auth = custom(|_| async { Err(AuthnError::Other("issuer unreachable".into())) });
    let (status, body) = serve(auth, get_with_bearer("/"), Arc::new(AtomicBool::new(false))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "authentication failed\n");
}

#[tokio::test]
async fn test_middleware_forbidden_is403() {
    let auth = custom(|_| async {
        Err(AuthnError::Forbidden(Some(
            "principal not permitted".into(),
        )))
    });
    let (status, body) = serve(auth, get_with_bearer("/"), Arc::new(AtomicBool::new(false))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, "forbidden\n");
}

// parity: adapted: an AuthnError is one variant, so "wraps both" is Forbidden with a cause naming the other
#[tokio::test]
async fn test_middleware_forbidden_wins_when_error_wraps_both() {
    let auth = custom(|_| async {
        Err(AuthnError::Forbidden(Some(
            "refused: authn: unauthenticated".into(),
        )))
    });
    let (status, _) = serve(auth, get_with_bearer("/"), Arc::new(AtomicBool::new(false))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// parity: adapted: middleware takes no nil authenticator; noop (what a nil ServerConfig authenticator means) passes through with an empty user ID
#[tokio::test]
async fn test_middleware_nil_is_pass_through() {
    let (status, body) = serve(noop(), get("/"), Arc::new(AtomicBool::new(false))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "", "no identity without an authenticator");
}

#[tokio::test]
async fn test_middleware_caller_nil_no_error_is_rejected() {
    let auth = custom(|_| async { Ok(None) });
    let called = Arc::new(AtomicBool::new(false));
    let (status, body) = serve(auth, get("/"), called.clone()).await;
    assert!(
        !called.load(Ordering::SeqCst),
        "next handler ran despite a nil identity"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "unauthorized\n");
}
