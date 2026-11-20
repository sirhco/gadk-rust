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

//! `VertexClient`: one Vertex AI REST call as the API expects it (`$alt` query,
//! `x-goog-request-params` routing header, credentials, protojson bodies), list paging, and
//! long-running-operation polling.

use std::time::Duration;

use bytes::Bytes;
use gadk_model::gcp_auth::credential_headers;
use gadk_model::rest::{
    CallTimeout, RequestHeaders, RestError, RestReply, RestTransport, RetryAfterRule, RetryPolicy,
};
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use serde_json::Value;
use tokio::time::Instant;

use crate::config::ResolvedClient;
use crate::error::{BoxError, VertexError};
use crate::protojson::{go_escaped_path, go_query_escape, go_values_encode};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod wire_tests;

/// The query parameter every REST call adds.
const ALT: (&str, &str) = ("$alt", "json;enum-encoding=int");

/// The routing header.
const X_GOOG_REQUEST_PARAMS: HeaderName = HeaderName::from_static("x-goog-request-params");

/// The upload-protocol header a multipart upload sends.
const X_GOOG_UPLOAD_PROTOCOL: HeaderName = HeaderName::from_static("x-goog-upload-protocol");

/// The content type of a JSON body.
const JSON: &str = "application/json";

/// No call is retried: the session and memory-bank calls use no retry options. (Operation
/// polls retry a 503 themselves; see [`VertexClient::get_operation`].)
const NO_RETRIES: RetryPolicy = RetryPolicy {
    max_retries: 0,
    max_backoff: Duration::ZERO,
    retry_after: RetryAfterRule::Unbounded,
    retry_dropped_body: false,
};

/// The first pause bound when waiting for an operation.
const WAIT_INITIAL: Duration = Duration::from_secs(1);
/// The wait interval, the pause bound's cap.
const WAIT_INTERVAL: Duration = Duration::from_secs(60);

/// The `GetOperation` call options: a 10 s timeout over the poll and its retries…
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
/// …and a 503 retried with a backoff of initial 500 ms, max 10 s, multiplier 2.
const POLL_RETRY_INITIAL: Duration = Duration::from_millis(500);
/// See [`POLL_RETRY_INITIAL`].
const POLL_RETRY_MAX: Duration = Duration::from_secs(10);

/// A backoff with multiplier 2: each [`pause`](Self::pause) is uniform in `[1ns, cur]`, then
/// `cur` doubles up to `max`.
pub(crate) struct Backoff {
    cur: Duration,
    max: Duration,
}

impl Backoff {
    /// A backoff starting at `initial` and capped at `max` (raised to `initial` when smaller,
    /// as `waitWithInterval` does).
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
        Self {
            cur: initial,
            max: max.max(initial),
        }
    }

    /// A random pause of `1 + rand(0..cur)` nanoseconds.
    pub(crate) fn pause(&mut self) -> Duration {
        self.pause_with(|n| rand::random_range(0..n))
    }

    /// [`pause`](Self::pause) with the random draw given: `below(n)` returns a value in
    /// `[0, n)` (clamped to it).
    pub(crate) fn pause_with(&mut self, below: impl FnOnce(u64) -> u64) -> Duration {
        let cur = u64::try_from(self.cur.as_nanos())
            .unwrap_or(u64::MAX)
            .max(1);
        let d = Duration::from_nanos(1 + below(cur).min(cur - 1));
        self.cur = self.cur.saturating_mul(2).min(self.max);
        d
    }
}

/// One attempt's headers: gadk's tracking `User-Agent`, the routing header (when the method
/// has one), `Content-Type` (sent on every call, bodiless ones too: `application/json` for
/// [`VertexClient::call`]), any extra headers, then the credentials' headers (fetched per
/// call, so a refreshed token is used).
struct CallHeaders<'a> {
    credentials: Option<&'a Credentials>,
    routing: Option<(&'a str, &'a str)>,
    content_type: &'a str,
    extra: &'a [(HeaderName, HeaderValue)],
}

