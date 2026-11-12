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

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use gadk_model::gcp_auth::credential_headers;
use google_cloud_auth::credentials::Credentials;
use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use tokio::runtime::Handle;

/// Go's OTLP exporters' default request timeout (`otlptracehttp` `DefaultTimeout`).
///
/// Divergence (ledger 7a): every request has this timeout; `OTEL_EXPORTER_OTLP_TIMEOUT` bounds
/// only opentelemetry-otlp's retry deadline (Go applies it per request).
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

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
        Self {
            client: reqwest::Client::new(),
            handle,
            credentials: None,
            headers: HeaderMap::new(),
        }
    }

    /// Authenticates every request with `credentials`' headers.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Sets `name: value` on every request, after the credentials' headers.
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Sends `request` (on the captured runtime).
    async fn send(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let (parts, body) = request.into_parts();
        let mut headers = parts.headers;
        if let Some(credentials) = &self.credentials {
            replace_headers(&mut headers, &credential_headers(credentials).await?);
        }
        replace_headers(&mut headers, &self.headers);
        let response = self
            .client
            .request(parts.method, parts.uri.to_string())
            .headers(headers)
            .body(body)
            .timeout(EXPORT_TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        let response_headers = response.headers().clone();
        let body = response.bytes().await?;
        let mut out = Response::builder().status(status).body(body)?;
        *out.headers_mut() = response_headers;
        Ok(out)
    }
}

/// Replaces every header of `with` in `headers` (all of a name's values).
fn replace_headers(headers: &mut HeaderMap, with: &HeaderMap) {
    for name in with.keys() {
        headers.remove(name);
    }
    for (name, value) in with {
        headers.append(name.clone(), value.clone());
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
        let this = self.clone();
        self.handle
            .spawn(async move { this.send(request).await })
            .await?
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::test_support::{Sink, failing_credentials, fake_credentials};

    fn request(sink: &Sink) -> Request<Bytes> {
        Request::post(format!("{}/v1/traces", sink.url))
            .header("content-type", "application/x-protobuf")
            .header("x-goog-user-project", "from-request")
            .body(Bytes::from_static(b"payload"))
            .unwrap()
    }

    fn values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
        headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn explicit_headers_win_over_credential_and_request_headers() {
        let sink = Sink::start().await;
        let client = RuntimeHttpClient::new(Handle::current())
            .with_credentials(fake_credentials(&[
                ("authorization", "Bearer token"),
                ("x-goog-user-project", "from-credentials"),
            ]))
            .with_header(
                HeaderName::from_static("x-goog-user-project"),
                HeaderValue::from_static("quota-project"),
            );
        let response = client.send_bytes(request(&sink)).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let got = sink.requests();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "/v1/traces");
        assert_eq!(
            values(&got[0].headers, "x-goog-user-project"),
            ["quota-project"]
        );
        assert_eq!(values(&got[0].headers, "authorization"), ["Bearer token"]);
        assert_eq!(
            values(&got[0].headers, "content-type"),
            ["application/x-protobuf"]
        );
        assert_eq!(got[0].body, b"payload");
    }

    #[tokio::test]
    async fn without_credentials_the_request_is_sent_as_is() {
        let sink = Sink::start().await;
        let client = RuntimeHttpClient::new(Handle::current());
        client.send_bytes(request(&sink)).await.unwrap();
        let got = sink.requests();
        assert_eq!(
            values(&got[0].headers, "x-goog-user-project"),
            ["from-request"]
        );
        assert!(got[0].headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn a_credentials_failure_sends_nothing() {
        let sink = Sink::start().await;
        let client =
            RuntimeHttpClient::new(Handle::current()).with_credentials(failing_credentials());
        let err = client.send_bytes(request(&sink)).await.unwrap_err();
        assert!(err.to_string().contains("no token"), "{err}");
        assert!(sink.requests().is_empty());
    }

    // The SDK's batch processors export from a plain thread through `block_on`, with no Tokio
    // reactor: the request still runs, on the captured runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sends_from_a_thread_without_a_runtime() {
        let sink = Sink::start().await;
        let client = RuntimeHttpClient::new(Handle::current());
        let req = request(&sink);
        let status = std::thread::spawn(move || {
            futures::executor::block_on(client.send_bytes(req)).map(|r| r.status())
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(sink.requests().len(), 1);
    }
}
