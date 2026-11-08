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

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use gadk_model::{BodyStream, HttpClient, ModelError, collect_body};
use google_cloud_auth::credentials::{CacheableResource, Credentials};
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method, Uri};
use tokio::time::Instant;

use crate::error::{AnthropicError, api_error_from_response, error_chain, go_quote};
use crate::retry::{header_str, retry_delay, should_retry, should_retry_dropped_body};
use crate::wire::{MessageNewParams, VertexBody, encode_request};

/// The Anthropic backend's default base URL (SDK `WithEnvironmentProduction`).
pub(crate) const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/";

/// The `Anthropic-Version` header value.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Go's default client `ResponseHeaderTimeout` (`defaultResponseHeaderTimeout`).
pub(crate) const DEFAULT_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(600);

/// The Messages API path, relative to the base URL (SDK `"v1/messages"`).
const MESSAGES_PATH: &str = "v1/messages";

/// Go's `http.Transport` error when `ResponseHeaderTimeout` expires.
const RESPONSE_HEADER_TIMEOUT_TEXT: &str = "net/http: timeout awaiting response headers";

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
pub(crate) fn vertex_base_url(location: &str) -> String {
    match location {
        "global" => "https://aiplatform.googleapis.com/".to_string(),
        "us" => "https://aiplatform.us.rep.googleapis.com/".to_string(),
        "eu" => "https://aiplatform.eu.rep.googleapis.com/".to_string(),
        region => format!("https://{region}-aiplatform.googleapis.com/"),
    }
}

/// `v1/messages` resolved against `base_url` as the SDK does: `option.WithBaseURL` gives a
/// non-empty path a trailing `/`, then `url.Parse` + `ResolveReference` keep the base's path as
/// a prefix and drop its query and fragment.
fn messages_url(base_url: &str) -> Result<url::Url, AnthropicError> {
    let parse_err = |e: url::ParseError| {
        AnthropicError::request_failed(format!(
            "requestoption: WithBaseURL failed to parse url {e}"
        ))
    };
    let mut base = url::Url::parse(base_url).map_err(parse_err)?;
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.join(MESSAGES_PATH).map_err(parse_err)
}

/// Whether the SDK's Vertex middleware rewrites a request on `url`: only when its path is
/// exactly `/v1/messages` (`r.URL.Path == "/v1/messages"`), which a base URL with a path prefix
/// never gives.
fn vertex_rewrites(url: &url::Url) -> bool {
    url.path() == "/v1/messages"
}

/// The request URL: the endpoint's API path joined to `base_url` with exactly one `/`
/// (a base URL's own path is kept as a prefix).
///
/// On Vertex the SDK sends `v1/messages` through its middleware, which rewrites the path to
/// `/v1/projects/{p}/locations/{l}/publishers/anthropic/models/{model}:rawPredict`
/// (`:streamRawPredict` when streaming) only when the resolved path is exactly `/v1/messages`;
/// with a base URL that carries a path prefix the request stays at `{prefix}/v1/messages`
/// (and [`Transport::encode_body`] keeps `model` in the body), as in Go.
pub(crate) fn request_url(
    base_url: &str,
    endpoint: &Endpoint,
    model_id: &str,
    stream: bool,
) -> Result<String, AnthropicError> {
    let mut url = messages_url(base_url)?;
    if let Endpoint::Vertex { project, location } = endpoint
        && vertex_rewrites(&url)
    {
        let specifier = if stream {
            "streamRawPredict"
        } else {
            "rawPredict"
        };
        url.set_path(&format!(
            "/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model_id}:{specifier}"
        ));
    }
    Ok(url.into())
}

/// A header value marked sensitive; an invalid one fails the attempt as Go's `net/http` does
/// (a transport error).
fn sensitive_value(name: &str, value: &str) -> Result<HeaderValue, AnthropicError> {
    let mut v = HeaderValue::from_str(value).map_err(|_| {
        AnthropicError::transport(format!(
            "net/http: invalid header field value for {}",
            go_quote(name)
        ))
    })?;
    v.set_sensitive(true);
    Ok(v)
}

/// The credentials' headers (Go's `oauth2.Transport`); a failure is a transport error.
async fn credential_headers(credentials: &Credentials) -> Result<HeaderMap, AnthropicError> {
    match credentials.headers(http::Extensions::new()).await {
        Ok(CacheableResource::New { data, .. }) => Ok(data),
        // Only returned when the caller passes an entity tag, which gadk never does.
        Ok(CacheableResource::NotModified) => {
            Err(AnthropicError::transport("credentials returned no headers"))
        }
        Err(e) => Err(AnthropicError::transport(error_chain(&e))),
    }
}

/// Go `requestconfig`'s `error reading response body: %w`, classified by `classifyError` (not a
/// `*url.Error`, so `request failed`).
fn body_read_error(err: ModelError) -> AnthropicError {
    AnthropicError::from_body_error(err, "error reading response body: ")
}

/// A streaming body still under the attempt's deadline (SDK `bodyWithTimeout`): once the
/// deadline passes, the next poll yields
/// `ModelError::Other(AnthropicError::deadline_exceeded())` and the body ends (it is dropped,
/// which closes the connection).
fn body_with_deadline(body: BodyStream, deadline: Instant) -> BodyStream {
    let sleep = Box::pin(tokio::time::sleep_until(deadline));
    futures::stream::unfold(Some((body, sleep)), |state| async move {
        let (mut body, mut sleep) = state?;
        tokio::select! {
            biased;
            chunk = body.next() => chunk.map(|c| (c, Some((body, sleep)))),
            () = &mut sleep => Some((
                Err(ModelError::from(AnthropicError::deadline_exceeded())),
                None,
            )),
        }
    })
    .boxed()
}