#[async_trait::async_trait]
impl RequestHeaders for CallHeaders<'_> {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        let invalid = |e: http::header::InvalidHeaderValue| RestError::Headers(Box::new(e));
        let mut h = HeaderMap::new();
        h.insert(
            USER_AGENT,
            HeaderValue::from_str(&gadk_platform::tracking_header()).map_err(invalid)?,
        );
        if let Some((key, value)) = self.routing {
            let routing = format!("{key}={}", go_query_escape(value));
            h.insert(
                X_GOOG_REQUEST_PARAMS,
                HeaderValue::from_str(&routing).map_err(invalid)?,
            );
        }
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(self.content_type).map_err(invalid)?,
        );
        for (name, value) in self.extra {
            h.insert(name.clone(), value.clone());
        }
        if let Some(credentials) = self.credentials {
            let auth = credential_headers(credentials)
                .await
                .map_err(|e| RestError::Headers(Box::new(e)))?;
            h.extend(auth);
        }
        Ok(h)
    }
}

/// A request body and the headers that describe it.
struct Payload<'a> {
    body: Bytes,
    content_type: &'a str,
    extra: &'a [(HeaderName, HeaderValue)],
}

/// The Vertex AI REST client both services use.
pub(crate) struct VertexClient {
    /// Scheme + authority (+ optional path prefix), no trailing `/`.
    base_url: String,
    /// Scheme + authority only: an operation poll replaces the whole URL path, dropping any
    /// prefix.
    origin: String,
    credentials: Option<Credentials>,
    transport: RestTransport,
    /// The deadline of each `call` and upload; `None` waits as long as it takes.
    timeout: Option<Duration>,
}

impl VertexClient {
    pub(crate) fn new(resolved: ResolvedClient) -> Self {
        let origin = resolved
            .base_url
            .parse::<Uri>()
            .ok()
            .and_then(|uri| Some(format!("{}://{}", uri.scheme_str()?, uri.authority()?)))
            .unwrap_or_else(|| resolved.base_url.clone());
        Self {
            base_url: resolved.base_url,
            origin,
            credentials: resolved.credentials,
            transport: RestTransport {
                http: resolved.http,
                policy: NO_RETRIES,
                response_header_timeout: None,
            },
            timeout: None,
        }
    }

    /// Gives every [`call`](Self::call) (so every [`list_all`](Self::list_all) page) and
    /// [`upload_multipart`](Self::upload_multipart) a deadline of `timeout`
    /// (`CallTimeout::PerAttempt`; this client never retries, so it bounds the request): a
    /// slower request fails with `context deadline exceeded`. Operation polls keep their own
    /// 10 s deadline.
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The timeout of a `call` or upload: `PerAttempt(timeout)`, or none.
    fn call_timeout(&self) -> CallTimeout {
        self.timeout
            .map_or(CallTimeout::None, CallTimeout::PerAttempt)
    }

