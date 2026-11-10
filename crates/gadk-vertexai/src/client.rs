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

//! `VertexClient`: one Vertex AI REST call the way Go's generated REST client makes it (`$alt`
//! query, `x-goog-request-params` routing header, credentials, protojson bodies), list paging,
//! and long-running-operation polling.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use serde_json::Value;

use crate::config::ResolvedClient;
use crate::error::BoxError;

#[cfg(test)]
mod wire_tests;

/// The Vertex AI REST client both services use.
pub(crate) struct VertexClient {
    resolved: ResolvedClient,
}

impl VertexClient {
    pub(crate) fn new(resolved: ResolvedClient) -> Self {
        todo!()
    }

    /// One call: `{base_url}{go_escaped_path(path)}?{go_values_encode(query + ("$alt",
    /// "json;enum-encoding=int"))}`, header `x-goog-request-params:
    /// {routing.0}={go_query_escape(routing.1)}`, gadk's tracking `User-Agent`, credential
    /// headers, and `Content-Type: application/json` with a body. `path` is unescaped and starts
    /// with `/` (`/v1beta1/projects/…:appendEvent`, `/ui/projects/…/operations/…`). A 2xx reply is
    /// its body as JSON (`{}` when empty); any other status is
    /// `VertexError::from_response`. No retries.
    pub(crate) async fn call(
        &self,
        method: http::Method,
        path: &str,
        query: &[(&str, String)],
        routing: (&str, &str),
        body: Option<&Value>,
    ) -> Result<Value, BoxError> {
        todo!()
    }

    /// GETs `path` repeatedly, adding `pageToken` from each page's `nextPageToken` until it is
    /// empty, and concatenates each page's `items_key` array.
    pub(crate) async fn list_all(
        &self,
        path: &str,
        query: &[(&str, String)],
        routing: (&str, &str),
        items_key: &str,
    ) -> Result<Vec<Value>, BoxError> {
        todo!()
    }

    /// Go `Operation.WaitWithInterval(ctx, resp, time.Minute)`: while `op.done` is not true,
    /// sleep a full-jitter pause (initial 1 s, ×2, cap 60 s; the first poll is immediate) and GET
    /// `/ui/{op.name}`; a done operation with `error` is
    /// `Err(VertexError::from_operation_error)`, else `Ok(op)`.
    pub(crate) async fn wait_operation(&self, op: Value) -> Result<Value, BoxError> {
        todo!()
    }
}