/// What one attempt produced.
enum Attempt {
    /// No response: a connection error, a credential failure, or the response-header timeout.
    NoResponse(AnthropicError),
    /// The attempt's `timeout` expired.
    TimedOut,
    /// A response that ends the call, its body read.
    Read(http::response::Parts, Bytes),
    /// A response that ends the call whose body could not be read.
    DroppedBody(http::response::Parts, AnthropicError),
    /// A response left unread: one that is retried, or a streaming 2xx that ends the call.
    Unread(http::Response<BodyStream>),
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

impl Transport {
    /// The headers of one attempt, in Go's precedence (a later step replaces an earlier one's
    /// values for the same name):
    /// 1. `Accept: application/json`, `Anthropic-Version`, `Content-Type: application/json`;
    /// 2. Anthropic: `x-api-key` and/or `Authorization: Bearer` (SDK `WithAPIKey` /
    ///    `WithAuthToken`); Vertex: `x-goog-api-client` and `user-agent` set to
    ///    `gadk_platform::tracking_header()`;
    /// 3. [`headers`](Self::headers) (Go's `option.WithHeader`, applied after the above);
    /// 4. Vertex: the credentials' headers, fetched now (Go's `oauth2.Transport` sets
    ///    `Authorization` on the outgoing request last).
    pub(crate) async fn request_headers(&self) -> Result<HeaderMap, AnthropicError> {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match &self.auth {
            Auth::Anthropic {
                api_key,
                auth_token,
            } => {
                if let Some(key) = api_key {
                    h.insert("x-api-key", sensitive_value("X-Api-Key", key)?);
                }
                if let Some(token) = auth_token {
                    h.insert(
                        AUTHORIZATION,
                        sensitive_value("Authorization", &format!("Bearer {token}"))?,
                    );
                }
                h.extend(self.headers.clone());
            }
            Auth::Vertex(credentials) => {
                let tracking = HeaderValue::from_str(&gadk_platform::tracking_header())
                    .map_err(|e| AnthropicError::transport(e.to_string()))?;
                h.insert("x-goog-api-client", tracking.clone());
                h.insert(USER_AGENT, tracking);
                h.extend(self.headers.clone());
                h.extend(credential_headers(credentials).await?);
            }
        }
        Ok(h)
    }

    /// The request body the SDK sends for `params` (whose `model` is set): [`encode_request`],
    /// on Vertex dropping `model` only when the middleware rewrites the path (see
    /// [`request_url`]); with a base URL whose path it does not rewrite `model` stays and only
    /// `,"anthropic_version":"vertex-2023-10-16"` is appended, as the SDK's middleware does.
    pub(crate) fn encode_body(
        &self,
        params: MessageNewParams,
        stream: bool,
    ) -> Result<Bytes, AnthropicError> {
        let vertex = match &self.endpoint {
            Endpoint::Anthropic => None,
            Endpoint::Vertex { .. } if vertex_rewrites(&messages_url(&self.base_url)?) => {
                Some(VertexBody::DropModel)
            }
            Endpoint::Vertex { .. } => Some(VertexBody::KeepModel),
        };
        encode_request(params, stream, vertex)
            .map(Bytes::from)
            .map_err(AnthropicError::request_failed)
    }

    /// One attempt, up to the point Go's loop decides on it: send (under
    /// `response_header_timeout`), then read the body of a response that ends the call unless
    /// it is a streaming 2xx (SDK `readsBody`).
    async fn attempt(&self, uri: &Uri, body: &Bytes, stream: bool, last_attempt: bool) -> Attempt {
        let headers = match self.request_headers().await {
            Ok(h) => h,
            Err(e) => return Attempt::NoResponse(e),
        };
        let mut req = http::Request::new(body.clone());
        *req.method_mut() = Method::POST;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = headers;
        let sent = match self.response_header_timeout {
            Some(limit) => match tokio::time::timeout(limit, self.http.send(req)).await {
                Ok(sent) => sent,
                Err(_) => {
                    return Attempt::NoResponse(AnthropicError::transport(
                        RESPONSE_HEADER_TIMEOUT_TEXT,
                    ));
                }
            },
            None => self.http.send(req).await,
        };
        let res = match sent {
            Ok(res) => res,
            Err(e) => return Attempt::NoResponse(AnthropicError::from_model_error(e)),
        };
        let ends_call = last_attempt || !should_retry(res.status(), res.headers());
        let reads_body = res.status().as_u16() >= 400 || !stream;
        if !(ends_call && reads_body) {
            return Attempt::Unread(res);
        }
        let (parts, body) = res.into_parts();
        match collect_body(body).await {
            Ok(bytes) => Attempt::Read(parts, bytes),
            Err(e) => Attempt::DroppedBody(parts, body_read_error(e)),
        }
    }

