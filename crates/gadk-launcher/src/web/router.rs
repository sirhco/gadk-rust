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

//! The gorilla/mux v1.8.1 router as the web launcher configures it (plan D6; Task 7).
// Contract stub (plan D1): Task 7 writes the bodies.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::convert::Infallible;

use gadk_server::mux::{Handler, Request, Response};

/// Go `router.Use(mw)`: wraps a matched route's handler.
pub type Middleware = std::sync::Arc<dyn Fn(Handler) -> Handler + Send + Sync>;

/// gorilla/mux v1.8.1 router as the launcher configures it (D6).
#[derive(Clone)]
pub struct WebRouter {
    strict_slash: bool,
}

impl WebRouter {
    /// gorilla `NewRouter().StrictSlash(strict_slash)`.
    pub fn new(strict_slash: bool) -> WebRouter {
        todo!("Task 7")
    }

    /// gorilla `Handle(path).Methods(...)`; empty `methods` = any method.
    pub fn handle(&mut self, methods: &[http::Method], path: &str, h: Handler) {
        todo!("Task 7")
    }

    /// gorilla `PathPrefix(prefix).Methods(...).Handler(h)` (not segment-aware).
    pub fn handle_prefix(&mut self, methods: &[http::Method], prefix: &str, h: Handler) {
        todo!("Task 7")
    }

    /// gorilla `Use`.
    pub fn use_middleware(&mut self, mw: Middleware) {
        todo!("Task 7")
    }

    /// Serves one request (gorilla `Router.ServeHTTP`).
    pub async fn serve(&self, req: Request) -> Response {
        todo!("Task 7")
    }

    /// An axum-compatible service (for `axum::serve`/hyper and tests).
    pub fn into_service(self) -> tower::util::BoxCloneSyncService<Request, Response, Infallible> {
        todo!("Task 7")
    }
}

/// Wraps an `axum::Router` as a `Handler` (the request is passed unchanged).
pub fn axum_handler(router: axum::Router) -> Handler {
    todo!("Task 7")
}
