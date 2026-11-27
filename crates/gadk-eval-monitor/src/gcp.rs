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

//! Google Cloud access shared by the BigQuery source and the GCS report store: request
//! credentials ([`TokenSource`]) and JSON requests on gadk's [`HttpClient`] seam.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::gcp_auth::{AdcError, adc_credentials, credential_headers};
use gadk_model::{HttpClient, ReqwestClient, collect_body};
use gadk_platform::env::{EnvProvider, ProcessEnv};
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;

/// The OAuth scope requested for Application Default Credentials.
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The longest reply body quoted in an error ([`excerpt`]).
pub(crate) const BODY_QUOTE_MAX: usize = 512;

/// The headers that authenticate a Google Cloud request.
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    /// The headers for one request, typically `Authorization: Bearer …`; the error describes
    /// why none could be produced.
    async fn headers(&self) -> Result<HeaderMap, String>;
}

/// Google credentials: Application Default Credentials ([`AdcTokenSource::adc`]) or explicit
/// ones ([`AdcTokenSource::from_credentials`]).
#[derive(Debug, Clone)]
pub struct AdcTokenSource {
    credentials: Credentials,
}

impl AdcTokenSource {
    /// Application Default Credentials with the `cloud-platform` scope (honouring
    /// `GOOGLE_APPLICATION_CREDENTIALS`). Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`AdcError`] when no credentials can be built; some credential failures (such as
    /// metadata-server credentials with no metadata server) only show on the first request.
    pub fn adc() -> Result<Self, AdcError> {
        let file = ProcessEnv.var("GOOGLE_APPLICATION_CREDENTIALS");
        Ok(Self::from_credentials(adc_credentials(
            file.as_deref(),
            &[CLOUD_PLATFORM_SCOPE],
        )?))
    }

    /// Explicit credentials.
    pub fn from_credentials(credentials: Credentials) -> Self {
        Self { credentials }
    }
}

#[async_trait::async_trait]
impl TokenSource for AdcTokenSource {
    async fn headers(&self) -> Result<HeaderMap, String> {
        credential_headers(&self.credentials)
            .await
            .map_err(|e| format!("credentials: {e}"))
    }
}

/// Sends no credentials (tests and emulators).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

#[async_trait::async_trait]
impl TokenSource for NoAuth {
    async fn headers(&self) -> Result<HeaderMap, String> {
        Ok(HeaderMap::new())
    }
}

/// A reply: the status and the whole body.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
}

impl Reply {
    /// Whether the status is 2xx.
    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The error text of a non-2xx reply: `HTTP <status>: <error.message>` for a Google API error
    /// body, else the start of the body.
    pub(crate) fn error_text(&self) -> String {
        let message = serde_json::from_slice::<Value>(&self.body)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| excerpt(&self.body));
        format!("HTTP {}: {message}", self.status)
    }
}

/// The start of `bytes`, at most [`BODY_QUOTE_MAX`] bytes, cut on a character boundary.
pub(crate) fn excerpt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text[..text.floor_char_boundary(BODY_QUOTE_MAX.min(text.len()))].to_owned()
}

/// Authenticated requests to Google Cloud REST APIs.
#[derive(Clone)]
pub(crate) struct GcpHttp {
    http: Arc<dyn HttpClient>,
    auth: Arc<dyn TokenSource>,
}

impl GcpHttp {
    pub(crate) fn new(http: Arc<dyn HttpClient>, auth: Arc<dyn TokenSource>) -> Self {
        Self { http, auth }
    }