    /// Sends `body` with the SDK's retry policy and returns the response that ends the call.
    ///
    /// - Attempts: `max_retries + 1`. A connection error (including a credential failure and
    ///   the response-header timeout), or `timeout` (> 0, per attempt) expiring, is retried like
    ///   a connection error; [`AnthropicError::deadline_exceeded`] surfaces only when the last
    ///   attempt timed out.
    /// - A response is retried per [`crate::retry::should_retry`] (a 2xx with
    ///   `x-should-retry: true` included), unread; waits use [`crate::retry::retry_delay`] (the
    ///   response's headers, or none after a connection error or timeout).
    /// - The body of a response that ends the call is read inside the attempt (under its
    ///   `timeout`) unless it is a streaming 2xx; a body that fails to read is retried per
    ///   [`crate::retry::should_retry_dropped_body`] (its `Retry-After` still counts), else
    ///   returned as `anthropicmodel: request failed: error reading response body: …`.
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
        let url = request_url(&self.base_url, &self.endpoint, model_id, stream)?;
        let uri: Uri = url.parse().map_err(AnthropicError::request_failed)?;
        let timeout = timeout.filter(|t| !t.is_zero());
        let mut retry_count: u32 = 0;
        loop {
            let last_attempt = retry_count >= self.max_retries;
            let deadline = timeout.map(|t| Instant::now() + t);
            let attempt = self.attempt(&uri, &body, stream, last_attempt);
            let outcome = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, attempt)
                    .await
                    .unwrap_or(Attempt::TimedOut),
                None => attempt.await,
            };
            let retry_headers = match outcome {
                Attempt::NoResponse(err) => {
                    if last_attempt {
                        return Err(err);
                    }
                    None
                }
                Attempt::TimedOut => {
                    if last_attempt {
                        return Err(AnthropicError::deadline_exceeded());
                    }
                    None
                }
                Attempt::DroppedBody(parts, err) => {
                    if last_attempt || !should_retry_dropped_body(&parts.headers) {
                        return Err(err);
                    }
                    Some(parts.headers)
                }
                Attempt::Read(parts, bytes) => {
                    if parts.status.as_u16() >= 400 {
                        return Err(AnthropicError::api(api_error_from_response(
                            parts.status.as_u16(),
                            header_str(&parts.headers, "request-id"),
                            &bytes,
                        )));
                    }
                    let body: BodyStream = futures::stream::iter([Ok(bytes)]).boxed();
                    return Ok(http::Response::from_parts(parts, body));
                }
                Attempt::Unread(res) => {
                    if last_attempt || !should_retry(res.status(), res.headers()) {
                        return Ok(match deadline {
                            Some(deadline) => res.map(|b| body_with_deadline(b, deadline)),
                            None => res,
                        });
                    }
                    // The response is dropped unread, which closes its connection.
                    Some(res.headers().clone())
                }
            };
            tokio::time::sleep(retry_delay(retry_headers.as_ref(), retry_count)).await;
            retry_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use google_cloud_auth::credentials::{CredentialsProvider, EntityTag};
    use google_cloud_auth::errors::CredentialsError;
    use http::StatusCode;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::error::ErrorKind;

    // ------------------------------------------------------------ scripted fake HttpClient

    /// One scripted answer of [`Fake`].
    #[derive(Clone)]
    enum Reply {
        /// A response; `body` chunks are sent in order (`Err` is a body error that ends it),
        /// then the body ends, or stays open forever when `stall`.
        Respond {
            status: u16,
            headers: Vec<(&'static str, &'static str)>,
            body: Vec<Result<&'static str, &'static str>>,
            stall: bool,
        },
        /// A connection error (no response).
        ConnErr(&'static str),
        /// Never answers.
        Hang,
    }

    fn reply(status: u16, body: &'static str) -> Reply {
        Reply::Respond {
            status,
            headers: vec![("content-type", "application/json")],
            body: vec![Ok(body)],
            stall: false,
        }
    }

    impl Reply {
        fn header(mut self, name: &'static str, value: &'static str) -> Self {
            if let Reply::Respond { headers, .. } = &mut self {
                headers.push((name, value));
            }
            self
        }
        /// The body yields its first chunk, then fails as a dropped connection does.
        fn dropped(mut self) -> Self {
            if let Reply::Respond { body, .. } = &mut self {
                body.push(Err("unexpected EOF"));
            }
            self
        }
        /// The body yields its chunks, then stays open.
        fn stalled(mut self) -> Self {
            if let Reply::Respond { stall, .. } = &mut self {
                *stall = true;
            }
            self
        }
    }

    /// What [`Fake`] saw of one request.
    struct Seen {
        method: Method,
        uri: String,
        headers: HeaderMap,
        body: Bytes,
        at: Instant,
    }

    /// An [`HttpClient`] answering from a script (the last reply repeats).
    struct Fake {
        script: Mutex<VecDeque<Reply>>,
        seen: Mutex<Vec<Seen>>,
        /// Set when a hanging send is dropped.
        hang_dropped: Arc<AtomicBool>,
    }

