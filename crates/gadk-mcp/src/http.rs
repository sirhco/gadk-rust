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

//! The HTTP client of MCP streamable-HTTP sessions (port of ADK-Go
//! `tool/mcptoolset/tracking.go` `trackingTransport` and set.go `httpClient`): the ADK tracking
//! headers on every request and, with auth, the credential's headers with one refresh on an
//! auth rejection.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_auth::{AuthContext, CredentialProvider};
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};

/// A `reqwest::Client` that adds the tracking headers, the configured headers and (with `auth`)
/// the credential's headers to every MCP request, delegating to rmcp's `reqwest::Client`
/// implementation.
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct AdkHttpClient {
    /// The client requests go through.
    pub(crate) inner: reqwest::Client,
    /// The transport's configured headers (`user-agent` and `x-goog-api-client` are merged after
    /// the tracking tokens).
    pub(crate) headers: http::HeaderMap,
    /// The provider and the identity of the session's creator.
    pub(crate) auth: Option<(Arc<dyn CredentialProvider>, AuthContext)>,
}

#[allow(unused_variables, clippy::todo)]
impl StreamableHttpClient for AdkHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        todo!("Task 7")
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        todo!("Task 7")
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        todo!("Task 7")
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        todo!("Task 7")
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        todo!("Task 7")
    }
}

/// Go `trackingTransport` for one header name: the ADK tracking tokens, then the whitespace-split
/// tokens of `existing` (the configured values, in order) not already present, joined by one
/// space.
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn merge_tracking_tokens(existing: &[&str]) -> String {
    todo!("Task 7")
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