    /// A `reqwest` client whose requests time out after five minutes.
    pub(crate) fn default_http() -> Arc<dyn HttpClient> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        Arc::new(ReqwestClient::new(client))
    }

    /// Sends `method` to `url` with an optional `(content type, body)`; transport and
    /// credential failures are errors, any HTTP status is a [`Reply`].
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<(&'static str, Bytes)>,
    ) -> Result<Reply, String> {
        let mut req = http::Request::new(Bytes::new());
        *req.method_mut() = method;
        *req.uri_mut() = url.parse().map_err(|e| format!("url {url}: {e}"))?;
        let headers = req.headers_mut();
        headers.extend(self.auth.headers().await?);
        let ua = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| format!("user agent: {e}"))?;
        headers.insert(USER_AGENT, ua);
        if let Some((content_type, bytes)) = body {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            *req.body_mut() = bytes;
        }
        let resp = self
            .http
            .send(req)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status().as_u16();
        let body = collect_body(resp.into_body())
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        Ok(Reply { status, body })
    }

    /// Sends a JSON request (or none) and decodes a 2xx JSON reply; a non-2xx reply is an error
    /// ([`Reply::error_text`]).
    pub(crate) async fn json(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let body = body.map(|b| ("application/json", Bytes::from(b.to_string())));
        let reply = self.send(method, url, body).await?;
        if !reply.ok() {
            return Err(reply.error_text());
        }
        serde_json::from_slice(&reply.body).map_err(|e| format!("decoding the reply: {e}"))
    }
}

/// `s` percent-encoded for a URL path segment or query value: everything except
/// `[A-Za-z0-9-._~]` becomes `%XX`.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient, bearer_credentials};
    use serde_json::json;

    use super::*;
    use crate::testutil::{exchange, replay};

    #[test]
    fn encode_escapes_everything_but_unreserved() {
        assert_eq!(encode("A-z_0.9~"), "A-z_0.9~");
        assert_eq!(encode("reports/a b"), "reports%2Fa%20b");
        assert_eq!(encode("é"), "%C3%A9");
    }

    #[tokio::test]
    async fn sends_credentials_and_the_user_agent() {
        let client = ScriptedClient::json(r#"{"ok": true}"#);
        let auth = AdcTokenSource::from_credentials(bearer_credentials("tok"));
        let gcp = GcpHttp::new(client.clone(), Arc::new(auth));
        let v = gcp
            .json(
                Method::POST,
                "https://example.test/x",
                Some(&json!({"a": 1})),
            )
            .await
            .unwrap();
        assert_eq!(v, json!({"ok": true}));
        let req = client.last_request();
        assert_eq!(req.header("authorization"), "Bearer tok");
        assert!(req.header("user-agent").starts_with("gadk-rust/"));
        assert_eq!(req.header("content-type"), "application/json");
        assert_eq!(req.json(), json!({"a": 1}));
    }

    #[tokio::test]
    async fn no_auth_sends_no_authorization() {
        let client = ScriptedClient::json("{}");
        let gcp = GcpHttp::new(client.clone(), Arc::new(NoAuth));
        gcp.json(Method::GET, "https://example.test/x", None)
            .await
            .unwrap();
        assert_eq!(client.last_request().header("authorization"), "");
    }

    #[tokio::test]
    async fn replays_a_trace_and_reports_api_errors() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                exchange(
                    "POST",
                    "https://example.test/ok",
                    r#"{"q":1}"#,
                    200,
                    r#"{"rows":[]}"#,
                ),
                exchange(
                    "GET",
                    "https://example.test/denied",
                    "",
                    403,
                    r#"{"error":{"code":403,"message":"Access Denied","status":"PERMISSION_DENIED"}}"#,
                ),
                exchange("GET", "https://example.test/plain", "", 500, "oops"),
            ],
        );
        let gcp = GcpHttp::new(http, Arc::new(NoAuth));
        assert_eq!(
            gcp.json(
                Method::POST,
                "https://example.test/ok",
                Some(&json!({"q": 1}))
            )
            .await
            .unwrap(),
            json!({"rows": []})
        );
        assert_eq!(
            gcp.json(Method::GET, "https://example.test/denied", None)
                .await
                .unwrap_err(),
            "HTTP 403: Access Denied"
        );
        assert_eq!(
            gcp.json(Method::GET, "https://example.test/plain", None)
                .await
                .unwrap_err(),
            "HTTP 500: oops"
        );
    }

    #[tokio::test]
    async fn a_status_is_a_reply_not_an_error() {
        let client = ScriptedClient::always(FakeResponse::json_status(412, "{}"));
        let gcp = GcpHttp::new(client, Arc::new(NoAuth));
        let reply = gcp
            .send(Method::POST, "https://example.test/x", None)
            .await
            .unwrap();
        assert_eq!(reply.status, 412);
        assert!(!reply.ok());
    }
}
