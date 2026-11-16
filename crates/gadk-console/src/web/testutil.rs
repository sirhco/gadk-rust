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

//! Test helpers for the web layer: a scripted trace source, a fixed state, and in-process
//! requests through the full router (guards and CSP included).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use chrono::{DateTime, TimeZone, Utc};
use gadk_prompts::PromptStore;
use http::header::{CONTENT_TYPE, HOST, ORIGIN};
use http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;

use super::{AppState, router};
use crate::traces::{SourceError, TraceDetail, TraceId, TracePage, TraceQuery, TraceSource};

/// The port every test state claims to be bound to.
pub(crate) const PORT: u16 = 8765;
/// The CSRF token every test state carries (64 × 'a').
pub(crate) const CSRF: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// 2026-11-16T12:00:00Z.
pub(crate) fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
}

/// Scripted TraceSource: returns clones of the stored results and counts calls.
pub(crate) struct FakeSource {
    pub list: Mutex<Result<TracePage, SourceError>>,
    pub get: Mutex<Result<TraceDetail, SourceError>>,
    pub calls: AtomicUsize,
}

impl FakeSource {
    /// `list` answers an empty page; `get` answers `NotFound`.
    pub(crate) fn new() -> Self {
        Self {
            list: Mutex::new(Ok(TracePage::default())),
            get: Mutex::new(Err(SourceError::NotFound)),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl TraceSource for FakeSource {
    async fn list(&self, _query: &TraceQuery) -> Result<TracePage, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.list.lock().unwrap().clone()
    }

    async fn get(&self, _id: &TraceId) -> Result<TraceDetail, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.get.lock().unwrap().clone()
    }
}

/// State with csrf `CSRF`, port `PORT` and clock `fixed_now`.
pub(crate) fn state(source: Arc<FakeSource>, store: Option<Arc<dyn PromptStore>>) -> AppState {
    AppState {
        source,
        store,
        csrf: Arc::from(CSRF),
        port: PORT,
        now: fixed_now,
    }
}

/// The `Host` value the guard accepts for `PORT`.
fn host() -> String {
    format!("127.0.0.1:{PORT}")
}

/// Sends GET path with Host 127.0.0.1:PORT through router(state) via tower::ServiceExt::oneshot.
pub(crate) async fn get(state: AppState, path: &str) -> (StatusCode, HeaderMap, String) {
    let req = Request::get(path)
        .header(HOST, host())
        .body(Body::empty())
        .unwrap();
    send(state, req).await
}

/// POSTs an application/x-www-form-urlencoded body with Host 127.0.0.1:PORT and the given
/// Origin (None = no header).
pub(crate) async fn post_form(
    state: AppState,
    path: &str,
    body: &str,
    origin: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::post(path)
        .header(HOST, host())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(origin) = origin {
        req = req.header(ORIGIN, origin);
    }
    send(state, req.body(Body::from(body.to_owned())).unwrap()).await
}

/// Sends an arbitrary request (for Host tests).
pub(crate) async fn send(state: AppState, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}