    impl Fake {
        fn new(script: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                seen: Mutex::new(Vec::new()),
                hang_dropped: Arc::new(AtomicBool::new(false)),
            })
        }
        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
        /// The time between request `i` and request `i + 1`.
        fn gap(&self, i: usize) -> Duration {
            let seen = self.seen.lock().unwrap();
            seen[i + 1].at - seen[i].at
        }
        fn next_reply(&self) -> Reply {
            let mut script = self.script.lock().unwrap();
            if script.len() > 1 {
                script.pop_front().unwrap()
            } else {
                script.front().cloned().unwrap()
            }
        }
    }

    /// Sets its flag when dropped.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn io_err(msg: &str) -> ModelError {
        ModelError::Transport(Box::new(std::io::Error::other(msg.to_string())))
    }

    // `HttpClient` is an `async_trait`; gadk-anthropic does not depend on `async-trait`, so the
    // fake spells out the method the macro generates.
    impl HttpClient for Fake {
        fn send<'life0, 'async_trait>(
            &'life0 self,
            req: http::Request<Bytes>,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<http::Response<BodyStream>, ModelError>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let (parts, body) = req.into_parts();
                self.seen.lock().unwrap().push(Seen {
                    method: parts.method,
                    uri: parts.uri.to_string(),
                    headers: parts.headers,
                    body,
                    at: Instant::now(),
                });
                match self.next_reply() {
                    Reply::ConnErr(msg) => Err(io_err(msg)),
                    Reply::Hang => {
                        let _flag = DropFlag(self.hang_dropped.clone());
                        futures::future::pending::<()>().await;
                        unreachable!()
                    }
                    Reply::Respond {
                        status,
                        headers,
                        body,
                        stall,
                    } => {
                        let chunks: Vec<Result<Bytes, ModelError>> = body
                            .into_iter()
                            .map(|c| c.map(|s| Bytes::from_static(s.as_bytes())).map_err(io_err))
                            .collect();
                        let mut stream: BodyStream = futures::stream::iter(chunks).boxed();
                        if stall {
                            stream = stream.chain(futures::stream::pending()).boxed();
                        }
                        let mut res = http::Response::new(stream);
                        *res.status_mut() = StatusCode::from_u16(status).unwrap();
                        for (k, v) in headers {
                            res.headers_mut().append(k, HeaderValue::from_static(v));
                        }
                        Ok(res)
                    }
                }
            })
        }
    }

    /// A credentials provider returning fixed headers, or failing; counts its calls.
    #[derive(Debug)]
    struct TestCreds {
        headers: Option<HeaderMap>,
        calls: Arc<AtomicUsize>,
    }

    impl CredentialsProvider for TestCreds {
        async fn headers(
            &self,
            _extensions: http::Extensions,
        ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.headers {
                Some(h) => Ok(CacheableResource::New {
                    entity_tag: EntityTag::new(),
                    data: h.clone(),
                }),
                None => Err(CredentialsError::from_msg(false, "boom")),
            }
        }

        async fn universe_domain(&self) -> Option<String> {
            None
        }
    }

    fn vertex_creds(headers: Option<HeaderMap>) -> (Credentials, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let creds = Credentials::from(TestCreds {
            headers,
            calls: calls.clone(),
        });
        (creds, calls)
    }

    fn bearer(token: &'static str) -> HeaderMap {
        HeaderMap::from_iter([(AUTHORIZATION, HeaderValue::from_static(token))])
    }

    fn anthropic(http: Arc<Fake>, max_retries: u32) -> Transport {
        Transport {
            http,
            base_url: ANTHROPIC_DEFAULT_BASE_URL.to_string(),
            endpoint: Endpoint::Anthropic,
            auth: Auth::Anthropic {
                api_key: Some("sk-test".to_string()),
                auth_token: None,
            },
            headers: HeaderMap::new(),
            max_retries,
            response_header_timeout: None,
        }
    }

    fn vertex(http: Arc<Fake>, credentials: Credentials, max_retries: u32) -> Transport {
        Transport {
            http,
            base_url: vertex_base_url("us-east5"),
            endpoint: Endpoint::Vertex {
                project: "p".to_string(),
                location: "us-east5".to_string(),
            },
            auth: Auth::Vertex(credentials),
            headers: HeaderMap::new(),
            max_retries,
            response_header_timeout: None,
        }
    }

    const BODY: &[u8] = br#"{"max_tokens":16,"model":"claude-x"}"#;
    const OK: &str = r#"{"id":"msg_1"}"#;
    const BAD: &str =
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad thing"}}"#;
    const BOOM: &str = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;

    async fn send(
        t: &Transport,
        stream: bool,
        timeout: Option<Duration>,
    ) -> Result<Bytes, AnthropicError> {
        let res = t
            .send("claude-x", Bytes::from_static(BODY), stream, timeout)
            .await?;
        Ok(collect_body(res.into_body()).await.unwrap())
    }

    /// Asserts `d` is a jittered backoff for retry `n` plus `extra`.
    fn assert_backoff(d: Duration, n: u32, extra: Duration) {
        let base = Duration::from_millis(500) * 2u32.pow(n);
        let (lo, hi) = (extra + base - base / 4, extra + base);
        assert!(d > lo && d <= hi, "gap {d:?} not in ({lo:?}, {hi:?}]");
    }

    fn sorted(h: &HeaderMap) -> Vec<(String, String)> {
        let mut v: Vec<_> = h
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        v.sort();
        v
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<_> = p
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        v.sort();
        v
    }

    // ------------------------------------------------------------ endpoints

    #[test]
    fn vertex_base_url_hosts() {
        let cases = [
            ("global", "https://aiplatform.googleapis.com/"),
            ("us", "https://aiplatform.us.rep.googleapis.com/"),
            ("eu", "https://aiplatform.eu.rep.googleapis.com/"),
            ("us-east5", "https://us-east5-aiplatform.googleapis.com/"),
            (
                "europe-west1",
                "https://europe-west1-aiplatform.googleapis.com/",
            ),
        ];
        for (location, want) in cases {
            assert_eq!(vertex_base_url(location), want, "{location}");
        }
    }

    #[test]
    fn request_url_cases() {
        let vertex = Endpoint::Vertex {
            project: "proj".to_string(),
            location: "us-east5".to_string(),
        };
        let global = Endpoint::Vertex {
            project: "proj".to_string(),
            location: "global".to_string(),
        };
        let cases: [(&str, &str, &Endpoint, bool, &str); 13] = [
            (
                "anthropic_default",
                ANTHROPIC_DEFAULT_BASE_URL,
                &Endpoint::Anthropic,
                false,
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "anthropic_streaming_same_path",
                ANTHROPIC_DEFAULT_BASE_URL,
                &Endpoint::Anthropic,
                true,
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "anthropic_host_without_slash",
                "http://127.0.0.1:8080",
                &Endpoint::Anthropic,
                false,
                "http://127.0.0.1:8080/v1/messages",
            ),
            (
                "anthropic_prefix_without_slash",
                "https://proxy.example/anthropic",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/anthropic/v1/messages",
            ),
            (
                "anthropic_prefix_with_slash",
                "https://proxy.example/anthropic/",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/anthropic/v1/messages",
            ),
            (
                "anthropic_query_and_fragment_dropped",
                "https://proxy.example/a?x=1#f",
                &Endpoint::Anthropic,
                false,
                "https://proxy.example/a/v1/messages",
            ),
            (
                "vertex_regional",
                "https://us-east5-aiplatform.googleapis.com/",
                &vertex,
                false,
                "https://us-east5-aiplatform.googleapis.com/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_regional_streaming",
                "https://us-east5-aiplatform.googleapis.com/",
                &vertex,
                true,
                "https://us-east5-aiplatform.googleapis.com/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict",
            ),
            (
                "vertex_global",
                "https://aiplatform.googleapis.com/",
                &global,
                false,
                "https://aiplatform.googleapis.com/v1/projects/proj/locations/global/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_custom_host",
                "http://127.0.0.1:9000",
                &vertex,
                false,
                "http://127.0.0.1:9000/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:rawPredict",
            ),
            (
                "vertex_custom_host_with_slash",
                "http://127.0.0.1:9000/",
                &vertex,
                true,
                "http://127.0.0.1:9000/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict",
            ),
            // R-a: the SDK's Vertex middleware rewrites only the path `/v1/messages`, so a
            // prefixed base URL keeps `{prefix}/v1/messages` (and `model` in the body).
            (
                "vertex_prefixed_base_url_is_not_rewritten",
                "https://proxy.example/vertex",
                &vertex,
                false,
                "https://proxy.example/vertex/v1/messages",
            ),
            (
                "vertex_prefixed_base_url_streaming_is_not_rewritten",
                "https://proxy.example/vertex/",
                &vertex,
                true,
                "https://proxy.example/vertex/v1/messages",
            ),
        ];
        for (name, base, endpoint, stream, want) in cases {
            let got = request_url(base, endpoint, "claude-x", stream).unwrap();
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn request_url_rejects_an_unparsable_base_url() {
        let err = request_url("not a url", &Endpoint::Anthropic, "m", false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: requestoption: WithBaseURL failed to parse url relative URL without a base"
        );
    }

    #[test]
    fn encode_body_per_endpoint() {
        let params = MessageNewParams {
            max_tokens: 16,
            model: "claude-x".to_string(),
            ..Default::default()
        };
        let fake = Fake::new(vec![reply(200, OK)]);
        let (creds, _) = vertex_creds(Some(HeaderMap::new()));
        let anthropic_t = anthropic(fake.clone(), 0);
        let vertex_t = vertex(fake.clone(), creds.clone(), 0);
        let mut custom_host = vertex(fake.clone(), creds.clone(), 0);
        custom_host.base_url = "http://127.0.0.1:9000".to_string();
        let mut prefixed = vertex(fake, creds, 0);
        prefixed.base_url = "https://proxy.example/vertex".to_string();
        let cases: [(&str, &Transport, bool, &str); 7] = [
            (
                "anthropic",
                &anthropic_t,
                false,
                r#"{"max_tokens":16,"model":"claude-x"}"#,
            ),
            (
                "anthropic_stream",
                &anthropic_t,
                true,
                r#"{"max_tokens":16,"model":"claude-x","stream":true}"#,
            ),
            (
                "vertex",
                &vertex_t,
                false,
                r#"{"max_tokens":16,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_stream",
                &vertex_t,
                true,
                r#"{"max_tokens":16,"stream":true,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_custom_host",
                &custom_host,
                false,
                r#"{"max_tokens":16,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_prefixed_keeps_model",
                &prefixed,
                false,
                r#"{"max_tokens":16,"model":"claude-x","anthropic_version":"vertex-2023-10-16"}"#,
            ),
            (
                "vertex_prefixed_stream_keeps_model",
                &prefixed,
                true,
                r#"{"max_tokens":16,"model":"claude-x","stream":true,"anthropic_version":"vertex-2023-10-16"}"#,
            ),
        ];
        for (name, t, stream, want) in cases {
            let got = t.encode_body(params.clone(), stream).unwrap();
            assert_eq!(std::str::from_utf8(&got).unwrap(), want, "{name}");
        }
    }

    // ------------------------------------------------------------ headers

    /// Header name/value pairs.
    type Pairs = &'static [(&'static str, &'static str)];

    const BASE_HEADERS: [(&str, &str); 3] = [
        ("accept", "application/json"),
        ("anthropic-version", "2023-06-01"),
        ("content-type", "application/json"),
    ];

    #[tokio::test]
    async fn request_headers_anthropic_auth() {
        let cases: [(&str, Option<&str>, Option<&str>, Pairs); 4] = [
            ("api_key", Some("sk-1"), None, &[("x-api-key", "sk-1")]),
            (
                "auth_token",
                None,
                Some("tok"),
                &[("authorization", "Bearer tok")],
            ),
            (
                "both_headers_when_both_set",
                Some("sk-1"),
                Some("tok"),
                &[("x-api-key", "sk-1"), ("authorization", "Bearer tok")],
            ),
            ("neither", None, None, &[]),
        ];
        for (name, api_key, auth_token, auth) in cases {
            let mut t = anthropic(Fake::new(vec![reply(200, OK)]), 0);
            t.auth = Auth::Anthropic {
                api_key: api_key.map(str::to_string),
                auth_token: auth_token.map(str::to_string),
            };
            let got = t.request_headers().await.unwrap();
            let mut want = BASE_HEADERS.to_vec();
            want.extend_from_slice(auth);
            assert_eq!(sorted(&got), pairs(&want), "{name}");
            for name in ["x-api-key", "authorization"] {
                if let Some(v) = got.get(name) {
                    assert!(v.is_sensitive(), "{name} must be sensitive");
                }
            }
        }
    }

    #[tokio::test]
    async fn request_headers_user_headers_replace_and_add() {
        let mut t = anthropic(Fake::new(vec![reply(200, OK)]), 0);
        t.headers
            .append("anthropic-version", HeaderValue::from_static("2099-01-01"));
        t.headers.append("x-extra", HeaderValue::from_static("a"));
        t.headers.append("x-extra", HeaderValue::from_static("b"));
        t.headers
            .append("x-api-key", HeaderValue::from_static("override"));
        let got = t.request_headers().await.unwrap();
        assert_eq!(
            sorted(&got),
            pairs(&[
                ("accept", "application/json"),
                ("anthropic-version", "2099-01-01"),
                ("content-type", "application/json"),
                ("x-api-key", "override"),
                ("x-extra", "a"),
                ("x-extra", "b"),
            ])
        );
    }

    #[tokio::test]
    async fn request_headers_invalid_api_key_is_a_transport_error() {
        let mut t = anthropic(Fake::new(vec![reply(200, OK)]), 0);
        t.auth = Auth::Anthropic {
            api_key: Some("a\nb".to_string()),
            auth_token: None,
        };
        let err = t.request_headers().await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            r#"anthropicmodel: transport error: net/http: invalid header field value for "X-Api-Key""#
        );
    }

    #[tokio::test]
    async fn request_headers_vertex() {
        let mut creds_headers = bearer("Bearer vertex-token");
        creds_headers.insert("x-goog-user-project", HeaderValue::from_static("quota"));
        let (creds, calls) = vertex_creds(Some(creds_headers));
        let mut t = vertex(Fake::new(vec![reply(200, OK)]), creds, 0);
        t.headers.insert("x-extra", HeaderValue::from_static("1"));
        let got = t.request_headers().await.unwrap();
        let tracking = gadk_platform::tracking_header();
        let mut want = BASE_HEADERS.to_vec();
        want.extend_from_slice(&[
            ("authorization", "Bearer vertex-token"),
            ("x-goog-user-project", "quota"),
            ("x-goog-api-client", tracking.as_str()),
            ("user-agent", tracking.as_str()),
            ("x-extra", "1"),
        ]);
        assert_eq!(sorted(&got), pairs(&want));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!got.contains_key("x-api-key"));
    }

    #[tokio::test]
    async fn request_headers_vertex_precedence_follows_go() {
        // User headers replace the tracking headers (Go's WithHeader order), and the
        // credentials' Authorization replaces a user one (Go's oauth2.Transport runs last).
        let (creds, _) = vertex_creds(Some(bearer("Bearer vertex-token")));
        let mut t = vertex(Fake::new(vec![reply(200, OK)]), creds, 0);
        t.headers
            .insert(USER_AGENT, HeaderValue::from_static("mine"));
        t.headers
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer user"));
        let got = t.request_headers().await.unwrap();
        assert_eq!(got.get(USER_AGENT).unwrap(), "mine");
        assert_eq!(got.get(AUTHORIZATION).unwrap(), "Bearer vertex-token");
        assert_eq!(
            got.get("x-goog-api-client").unwrap(),
            gadk_platform::tracking_header().as_str()
        );
    }

    #[tokio::test]
    async fn request_headers_vertex_credential_failure() {
        let (creds, _) = vertex_creds(None);
        let t = vertex(Fake::new(vec![reply(200, OK)]), creds, 0);
        let err = t.request_headers().await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: boom and future attempts will not succeed"
        );
    }

    // ------------------------------------------------------------ send: success and wire

    #[tokio::test(start_paused = true)]
    async fn send_first_attempt_success_wire() {
        let fake = Fake::new(vec![reply(200, OK)]);
        let t = anthropic(fake.clone(), 2);
        let res = t
            .send("claude-x", Bytes::from_static(BODY), false, None)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get(CONTENT_TYPE).unwrap(), "application/json");
        assert_eq!(collect_body(res.into_body()).await.unwrap(), OK.as_bytes());
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, Method::POST);
        assert_eq!(seen[0].uri, "https://api.anthropic.com/v1/messages");
        assert_eq!(seen[0].body, BODY);
        assert_eq!(seen[0].headers.get("x-api-key").unwrap(), "sk-test");
        assert_eq!(
            seen[0].headers.get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        for h in seen[0].headers.keys() {
            assert!(
                !h.as_str().starts_with("x-stainless"),
                "{h} must not be sent"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_vertex_wire() {
        let (creds, calls) = vertex_creds(Some(bearer("Bearer vertex-token")));
        let fake = Fake::new(vec![reply(500, BOOM), reply(200, OK)]);
        let t = vertex(fake.clone(), creds, 2);
        send(&t, true, None).await.unwrap();
        assert_eq!(fake.calls(), 2);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "credentials fetched per attempt"
        );
        let seen = fake.seen.lock().unwrap();
        assert_eq!(
            seen[1].uri,
            "https://us-east5-aiplatform.googleapis.com/v1/projects/p/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict"
        );
        assert_eq!(
            seen[1].headers.get(AUTHORIZATION).unwrap(),
            "Bearer vertex-token"
        );
        assert_eq!(
            seen[1].headers.get(USER_AGENT).unwrap(),
            gadk_platform::tracking_header().as_str()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_resends_the_same_body_and_headers() {
        let fake = Fake::new(vec![reply(503, BOOM), reply(503, BOOM), reply(200, OK)]);
        let t = anthropic(fake.clone(), 2);
        send(&t, false, None).await.unwrap();
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for s in seen.iter() {
            assert_eq!(s.body, BODY);
            assert_eq!(sorted(&s.headers), sorted(&seen[0].headers));
        }
    }

    // ------------------------------------------------------------ send: status rules

    #[tokio::test(start_paused = true)]
    async fn send_retries_retryable_statuses() {
        for status in [408u16, 409, 429, 500, 502, 503, 529] {
            let fake = Fake::new(vec![reply(status, BOOM), reply(200, OK)]);
            let got = send(&anthropic(fake.clone(), 2), false, None)
                .await
                .unwrap();
            assert_eq!(got, OK.as_bytes(), "{status}");
            assert_eq!(fake.calls(), 2, "{status}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_does_not_retry_other_statuses() {
        let cases = [
            (400u16, ErrorKind::BadRequest),
            (401, ErrorKind::Authentication),
            (403, ErrorKind::Authentication),
            (404, ErrorKind::ApiStatus),
            (422, ErrorKind::ApiStatus),
        ];
        // The error type is neutral, so the kind comes from the status alone.
        let body = r#"{"type":"error","error":{"type":"some_error","message":"bad thing"}}"#;
        for (status, kind) in cases {
            let fake = Fake::new(vec![reply(status, body), reply(200, OK)]);
            let err = send(&anthropic(fake.clone(), 2), false, None)
                .await
                .unwrap_err();
            assert_eq!(fake.calls(), 1, "{status}");
            assert!(err.is(kind), "{status}: {err}");
            let api = err.api_error().unwrap();
            assert_eq!(api.status_code, status);
            assert_eq!(api.message, "bad thing");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_max_retries_0_1_2() {
        for max_retries in [0u32, 1, 2] {
            let fake = Fake::new(vec![reply(500, BOOM)]);
            let err = send(&anthropic(fake.clone(), max_retries), false, None)
                .await
                .unwrap_err();
            assert_eq!(
                fake.calls(),
                max_retries as usize + 1,
                "max_retries {max_retries}"
            );
            assert_eq!(err.api_error().unwrap().status_code, 500);
            assert_eq!(
                err.to_string(),
                "anthropicmodel: api error (status 500) [type=api_error]: boom"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_x_should_retry_overrides_status() {
        // true on a 2xx: retried.
        let fake = Fake::new(vec![
            reply(200, "first").header("x-should-retry", "true"),
            reply(200, OK),
        ]);
        assert_eq!(
            send(&anthropic(fake.clone(), 2), false, None)
                .await
                .unwrap(),
            OK.as_bytes()
        );
        assert_eq!(fake.calls(), 2);

        // true on a 400: retried.
        let fake = Fake::new(vec![
            reply(400, BAD).header("x-should-retry", "true"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        assert_eq!(fake.calls(), 2);

        // false on a 503: not retried.
        let fake = Fake::new(vec![
            reply(503, BOOM).header("x-should-retry", "false"),
            reply(200, OK),
        ]);
        let err = send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.calls(), 1);
        assert_eq!(err.api_error().unwrap().status_code, 503);

        // true on the last attempt: the response is returned.
        let fake = Fake::new(vec![reply(200, "last").header("x-should-retry", "true")]);
        assert_eq!(
            send(&anthropic(fake.clone(), 0), false, None)
                .await
                .unwrap(),
            "last".as_bytes()
        );
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn send_request_id_header_wins() {
        let fake = Fake::new(vec![reply(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"},"request_id":"req_body"}"#,
        )
        .header("request-id", "req_header")]);
        let err = send(&anthropic(fake, 2), false, None).await.unwrap_err();
        assert_eq!(err.api_error().unwrap().request_id, "req_header");
    }

    // ------------------------------------------------------------ send: delays

    #[tokio::test(start_paused = true)]
    async fn send_backoff_bounds_with_jitter() {
        let fake = Fake::new(vec![
            reply(500, BOOM),
            reply(500, BOOM),
            reply(500, BOOM),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 3), false, None)
            .await
            .unwrap();
        assert_eq!(fake.calls(), 4);
        for n in 0..3 {
            assert_backoff(fake.gap(n as usize), n, Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_ms() {
        let fake = Fake::new(vec![
            reply(429, BOOM).header("retry-after-ms", "1234"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        assert_eq!(fake.gap(0), Duration::from_millis(1234));
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_seconds() {
        let fake = Fake::new(vec![
            reply(503, BOOM).header("retry-after", "7"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(7));
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_date() {
        let at = (chrono::Utc::now() + chrono::Duration::seconds(30))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let at: &'static str = Box::leak(at.into_boxed_str());
        let fake = Fake::new(vec![
            reply(503, BOOM).header("retry-after", at),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        let gap = fake.gap(0);
        assert!(
            gap > Duration::from_secs(28) && gap <= Duration::from_secs(30),
            "{gap:?}"
        );
    }

    // ------------------------------------------------------------ send: connection errors

    #[tokio::test(start_paused = true)]
    async fn send_retries_connection_errors() {
        let fake = Fake::new(vec![Reply::ConnErr("connection refused"), reply(200, OK)]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        assert_eq!(fake.calls(), 2);
        assert_backoff(fake.gap(0), 0, Duration::ZERO);

        let fake = Fake::new(vec![Reply::ConnErr("connection refused")]);
        let err = send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.calls(), 3);
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: connection refused"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_retries_credential_failures() {
        let (creds, calls) = vertex_creds(None);
        let fake = Fake::new(vec![reply(200, OK)]);
        let err = send(&vertex(fake.clone(), creds, 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(fake.calls(), 0);
        assert!(err.is_transport());
    }

    #[tokio::test(start_paused = true)]
    async fn send_response_header_timeout() {
        let fake = Fake::new(vec![Reply::Hang, reply(200, OK)]);
        let mut t = anthropic(fake.clone(), 2);
        t.response_header_timeout = Some(DEFAULT_RESPONSE_HEADER_TIMEOUT);
        send(&t, false, None).await.unwrap();
        assert_backoff(fake.gap(0), 0, DEFAULT_RESPONSE_HEADER_TIMEOUT);

        let fake = Fake::new(vec![Reply::Hang]);
        let mut t = anthropic(fake.clone(), 0);
        t.response_header_timeout = Some(DEFAULT_RESPONSE_HEADER_TIMEOUT);
        let err = send(&t, false, None).await.unwrap_err();
        assert!(err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: transport error: net/http: timeout awaiting response headers"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn response_header_timeout_does_not_limit_a_streaming_body() {
        let fake = Fake::new(vec![Reply::Respond {
            status: 200,
            headers: vec![],
            body: vec![Ok("data: a\n\n")],
            stall: true,
        }]);
        let mut t = anthropic(fake, 0);
        t.response_header_timeout = Some(Duration::from_secs(1));
        let res = t
            .send("claude-x", Bytes::from_static(BODY), true, None)
            .await
            .unwrap();
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err(),
            "the body stays open"
        );
    }

    // ------------------------------------------------------------ send: per-attempt timeout

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_is_retried() {
        let fake = Fake::new(vec![Reply::Hang, Reply::Hang, reply(200, OK)]);
        let got = send(
            &anthropic(fake.clone(), 2),
            false,
            Some(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        assert_eq!(got, OK.as_bytes());
        assert_eq!(fake.calls(), 3);
        assert_backoff(fake.gap(0), 0, Duration::from_secs(1));
        assert_backoff(fake.gap(1), 1, Duration::from_secs(1));
        assert!(
            fake.hang_dropped.load(Ordering::SeqCst),
            "timed-out send dropped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_on_last_attempt_is_deadline_exceeded() {
        for max_retries in [0u32, 2] {
            let fake = Fake::new(vec![Reply::Hang]);
            let err = send(
                &anthropic(fake.clone(), max_retries),
                false,
                Some(Duration::from_secs(1)),
            )
            .await
            .unwrap_err();
            assert_eq!(fake.calls(), max_retries as usize + 1);
            assert!(err.is_deadline_exceeded());
            assert_eq!(err.to_string(), "context deadline exceeded");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_covers_a_non_streaming_body() {
        let stalled = reply(200, "{").stalled();
        let fake = Fake::new(vec![stalled.clone(), reply(200, OK)]);
        let got = send(
            &anthropic(fake.clone(), 1),
            false,
            Some(Duration::from_secs(2)),
        )
        .await
        .unwrap();
        assert_eq!(got, OK.as_bytes());
        assert_eq!(fake.calls(), 2);

        let fake = Fake::new(vec![stalled]);
        let err = send(&anthropic(fake, 1), false, Some(Duration::from_secs(2)))
            .await
            .unwrap_err();
        assert!(err.is_deadline_exceeded());
    }

    #[tokio::test(start_paused = true)]
    async fn send_attempt_timeout_keeps_running_on_a_streaming_body() {
        let fake = Fake::new(vec![Reply::Respond {
            status: 200,
            headers: vec![("content-type", "text/event-stream")],
            body: vec![Ok("data: a\n\n")],
            stall: true,
        }]);
        let t = anthropic(fake, 2);
        let start = Instant::now();
        let res = t
            .send(
                "claude-x",
                Bytes::from_static(BODY),
                true,
                Some(Duration::from_secs(5)),
            )
            .await
            .unwrap();
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        let err = body.next().await.unwrap().unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        let ModelError::Other(boxed) = err else {
            panic!("want ModelError::Other, got {err:?}");
        };
        let err = boxed.downcast::<AnthropicError>().unwrap();
        assert!(err.is_deadline_exceeded());
        assert!(
            body.next().await.is_none(),
            "the body ends after the deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_timeout_is_no_timeout() {
        let fake = Fake::new(vec![reply(200, "data: a\n\n").stalled()]);
        let res = anthropic(fake, 0)
            .send(
                "claude-x",
                Bytes::from_static(BODY),
                true,
                Some(Duration::ZERO),
            )
            .await
            .unwrap();
        let mut body = res.into_body();
        assert!(body.next().await.unwrap().is_ok());
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err()
        );
    }

    // ------------------------------------------------------------ send: dropped bodies

    #[tokio::test(start_paused = true)]
    async fn send_retries_a_dropped_body() {
        let fake = Fake::new(vec![reply(200, "{").dropped(), reply(200, OK)]);
        assert_eq!(
            send(&anthropic(fake.clone(), 2), false, None)
                .await
                .unwrap(),
            OK.as_bytes()
        );
        assert_eq!(fake.calls(), 2);
        assert_backoff(fake.gap(0), 0, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn send_dropped_body_honours_retry_after() {
        let fake = Fake::new(vec![
            reply(200, "{").dropped().header("retry-after-ms", "2000"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn send_dropped_body_not_retried() {
        let want = "anthropicmodel: request failed: error reading response body: unexpected EOF";
        // x-should-retry: false.
        let fake = Fake::new(vec![
            reply(200, "{").dropped().header("x-should-retry", "false"),
            reply(200, OK),
        ]);
        let err = send(&anthropic(fake.clone(), 2), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.calls(), 1);
        assert_eq!(err.to_string(), want);
        // The last attempt.
        let fake = Fake::new(vec![reply(200, "{").dropped()]);
        let err = send(&anthropic(fake.clone(), 1), false, None)
            .await
            .unwrap_err();
        assert_eq!(fake.calls(), 2);
        assert_eq!(err.to_string(), want);
        // An error status whose body drops on the last attempt.
        let fake = Fake::new(vec![reply(500, "{").dropped()]);
        let err = send(&anthropic(fake.clone(), 0), false, None)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), want);
    }

    // ------------------------------------------------------------ send: streaming

    #[tokio::test(start_paused = true)]
    async fn send_streaming_returns_the_live_body() {
        let fake = Fake::new(vec![
            reply(503, BOOM).dropped(),
            reply(200, "data: a\n\n").stalled(),
        ]);
        let res = anthropic(fake.clone(), 2)
            .send("claude-x", Bytes::from_static(BODY), true, None)
            .await
            .unwrap();
        // The retried 503's broken body was never read; the 2xx body is left to the caller.
        assert_eq!(fake.calls(), 2);
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
    }

    #[tokio::test(start_paused = true)]
    async fn send_streaming_error_status_reads_the_body() {
        let fake = Fake::new(vec![reply(400, BAD)]);
        let Err(err) = anthropic(fake, 2)
            .send("claude-x", Bytes::from_static(BODY), true, None)
            .await
        else {
            panic!("want an error");
        };
        assert!(err.is(ErrorKind::BadRequest));
        assert_eq!(err.api_error().unwrap().message, "bad thing");
    }

    // ------------------------------------------------------------ send: cancellation by drop

    #[tokio::test(start_paused = true)]
    async fn dropping_send_during_backoff_sends_nothing_more() {
        let fake = Fake::new(vec![
            reply(503, BOOM).header("retry-after", "60"),
            reply(200, OK),
        ]);
        let t = anthropic(fake.clone(), 2);
        let res = tokio::time::timeout(Duration::from_secs(10), send(&t, false, None)).await;
        assert!(res.is_err(), "still backing off");
        assert_eq!(fake.calls(), 1);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(fake.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_send_drops_the_in_flight_request() {
        let fake = Fake::new(vec![Reply::Hang]);
        let t = anthropic(fake.clone(), 2);
        let res = tokio::time::timeout(Duration::from_secs(1), send(&t, false, None)).await;
        assert!(res.is_err());
        assert!(fake.hang_dropped.load(Ordering::SeqCst));
        assert_eq!(fake.calls(), 1);
    }
}
