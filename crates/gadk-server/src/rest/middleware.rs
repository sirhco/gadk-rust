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

//! The request-body limit and the panic catcher.

use std::any::Any;
use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use futures::StreamExt;
use http::header::CONNECTION;
use http::{HeaderValue, StatusCode};
use tower::{Layer, Service, ServiceExt};
use tower_http::catch_panic::CatchPanicLayer;

use super::DEFAULT_MAX_PAYLOAD_SIZE;
use super::http_error::http_error;
use crate::mux::{Handler, Request, Response, handler};

/// Reading the request body went past the limit. Handlers answer 400 with its text, `http:
/// request body too large`.
///
/// A handler reading the body (`axum::body::to_bytes`) sees it as the error's source; its
/// `Display` (through `axum::Error`) is this text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("http: request body too large")]
pub(crate) struct MaxBytesError {
    /// The limit in bytes.
    pub limit: i64,
}

/// Whether `err` or an error in its source chain is a [`MaxBytesError`] (the trigger controllers'
/// body reader tells it apart from other read errors).
pub(crate) fn is_max_bytes_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if e.is::<MaxBytesError>() {
            return true;
        }
        cur = e.source();
    }
    false
}

/// The effective limit: `max_bytes`, or [`DEFAULT_MAX_PAYLOAD_SIZE`] when zero or
/// negative.
pub(crate) fn resolve_max_bytes(max_bytes: i64) -> i64 {
    if max_bytes <= 0 {
        DEFAULT_MAX_PAYLOAD_SIZE
    } else {
        max_bytes
    }
}

/// Runs `next` with a request body that fails with [`MaxBytesError`] once more than `limit` bytes
/// are read (this wrapper writes no status; the handler reading the body answers). A response
/// produced after the limit was hit carries `Connection: close`, so the connection is closed
/// after it.
pub(crate) async fn with_max_bytes<F, Fut>(limit: i64, req: Request, next: F) -> Response
where
    F: FnOnce(Request) -> Fut,
    Fut: Future<Output = Response>,
{
    let limit = resolve_max_bytes(limit);
    let hit = Arc::new(AtomicBool::new(false));
    let (parts, body) = req.into_parts();
    let body = limited(body, limit, Arc::clone(&hit));
    let mut resp = next(Request::from_parts(parts, body)).await;
    // Ask for the connection to be closed; the response head is complete once `next` returns.
    if hit.load(Ordering::Acquire) {
        resp.headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }
    resp
}

/// The body limit as a handler wrapper (zero/negative: the 10 MiB default,
/// [`DEFAULT_MAX_PAYLOAD_SIZE`](super::DEFAULT_MAX_PAYLOAD_SIZE)).
///
/// The wrapped handler's request body fails once more than the limit is read: the read error's
/// text is `http: request body too large`, and a handler answering after hitting the limit gets
/// `Connection: close` added to its response. The wrapper itself writes no status; the handler
/// reading the body answers.
pub fn max_bytes_middleware(limit: i64) -> Arc<dyn Fn(Handler) -> Handler + Send + Sync> {
    let limit = resolve_max_bytes(limit);
    Arc::new(move |next: Handler| {
        handler(move |req: Request| {
            let next = Arc::clone(&next);
            async move { with_max_bytes(limit, req, |req| next(req)).await }
        })
    })
}

/// `body` failing with [`MaxBytesError`] on the chunk that takes it past `limit` bytes, setting
/// `hit`.
fn limited(body: Body, limit: i64, hit: Arc<AtomicBool>) -> Body {
    let mut remaining = u64::try_from(limit).unwrap_or(0);
    let stream = body.into_data_stream().map(move |chunk| {
        let chunk = chunk.map_err(axum::BoxError::from)?;
        let len = chunk.len() as u64;
        if len > remaining {
            remaining = 0;
            hit.store(true, Ordering::Release);
            return Err(axum::BoxError::from(MaxBytesError { limit }));
        }
        remaining -= len;
        Ok(chunk)
    });
    Body::from_stream(stream)
}

