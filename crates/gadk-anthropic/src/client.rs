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

//! The HTTP transport: endpoints, request headers, and the SDK's send loop with retries and
//! per-attempt timeouts (port of anthropic-sdk-go v1.81.0 `RequestConfig.Execute` and
//! `vertex.WithCredentials`'s URL rules).
//!
//! Placeholder signatures written by Task 1; Task 5 implements them.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::{BodyStream, HttpClient};
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;

use crate::error::AnthropicError;

/// The Anthropic backend's default base URL (SDK `WithEnvironmentProduction`).
pub(crate) const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/";

/// The `Anthropic-Version` header value.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Go's default client `ResponseHeaderTimeout` (`defaultResponseHeaderTimeout`).
pub(crate) const DEFAULT_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(600);

/// Where a model's requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// `POST {base}/v1/messages`.
    Anthropic,
    /// `POST {base}/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:rawPredict`
    /// (`:streamRawPredict` when streaming).
    Vertex {
        /// The Google Cloud project.
        project: String,
        /// The Google Cloud location.
        location: String,
    },
}

/// How requests authenticate.
#[derive(Clone)]
pub(crate) enum Auth {
    /// Anthropic: `x-api-key` and/or `Authorization: Bearer` (both when both are set, as Go's
    /// SDK sends both).
    Anthropic {
        /// The API key.
        api_key: Option<String>,
        /// The bearer token.
        auth_token: Option<String>,
    },
    /// Vertex: the credentials' headers, fetched for every attempt.
    Vertex(Credentials),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic { .. } => f.write_str("Anthropic(<redacted>)"),
            Self::Vertex(_) => f.write_str("Vertex(<credentials>)"),
        }
    }
}

/// SDK `vertex.WithCredentials`'s base URL for `location`: `global` →
/// `https://aiplatform.googleapis.com/`, `us` → `https://aiplatform.us.rep.googleapis.com/`,
/// `eu` → `https://aiplatform.eu.rep.googleapis.com/`, else
/// `https://{location}-aiplatform.googleapis.com/`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn vertex_base_url(location: &str) -> String {
    todo!("Task 5")
}

/// The request URL: the endpoint's API path joined to `base_url` with exactly one `/`
/// (a base URL's own path is kept as a prefix).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn request_url(
    base_url: &str,
    endpoint: &Endpoint,
    model_id: &str,
    stream: bool,
) -> Result<String, AnthropicError> {
    todo!("Task 5")
}

/// A model's transport: everything needed to send one Messages request.
#[derive(Clone)]
pub(crate) struct Transport {
    /// The HTTP client (`ClientConfig.http_client`, or a `ReqwestClient`).
    pub(crate) http: Arc<dyn HttpClient>,
    /// The effective base URL (custom, `ANTHROPIC_BASE_URL`, or the backend default).
    pub(crate) base_url: String,
    /// The endpoint shape.
    pub(crate) endpoint: Endpoint,
    /// The authentication.
    pub(crate) auth: Auth,
    /// `ClientConfig.headers`, added after the headers gadk sets.
    pub(crate) headers: HeaderMap,
    /// Retries after the first attempt (`ClientConfig.max_retries`, default
    /// [`crate::retry::DEFAULT_MAX_RETRIES`]).
    pub(crate) max_retries: u32,
    /// A timeout on each `HttpClient::send` (until the response headers):
    /// [`DEFAULT_RESPONSE_HEADER_TIMEOUT`] when gadk built the client, `None` for a custom one.
    pub(crate) response_header_timeout: Option<Duration>,
}

#[allow(unused_variables, clippy::todo)]
impl Transport {
    /// The headers of one attempt: `Accept: application/json`, `Anthropic-Version`,
    /// `Content-Type: application/json`, the auth headers (on Vertex, the credentials' headers,
    /// fetched now), on Vertex `x-goog-api-client` and `user-agent` set to
    /// `gadk_platform::tracking_header()`, then [`headers`](Self::headers).
    pub(crate) async fn request_headers(&self) -> Result<HeaderMap, AnthropicError> {
        todo!("Task 5")
    }

    /// Sends `body` with the SDK's retry policy and returns the response that ends the call.
    ///
    /// - Attempts: `max_retries + 1`. A connection error, or `timeout` (> 0, per attempt)
    ///   expiring, is retried like a connection error; [`AnthropicError::deadline_exceeded`]
    ///   surfaces only when the last attempt timed out.
    /// - A response is retried per [`crate::retry::should_retry`] (a 2xx with
    ///   `x-should-retry: true` included); waits use [`crate::retry::retry_delay`].
    /// - The body of a response that ends the call is read inside the loop unless it is a
    ///   streaming 2xx; a body that fails to read is retried per
    ///   [`crate::retry::should_retry_dropped_body`].
    /// - A status ≥ 400 that ends the call is returned as `Err` with
    ///   [`crate::error::api_error_from_response`] (header `request-id`).
    /// - A non-streaming 2xx is returned with its whole body as one chunk; a streaming 2xx with
    ///   its live body, still under the attempt's `timeout` (an expiry yields
    ///   `ModelError::Other(AnthropicError::deadline_exceeded())`).
    /// - Every await is cancelled by dropping the future (no task is spawned).
    pub(crate) async fn send(
        &self,
        model_id: &str,
        body: Bytes,
        stream: bool,
        timeout: Option<Duration>,
    ) -> Result<http::Response<BodyStream>, AnthropicError> {
        todo!("Task 5")
    }
}