    /// One call: URL `{base_url}{go_escaped_path(path)}?{query}` where the query is `query`
    /// plus `$alt=json;enum-encoding=int`, encoded by `go_values_encode`; header
    /// `x-goog-request-params: {routing.0}={go_query_escape(routing.1)}` (none when `routing.0`
    /// is empty: the list methods send none), gadk's tracking `User-Agent`, credential headers, and
    /// `Content-Type: application/json` (on every call). `path` is unescaped and
    /// starts with `/` (`/v1beta1/projects/…:appendEvent`). A 2xx reply is its body as JSON
    /// (`{}` when empty); any other status is `VertexError::from_response`. No retries; no timeout
    /// unless [`with_timeout`](Self::with_timeout) set one.
    ///
    /// # Errors
    ///
    /// A [`VertexError`] for a non-2xx status; the [`RestError`] when no response was read; a
    /// `serde_json::Error` when the body or reply is not JSON.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        routing: (&str, &str),
        body: Option<&Value>,
    ) -> Result<Value, BoxError> {
        let mut pairs = query.to_vec();
        pairs.push((ALT.0, ALT.1.to_owned()));
        let url = format!(
            "{}{}?{}",
            self.base_url,
            go_escaped_path(path),
            go_values_encode(&pairs)
        );
        let routing = (!routing.0.is_empty()).then_some(routing);
        self.send(method, &url, routing, body, self.call_timeout())
            .await
    }

    /// GETs `path` repeatedly, adding `pageToken` from each page's `nextPageToken` until it is
    /// empty, and concatenates each page's `items_key` array. The list methods (sessions,
    /// events) send no routing header: pass `("", "")`.
    ///
    /// # Errors
    ///
    /// The first page's error, as [`call`](Self::call) returns it.
    pub(crate) async fn list_all(
        &self,
        path: &str,
        query: &[(&str, String)],
        routing: (&str, &str),
        items_key: &str,
    ) -> Result<Vec<Value>, BoxError> {
        let mut items = Vec::new();
        let mut token = String::new();
        loop {
            let mut pairs = query.to_vec();
            if !token.is_empty() {
                pairs.push(("pageToken", token));
            }
            let mut page = self.call(Method::GET, path, &pairs, routing, None).await?;
            if let Some(Value::Array(page_items)) = page.get_mut(items_key).map(Value::take) {
                items.extend(page_items);
            }
            token = page
                .get("nextPageToken")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if token.is_empty() {
                return Ok(items);
            }
        }
    }

    /// Waits for a long-running operation, polling it at `/ui/{name}` (the path the Agent
    /// Engine services' operations use); see [`wait_operation_at`](Self::wait_operation_at).
    ///
    /// # Errors
    ///
    /// The operation's error, or a failed poll's.
    pub(crate) async fn wait_operation(&self, op: Value) -> Result<Value, BoxError> {
        self.wait_operation_at(op, "/ui").await
    }

    /// Waits for a long-running operation: while `op.done` is not true,
    /// GET the operation ([`get_operation`](Self::get_operation)) and, while still not done,
    /// sleep a full-jitter pause (initial 1 s, ×2, cap 60 s; the first poll is immediate). The
    /// poll path is `{poll_prefix}/{name}` of the operation as given (`poll_prefix` such as
    /// `/ui` or `/v1`); the routing header names the latest operation's `name`. A done
    /// operation with `error` is `Err(VertexError::from_operation_error)`, else `Ok(op)`.
    ///
    /// # Errors
    ///
    /// The operation's error, or a failed poll's.
    pub(crate) async fn wait_operation_at(
        &self,
        mut op: Value,
        poll_prefix: &str,
    ) -> Result<Value, BoxError> {
        let poll_path = format!("{poll_prefix}/{}", operation_name(&op));
        let mut backoff = Backoff::new(WAIT_INITIAL, WAIT_INTERVAL);
        loop {
            if !is_done(&op) {
                let name = operation_name(&op).to_owned();
                op = self.get_operation(&poll_path, &name).await?;
            }
            if is_done(&op) {
                return match op.get("error").filter(|e| e.is_object()) {
                    Some(error) => Err(Box::new(VertexError::from_operation_error(error))),
                    None => Ok(op),
                };
            }
            tokio::time::sleep(backoff.pause()).await;
        }
    }

    /// The REST `GetOperation` call: `GET {origin}{path}` with
    /// no query, routing header `name={name}`, under a 10 s deadline over the call and its
    /// retries; a 503 is retried after a pause (500 ms, ×2, cap 10 s). Once the deadline
    /// passes, the poll fails with `context deadline exceeded`.
    async fn get_operation(&self, path: &str, name: &str) -> Result<Value, BoxError> {
        let url = format!("{}{}", self.origin, go_escaped_path(path));
        let deadline = Instant::now() + POLL_TIMEOUT;
        let mut backoff = Backoff::new(POLL_RETRY_INITIAL, POLL_RETRY_MAX);
        loop {
            let result = self
                .send(
                    Method::GET,
                    &url,
                    Some(("name", name)),
                    None,
                    CallTimeout::Call(deadline),
                )
                .await;
            match result {
                Err(e)
                    if e.downcast_ref::<VertexError>()
                        .is_some_and(|v| v.status == 503) =>
                {
                    let wake = Instant::now() + backoff.pause();
                    // Past the deadline the next send fails without a request.
                    tokio::time::sleep_until(wake.min(deadline)).await;
                }
                other => return other,
            }
        }
    }

    /// Sends one request to `url` and reads the reply as [`call`](Self::call) describes.
    async fn send(
        &self,
        method: Method,
        url: &str,
        routing: Option<(&str, &str)>,
        body: Option<&Value>,
        timeout: CallTimeout,
    ) -> Result<Value, BoxError> {
        let body = match body {
            Some(body) => Bytes::from(serde_json::to_vec(body)?),
            None => Bytes::new(),
        };
        let payload = Payload {
            body,
            content_type: JSON,
            extra: &[],
        };
        self.send_bytes(method, url, routing, payload, timeout)
            .await
    }

    /// Sends `payload` to `url` with the call headers and reads the reply: a 2xx reply is its
    /// body as JSON (`{}` when empty); any other status is `VertexError::from_response`.
    async fn send_bytes(
        &self,
        method: Method,
        url: &str,
        routing: Option<(&str, &str)>,
        payload: Payload<'_>,
        timeout: CallTimeout,
    ) -> Result<Value, BoxError> {
        let uri: Uri = url.parse()?;
        let headers = CallHeaders {
            credentials: self.credentials.as_ref(),
            routing,
            content_type: payload.content_type,
            extra: payload.extra,
        };
        let reply = self
            .transport
            .send_method(method, &uri, payload.body, false, &headers, timeout)
            .await?;
        let RestReply::Read(parts, body) = reply else {
            return Err("unexpected streaming reply".into());
        };
        if !parts.status.is_success() {
            return Err(Box::new(VertexError::from_response(
                parts.status.as_u16(),
                &body,
            )));
        }
        if body.is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        Ok(serde_json::from_slice(&body)?)
    }

    /// A multipart upload (the RAG Engine `ragFiles:upload` call): `POST
    /// {origin}/upload{go_escaped_path(path)}` with no query, `Content-Type:
    /// multipart/form-data; boundary=gadk-<32 random hex digits>`, `X-Goog-Upload-Protocol:
    /// multipart`, and the routing, User-Agent and credential headers of
    /// [`call`](Self::call) (no routing header when `routing.0` is empty). The body is
    /// [`multipart_body`]. The reply is read as [`call`](Self::call) reads it. No retries.
    ///
    /// # Errors
    ///
    /// A [`VertexError`] for a non-2xx status; the [`RestError`] when no response was read; a
    /// `serde_json::Error` when the metadata or reply cannot be (de)serialized.
    pub(crate) async fn upload_multipart(
        &self,
        path: &str,
        routing: (&str, &str),
        metadata: &Value,
        file_name: &str,
        file_content_type: &str,
        file: &[u8],
    ) -> Result<Value, BoxError> {
        let boundary = format!("gadk-{:032x}", rand::random::<u128>());
        let body = multipart_body(&boundary, metadata, file_name, file_content_type, file)?;
        let url = format!("{}/upload{}", self.origin, go_escaped_path(path));
        let content_type = format!("multipart/form-data; boundary={boundary}");
        let extra = [(
            X_GOOG_UPLOAD_PROTOCOL,
            HeaderValue::from_static("multipart"),
        )];
        let routing = (!routing.0.is_empty()).then_some(routing);
        let payload = Payload {
            body: Bytes::from(body),
            content_type: &content_type,
            extra: &extra,
        };
        self.send_bytes(Method::POST, &url, routing, payload, self.call_timeout())
            .await
    }
}

/// An operation's `name` (`""` when absent).
fn operation_name(op: &Value) -> &str {
    op.get("name").and_then(Value::as_str).unwrap_or_default()
}

/// Whether an operation's `done` is `true`.
fn is_done(op: &Value) -> bool {
    op.get("done").and_then(Value::as_bool).unwrap_or(false)
}

/// A `multipart/form-data` body with two parts separated by `boundary`: `metadata`
/// (`metadata` as JSON, `Content-Type: application/json`) and `file` (`file`, sent as
/// `filename="<file_name>"` with `file_content_type`). `"`, CR and LF in `file_name` are
/// percent-encoded (`%22`, `%0D`, `%0A`).
///
/// # Errors
///
/// A `serde_json::Error` when `metadata` cannot be serialized.
fn multipart_body(
    boundary: &str,
    metadata: &Value,
    file_name: &str,
    file_content_type: &str,
    file: &[u8],
) -> Result<Vec<u8>, serde_json::Error> {
    let file_name = file_name
        .replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A");
    let metadata = serde_json::to_vec(metadata)?;
    let mut body = Vec::with_capacity(metadata.len() + file.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\n\
             Content-Type: {JSON}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&metadata);
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{file_name}\"\r\nContent-Type: {file_content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok(body)
}
