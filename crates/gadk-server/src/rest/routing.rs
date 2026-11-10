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

//! The route table (port of ADK-Go `server/adkrest/internal/routers/routers.go`): each
//! controller's [`Route`]s, a gorilla/mux path-template matcher, per-route body limit and
//! authentication, and Go's fallback for unmatched requests (plan decision D4). Task 5 fills
//! [`RouteTable`]; the types, [`handler`] and [`route`] are complete.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::authn::Authenticator;

/// A request as a handler sees it (plan decision D3).
pub type Request = http::Request<axum::body::Body>;
/// A handler's response.
pub type Response = axum::response::Response;
/// A route's handler (Go `http.HandlerFunc`).
pub type Handler = Arc<dyn Fn(Request) -> BoxFuture<'static, Response> + Send + Sync>;

/// The path variables of the matched route (Go `mux.Vars`), as a request extension; tests insert
/// it directly (Go `mux.SetURLVars`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathVars(pub BTreeMap<String, String>);

/// Port of Go `routers.Route`.
pub struct Route {
    /// The route's name (Go `Name`).
    pub name: &'static str,
    /// The methods it serves; GET routes also serve HEAD (added by [`RouteTable::new`]).
    pub methods: Vec<http::Method>,
    /// A gorilla/mux path template (`{name}` matches one non-empty segment).
    pub pattern: String,
    /// Exempt from authentication (Go `Public`).
    pub public: bool,
    /// The handler.
    pub handler: Handler,
}

/// Wraps an async function as a [`Handler`].
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

/// A non-public [`Route`].
pub(crate) fn route(
    name: &'static str,
    methods: &[http::Method],
    pattern: impl Into<String>,
    handler: Handler,
) -> Route {
    Route {
        name,
        methods: methods.to_vec(),
        pattern: pattern.into(),
        public: false,
        handler,
    }
}

/// A [`Handler`] calling `$controller.$method(req)` on a clone of the `Arc`ed controller.
macro_rules! controller_handler {
    ($controller:expr, $method:ident) => {{
        let c = ::std::sync::Arc::clone(&$controller);
        $crate::rest::routing::handler(move |req| {
            let c = ::std::sync::Arc::clone(&c);
            async move { c.$method(req).await }
        })
    }};
}
pub(crate) use controller_handler;

/// Go `routers.DevPrefix`: the path prefix of the developer-only endpoints.
pub const DEV_PREFIX: &str = "/dev/apps/{app_name}";

/// The routes of a server in registration order, with their matcher, body limit and
/// authentication (Go `SetupSubRouters` on a `mux.Router` with `MaxBytesMiddleware`).
pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    /// Builds the table: HEAD added to GET routes (Go `withHead`), every non-public route wrapped
    /// in [`authn::middleware`](crate::authn::middleware) when `authenticator` is set, and
    /// `max_payload_size` (zero or negative: [`DEFAULT_MAX_PAYLOAD_SIZE`]) applied to every
    /// matched route.
    ///
    /// [`DEFAULT_MAX_PAYLOAD_SIZE`]: super::DEFAULT_MAX_PAYLOAD_SIZE
    pub fn new(
        routes: Vec<Route>,
        authenticator: Option<Arc<dyn Authenticator>>,
        max_payload_size: i64,
    ) -> Self {
        todo!("plan 5a Task 5")
    }

    /// Serves `req`: the first matching route (path variables as [`PathVars`]) runs the body
    /// limit, then authentication unless public, then its handler; an unmatched request gets Go's
    /// fallback (trailing slash re-dispatch, OPTIONS 204, 405 with `Allow`, 404).
    pub async fn serve(&self, req: Request) -> Response {
        todo!("plan 5a Task 5")
    }
}