/// The panic catcher around a route's handler (`tower_http::catch_panic`): a panicking handler
/// answers 500 `Internal Server Error` in [`http_error`] framing and the panic is logged; the
/// connection is not aborted.
pub(crate) fn catch_panic<S>(
    inner: S,
) -> impl Service<Request, Response = Response, Error = Infallible, Future: Send>
+ Clone
+ Send
+ Sync
+ 'static
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
{
    CatchPanicLayer::custom(panic_response as fn(Box<dyn Any + Send + 'static>) -> Response)
        .layer(inner)
        .map_response(|resp: http::Response<_>| resp.map(Body::new))
}

/// The response to a handler panic; logs the panic message as `http: panic serving request`.
fn panic_response(err: Box<dyn Any + Send + 'static>) -> Response {
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    tracing::error!(panic = msg, "http: panic serving request");
    http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::INTERNAL_SERVER_ERROR
            .canonical_reason()
            .unwrap_or_default(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Go `TestMaxBytesMiddlewarePinsTheDefaultLimit`'s handler: read the whole body; a
    /// [`MaxBytesError`] answers 400 with its text, anything else 500; success 204.
    async fn read_all(req: Request) -> Response {
        match axum::body::to_bytes(req.into_body(), usize::MAX).await {
            Ok(_) => {
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::NO_CONTENT;
                r
            }
            Err(e) => {
                let status = if is_max_bytes_error(&e) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                http_error(status, &e.to_string())
            }
        }
    }

    fn post(body: Vec<u8>) -> Request {
        http::Request::post("/").body(Body::from(body)).unwrap()
    }

    async fn text(resp: Response) -> String {
        let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(b.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn default_limit_boundary_matches_go() {
        let limit = usize::try_from(DEFAULT_MAX_PAYLOAD_SIZE).unwrap();
        let ok = with_max_bytes(0, post(vec![b'a'; limit]), read_all).await;
        assert_eq!(ok.status(), StatusCode::NO_CONTENT, "exactly the limit");
        assert!(ok.headers().get(CONNECTION).is_none());

        let over = with_max_bytes(-1, post(vec![b'a'; limit + 1]), read_all).await;
        assert_eq!(over.status(), StatusCode::BAD_REQUEST, "one byte over");
        assert_eq!(over.headers()[CONNECTION], "close");
        assert_eq!(text(over).await, "http: request body too large\n");
    }

    #[tokio::test]
    async fn configured_limit_applies_across_chunks() {
        let chunks = futures::stream::iter(vec![
            Ok::<_, Infallible>(bytes::Bytes::from_static(b"12345")),
            Ok(bytes::Bytes::from_static(b"678")),
        ]);
        let req = http::Request::post("/")
            .body(Body::from_stream(chunks))
            .unwrap();
        let resp = with_max_bytes(7, req, read_all).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(text(resp).await, "http: request body too large\n");
    }

    /// A handler that never reads an oversized body is served normally and keeps the
    /// connection (Go only closes once the limit is hit).
    #[tokio::test]
    async fn unread_body_does_not_close() {
        let resp = with_max_bytes(1, post(b"too long".to_vec()), |_req| async {
            Response::new(Body::from("ok"))
        })
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(CONNECTION).is_none());
    }

    #[tokio::test]
    async fn panicking_handler_answers_500() {
        let h = handler(|_req: Request| async {
            if true {
                panic!("boom");
            }
            Response::new(Body::empty())
        });
        let svc = catch_panic(tower::service_fn(move |req| {
            let h = Arc::clone(&h);
            async move { Ok::<_, Infallible>(h(req).await) }
        }));
        let resp = svc
            .oneshot(http::Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
        assert_eq!(text(resp).await, "Internal Server Error\n");
    }
}
