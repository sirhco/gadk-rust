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

//! [`HttpTestServer`]: a streamable-HTTP MCP server on `127.0.0.1` (rmcp
//! `StreamableHttpService` behind a hyper listener), standing in for the `httptest` servers of
//! ADK-Go's `tracking_test.go` and `auth_session_test.go`.

use std::sync::Arc;

use rmcp::ServerHandler;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    /// The request method.
    pub method: http::Method,
    /// The request headers, as received.
    pub headers: http::HeaderMap,
}

/// Answers a request before the MCP service sees it: `Some(response)` is sent instead, `None`
/// passes the request on. It gets the request head and its body bytes.
pub type FrontHook =
    Arc<dyn Fn(&http::request::Parts, &[u8]) -> Option<http::Response<String>> + Send + Sync>;

/// A local streamable-HTTP MCP server. Every request is recorded (Go `headerRecorder`), passed to
/// the front hook, then served. Dropping it stops the listener.
pub struct HttpTestServer {}

#[allow(unused_variables, clippy::todo)]
impl HttpTestServer {
    /// Serves a fresh `make_server()` per MCP session (stateful streamable HTTP) on an ephemeral
    /// `127.0.0.1` port.
    pub async fn start<S, F>(make_server: F) -> Self
    where
        F: Fn() -> S + Send + Sync + 'static,
        S: ServerHandler,
    {
        todo!("Task 4")
    }

    /// Go `newUserBoundServer`: every request needs `Authorization: Bearer token-of-<user>`
    /// (else 401); the MCP session is bound to the user whose request created it, and a request
    /// on another user's session gets 403 `session user mismatch`; the one tool `whoami`
    /// ("returns the caller") answers the user as text. Records the sessions each user's requests
    /// arrived on and those each user closed with DELETE.
    pub async fn user_bound() -> Self {
        todo!("Task 4")
    }

    /// The MCP endpoint URL (`http://127.0.0.1:<port>/`).
    pub fn url(&self) -> String {
        todo!("Task 4")
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        todo!("Task 4")
    }

    /// Go `sessionsOf`: how many distinct MCP sessions `user`'s requests arrived on.
    pub fn sessions_of(&self, user: &str) -> usize {
        todo!("Task 4")
    }

    /// Go `deletedOf`: how many distinct MCP sessions `user` closed with DELETE.
    pub fn deleted_of(&self, user: &str) -> usize {
        todo!("Task 4")
    }

    /// Holds the `initialize` POST of any request whose `Authorization` header contains `needle`
    /// until [`InitializeHold::release`] (Go `TestAuthConcurrentCloseWaits`'s front server).
    pub fn hold_initialize(&self, needle: &str) -> InitializeHold {
        todo!("Task 4")
    }

    /// Installs (or with `None` removes) the front hook.
    pub fn set_front(&self, hook: Option<FrontHook>) {
        todo!("Task 4")
    }
}

/// A held `initialize` request (see [`HttpTestServer::hold_initialize`]).
pub struct InitializeHold {}

#[allow(clippy::todo)]
impl InitializeHold {
    /// Waits until a matching `initialize` request has arrived and is being held (Go
    /// `<-initializing`).
    pub async fn initializing(&self) {
        todo!("Task 4")
    }

    /// Lets the held request (and later matching ones) through (Go `close(release)`).
    pub fn release(&self) {
        todo!("Task 4")
    }
}
