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

//! [`RuntimeHttpClient`]: the `opentelemetry_http::HttpClient` every gadk OTLP exporter sends
//! through.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;

use async_trait::async_trait;
use google_cloud_auth::credentials::Credentials;
use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use tokio::runtime::Handle;

/// An OTLP exporter HTTP client on gadk's async reqwest stack.
///
/// The SDK's batch processors export from their own threads through a plain `block_on`, where
/// no Tokio reactor runs; `send_bytes` therefore spawns each request onto the runtime captured in
/// [`RuntimeHttpClient::new`] and waits for it. Each request carries the credentials' headers
/// (fetched per request, Go's `oauth2.Transport`), then the explicit headers (Go's
/// `otlptracehttp.WithHeaders`), which replace any header of the same name.
#[derive(Clone)]
pub struct RuntimeHttpClient {
    client: reqwest::Client,
    handle: Handle,
    credentials: Option<Credentials>,
    headers: HeaderMap,
}

impl RuntimeHttpClient {
    /// A client whose requests run on `handle`'s runtime.
    pub fn new(handle: Handle) -> Self {
        todo!()
    }

    /// Authenticates every request with `credentials`' headers.
    #[must_use]
    pub fn with_credentials(self, credentials: Credentials) -> Self {
        todo!()
    }

    /// Sets `name: value` on every request, after the credentials' headers.
    #[must_use]
    pub fn with_header(self, name: HeaderName, value: HeaderValue) -> Self {
        todo!()
    }
}

impl fmt::Debug for RuntimeHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeHttpClient")
            .field("credentials", &self.credentials.is_some())
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for RuntimeHttpClient {
    async fn send_bytes(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        todo!()
    }
}
