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

//! The attempt loop (port of anthropic-sdk-go v1.84.0 `RequestConfig.Execute`/`send` and
//! openai-go v3.91.1 `RequestConfig.Execute`): headers per attempt, the response-header
//! timeout, retries with backoff, which bodies are read, dropped bodies, and both timeout
//! models.

use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use http::{HeaderMap, Method, Uri};
use tokio::time::Instant;

use super::error::RestError;
use super::policy::{RetryAfter, RetryPolicy, should_retry, should_retry_dropped_body};
use crate::error::ModelError;
use crate::http::{BodyStream, HttpClient, collect_body};

/// Both Go SDKs' default client `ResponseHeaderTimeout` (`defaultResponseHeaderTimeout`,
/// 10 minutes).
pub const DEFAULT_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(600);

/// The timeout model of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallTimeout {
    /// No timeout (cancellation is by drop only).
    #[default]
    None,
    /// anthropic-sdk-go `WithRequestTimeout`: each attempt gets its own deadline, `d` after it
    /// starts, covering the header fetch, the send and the read of a body that ends the call;
    /// a timed-out attempt is retried like a connection error unless it is the last, which
    /// fails with [`RestError::DeadlineExceeded`]; a streaming body stays under the last
    /// attempt's deadline. `PerAttempt(Duration::ZERO)` is [`CallTimeout::None`].
    PerAttempt(Duration),
    /// Go `context.WithTimeout` around the whole call (openai-go): one deadline over every
    /// attempt, every backoff sleep and the streaming body. Reaching it ends the call at once
    /// with [`RestError::DeadlineExceeded`], never retried; a deadline already passed sends
    /// nothing. The caller computes it when the call (or stream) starts running.
    Call(Instant),
}

/// The headers of one attempt, fetched again for every attempt (credentials may refresh).
#[async_trait::async_trait]
pub trait RequestHeaders: Send + Sync {
    /// The complete header map of the next attempt. An error is treated like a connection
    /// error (retried; after the last attempt returned as [`RestError::Headers`] or whatever
    /// the implementation returns).
    async fn headers(&self) -> Result<HeaderMap, RestError>;
}

/// A fixed header map, sent as it is on every attempt.
#[async_trait::async_trait]
impl RequestHeaders for HeaderMap {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        Ok(self.clone())
    }
}

/// The response that ended a call.
pub enum RestReply {
    /// A response whose body was read whole: every status ≥ 400, and a 2xx (or other status
    /// < 400) when the call was not streaming. The caller maps an error status to its own
    /// error.
    Read(http::response::Parts, Bytes),
    /// A streaming call's response with status < 400, its body left to the caller. Under a
    /// deadline ([`CallTimeout::PerAttempt`]'s last attempt, or [`CallTimeout::Call`]) the
    /// body yields `Err(ModelError::Other(Box::new(RestError::DeadlineExceeded)))` once the
    /// deadline passes, then ends.
    Streaming(http::Response<BodyStream>),
}

impl std::fmt::Debug for RestReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(parts, body) => f.debug_tuple("Read").field(parts).field(body).finish(),
            Self::Streaming(res) => f
                .debug_struct("Streaming")
                .field("status", &res.status())
                .field("headers", res.headers())
                .finish_non_exhaustive(),
        }
    }
}

/// A provider's REST transport: the HTTP client, the retry policy and the response-header
/// timeout.
#[derive(Clone)]
pub struct RestTransport {
    /// The HTTP client.
    pub http: Arc<dyn HttpClient>,
    /// The retry policy.
    pub policy: RetryPolicy,
    /// A timeout on each `HttpClient::send` (until the response headers):
    /// [`DEFAULT_RESPONSE_HEADER_TIMEOUT`] when the provider built the client itself, `None`
    /// for a caller's client. It covers connecting and writing the request too (Go's
    /// `ResponseHeaderTimeout` starts once the request is written), never a body; the header
    /// fetch before `send` is outside it.
    pub response_header_timeout: Option<Duration>,
}

impl std::fmt::Debug for RestTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestTransport")
            .field("policy", &self.policy)
            .field("response_header_timeout", &self.response_header_timeout)
            .finish_non_exhaustive()
    }
}

/// What one attempt produced.
enum Attempt {
    /// No response: a header failure, a connection error, or the response-header timeout.
    NoResponse(RestError),
    /// The attempt's deadline passed.
    TimedOut,
    /// A response to retry after this wait; it was dropped unread.
    Retry(Duration),
    /// A response that ends the call, its body read.
    Read(http::response::Parts, Bytes),
    /// A response that ends the call whose body could not be read.
    DroppedBody(http::response::Parts, ModelError),
    /// A streaming call's response with status < 400 that ends the call, unread.
    Streaming(http::Response<BodyStream>),
}

/// A streaming body still under `deadline` (SDK `bodyWithTimeout`): once it passes, the next
/// poll yields `Err(ModelError::Other(RestError::DeadlineExceeded))` and the body ends (it is
/// dropped, which closes the connection). A chunk ready at the deadline wins.
fn body_with_deadline(body: BodyStream, deadline: Instant) -> BodyStream {
    let mut body = Some(body);
    let mut sleep = Box::pin(tokio::time::sleep_until(deadline));
    futures::stream::poll_fn(move |cx| {
        let Some(inner) = body.as_mut() else {
            return Poll::Ready(None);
        };
        if let Poll::Ready(item) = inner.poll_next_unpin(cx) {
            if item.is_none() {
                body = None;
            }
            return Poll::Ready(item);
        }
        if sleep.poll_unpin(cx).is_ready() {
            body = None;
            return Poll::Ready(Some(Err(ModelError::Other(Box::new(
                RestError::DeadlineExceeded,
            )))));
        }
        Poll::Pending
    })
    .boxed()
}

impl RestTransport {
    /// One attempt, up to the point Go's loop decides on it: fetch the headers, send (under
    /// `response_header_timeout`), then either decide to retry (unread) or read the body of a
    /// response that ends the call unless it is a streaming status < 400 (SDK `readsBody`).
    async fn attempt(
        &self,
        method: &Method,
        uri: &Uri,
        body: &Bytes,
        stream: bool,
        headers: &dyn RequestHeaders,
        retry_count: u32,
    ) -> Attempt {
        let headers = match headers.headers().await {
            Ok(h) => h,
            Err(e) => return Attempt::NoResponse(e),
        };
        let mut req = http::Request::new(body.clone());
        *req.method_mut() = method.clone();
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = headers;
        let sent = match self.response_header_timeout {
            Some(limit) => match tokio::time::timeout(limit, self.http.send(req)).await {
                Ok(sent) => sent,
                Err(_) => return Attempt::NoResponse(RestError::ResponseHeaderTimeout),
            },
            None => self.http.send(req).await,
        };
        let res = match sent {
            Ok(res) => res,
            Err(e) => return Attempt::NoResponse(RestError::Transport(e)),
        };
        if retry_count < self.policy.max_retries
            && should_retry(res.status(), res.headers())
            && let RetryAfter::Wait(delay) =
                self.policy.retry_delay(Some(res.headers()), retry_count)
        {
            // The response is dropped unread, which closes its connection.
            return Attempt::Retry(delay);
        }
        if stream && res.status().as_u16() < 400 {
            return Attempt::Streaming(res);
        }
        let (parts, body) = res.into_parts();
        match collect_body(body).await {
            Ok(bytes) => Attempt::Read(parts, bytes),
            Err(e) => Attempt::DroppedBody(parts, e),
        }
    }

    /// The wait before retry `retry_count` after an attempt without a response (Go's `res` is
    /// `nil`, so only the backoff applies).
    fn backoff(&self, retry_count: u32) -> Duration {
        match self.policy.retry_delay(None, retry_count) {
            RetryAfter::Wait(d) => d,
            RetryAfter::Stop => Duration::ZERO,
        }
    }

    /// `POST`s `body` to `uri` with the policy's retries and returns the response that ends
    /// the call: [`send_method`](Self::send_method) with [`Method::POST`].
    pub async fn send(
        &self,
        uri: &Uri,
        body: Bytes,
        stream: bool,
        headers: &dyn RequestHeaders,
        timeout: CallTimeout,
    ) -> Result<RestReply, RestError> {
        self.send_method(Method::POST, uri, body, stream, headers, timeout)
            .await
    }

    /// Sends `body` to `uri` as a `method` request (an empty body for a `GET` or `DELETE`)
    /// with the policy's retries and returns the response that ends the call.
    ///
    /// - Attempts: `policy.max_retries + 1`. Each attempt fetches `headers` first. A header
    ///   failure, a connection error and the response-header timeout are retried with backoff;
    ///   after the last attempt they are returned ([`RestError::Headers`] or the provider's
    ///   error, [`RestError::Transport`], [`RestError::ResponseHeaderTimeout`]).
    /// - A response is retried per [`should_retry`](super::should_retry) (a 2xx with
    ///   `x-should-retry: true` included) after
    ///   [`RetryPolicy::retry_delay`] with its headers, unread; a [`RetryAfter::Stop`] makes it
    ///   the final response instead (openai-go preserves it).
    /// - The final response's body is read, unless the call streams and the status is < 400:
    ///   [`RestReply::Read`], or [`RestReply::Streaming`] with the live body.
    /// - A body that fails to read is retried with that response's headers when
    ///   `policy.retry_dropped_body` and [`should_retry_dropped_body`](super::should_retry_dropped_body)
    ///   allow it and attempts remain; otherwise it is [`RestError::DroppedBody`].
    /// - `timeout`: see [`CallTimeout`].
    /// - Every await is cancelled by dropping the future (no task is spawned).
    pub async fn send_method(
        &self,
        method: Method,
        uri: &Uri,
        body: Bytes,
        stream: bool,
        headers: &dyn RequestHeaders,
        timeout: CallTimeout,
    ) -> Result<RestReply, RestError> {
        let (per_attempt, call_deadline) = match timeout {
            CallTimeout::None => (None, None),
            CallTimeout::PerAttempt(d) => ((!d.is_zero()).then_some(d), None),
            CallTimeout::Call(at) => (None, Some(at)),
        };
        let mut retry_count: u32 = 0;
        loop {
            if call_deadline.is_some_and(|at| Instant::now() >= at) {
                return Err(RestError::DeadlineExceeded);
            }
            let last_attempt = retry_count >= self.policy.max_retries;
            // A per-attempt deadline past `Instant`'s range is none (Go saturates).
            let deadline = per_attempt
                .and_then(|t| Instant::now().checked_add(t))
                .or(call_deadline);
            let attempt = self.attempt(&method, uri, &body, stream, headers, retry_count);
            let outcome = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, attempt)
                    .await
                    .unwrap_or(Attempt::TimedOut),
                None => attempt.await,
            };
            let delay = match outcome {
                Attempt::NoResponse(err) => {
                    if last_attempt {
                        return Err(err);
                    }
                    self.backoff(retry_count)
                }
                Attempt::TimedOut => {
                    if last_attempt || call_deadline.is_some() {
                        return Err(RestError::DeadlineExceeded);
                    }
                    self.backoff(retry_count)
                }
                Attempt::Retry(delay) => delay,
                Attempt::DroppedBody(parts, source) => {
                    let retry = !last_attempt
                        && self.policy.retry_dropped_body
                        && should_retry_dropped_body(&parts.headers);
                    match retry.then(|| self.policy.retry_delay(Some(&parts.headers), retry_count))
                    {
                        Some(RetryAfter::Wait(delay)) => delay,
                        _ => {
                            return Err(RestError::DroppedBody {
                                parts: Box::new(parts),
                                source,
                            });
                        }
                    }
                }
                Attempt::Read(parts, bytes) => return Ok(RestReply::Read(parts, bytes)),
                Attempt::Streaming(res) => {
                    return Ok(RestReply::Streaming(match deadline {
                        Some(deadline) => res.map(|b| body_with_deadline(b, deadline)),
                        None => res,
                    }));
                }
            };
            let sleep = tokio::time::sleep(delay);
            match call_deadline {
                Some(at) => {
                    if tokio::time::timeout_at(at, sleep).await.is_err() {
                        return Err(RestError::DeadlineExceeded);
                    }
                }
                None => sleep.await,
            }
            retry_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use http::{HeaderValue, StatusCode};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::rest::test_util::{FakeResponse, ScriptedClient};

    // ------------------------------------------------------------ helpers

    const URI: &str = "https://api.example.test/v1/endpoint";
    const BODY: &[u8] = br#"{"max_tokens":16,"model":"m"}"#;
    const OK: &str = r#"{"id":"msg_1"}"#;
    const BOOM: &str = r#"{"error":"boom"}"#;

    fn reply(status: u16, body: &'static str) -> FakeResponse {
        FakeResponse::json_status(status, body)
    }

    fn transport(http: Arc<ScriptedClient>, policy: RetryPolicy) -> RestTransport {
        RestTransport {
            http,
            policy,
            response_header_timeout: None,
        }
    }

    fn anthropic(http: Arc<ScriptedClient>, max_retries: u32) -> RestTransport {
        transport(
            http,
            RetryPolicy {
                max_retries,
                ..RetryPolicy::ANTHROPIC
            },
        )
    }

    fn openai(http: Arc<ScriptedClient>, max_retries: u32) -> RestTransport {
        transport(
            http,
            RetryPolicy {
                max_retries,
                ..RetryPolicy::OPENAI
            },
        )
    }

    fn fixed_headers() -> HeaderMap {
        HeaderMap::from_iter([
            (
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                http::HeaderName::from_static("x-api-key"),
                HeaderValue::from_static("sk-test"),
            ),
        ])
    }

    async fn send_with(
        t: &RestTransport,
        stream: bool,
        timeout: CallTimeout,
    ) -> Result<RestReply, RestError> {
        t.send(
            &Uri::from_static(URI),
            Bytes::from_static(BODY),
            stream,
            &fixed_headers(),
            timeout,
        )
        .await
    }

    /// Sends without a timeout and returns the final reply's status and body (a streaming
    /// reply's body is collected).
    async fn send(t: &RestTransport, stream: bool) -> Result<(u16, Bytes), RestError> {
        send_timeout(t, stream, CallTimeout::None).await
    }

    async fn send_timeout(
        t: &RestTransport,
        stream: bool,
        timeout: CallTimeout,
    ) -> Result<(u16, Bytes), RestError> {
        Ok(match send_with(t, stream, timeout).await? {
            RestReply::Read(parts, body) => (parts.status.as_u16(), body),
            RestReply::Streaming(res) => (
                res.status().as_u16(),
                collect_body(res.into_body()).await.unwrap(),
            ),
        })
    }

    fn streaming(reply: RestReply) -> http::Response<BodyStream> {
        match reply {
            RestReply::Streaming(res) => res,
            other => panic!("want a streaming reply, got {other:?}"),
        }
    }

    /// Asserts `d` is a jittered backoff for retry `n` plus `extra`.
    fn assert_backoff(d: Duration, n: u32, extra: Duration) {
        let base = Duration::from_millis(500) * 2u32.pow(n);
        let (lo, hi) = (extra + base - base / 4, extra + base);
        assert!(d > lo && d <= hi, "gap {d:?} not in ({lo:?}, {hi:?}]");
    }

    fn assert_deadline_body_error(err: ModelError) {
        let ModelError::Other(boxed) = err else {
            panic!("want ModelError::Other, got {err:?}");
        };
        let err = boxed.downcast::<RestError>().unwrap();
        assert!(matches!(*err, RestError::DeadlineExceeded), "{err:?}");
    }

    /// Headers that count their calls and fail the first `fail` of them.
    struct CountingHeaders {
        calls: AtomicUsize,
        fail: usize,
    }

    #[async_trait::async_trait]
    impl RequestHeaders for CountingHeaders {
        async fn headers(&self) -> Result<HeaderMap, RestError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail {
                return Err(RestError::Headers("credentials: boom".into()));
            }
            let mut h = fixed_headers();
            h.insert("x-attempt", HeaderValue::from_str(&n.to_string()).unwrap());
            Ok(h)
        }
    }

    // ------------------------------------------------------------ success and wire

    #[tokio::test(start_paused = true)]
    async fn send_first_attempt_success_wire() {
        let fake = ScriptedClient::always(reply(200, OK));
        let t = anthropic(fake.clone(), 2);
        let RestReply::Read(parts, body) = send_with(&t, false, CallTimeout::None).await.unwrap()
        else {
            panic!("want a read reply");
        };
        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(
            parts.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(body, OK.as_bytes());
        let seen = fake.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].url, URI);
        assert_eq!(seen[0].body, BODY);
        assert_eq!(seen[0].headers, fixed_headers());
    }

    #[tokio::test(start_paused = true)]
    async fn send_method_sends_get_and_delete_through_the_retry_loop() {
        for method in [Method::GET, Method::DELETE] {
            let fake = ScriptedClient::sequence(vec![reply(503, BOOM), reply(200, OK)]);
            let RestReply::Read(parts, body) = anthropic(fake.clone(), 2)
                .send_method(
                    method.clone(),
                    &Uri::from_static(URI),
                    Bytes::new(),
                    false,
                    &fixed_headers(),
                    CallTimeout::None,
                )
                .await
                .unwrap()
            else {
                panic!("want a read reply");
            };
            assert_eq!(parts.status, StatusCode::OK);
            assert_eq!(body, OK.as_bytes());
            let seen = fake.requests();
            assert_eq!(seen.len(), 2, "{method}");
            for s in &seen {
                assert_eq!(s.method, method.as_str());
                assert_eq!(s.url, URI);
                assert!(s.body.is_empty());
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_is_send_method_post() {
        let fake = ScriptedClient::always(reply(200, OK));
        send(&anthropic(fake.clone(), 0), false).await.unwrap();
        anthropic(fake.clone(), 0)
            .send_method(
                Method::POST,
                &Uri::from_static(URI),
                Bytes::from_static(BODY),
                false,
                &fixed_headers(),
                CallTimeout::None,
            )
            .await
            .unwrap();
        let seen = fake.requests();
        assert_eq!(seen.len(), 2);
        for s in &seen {
            assert_eq!(s.method, "POST");
            assert_eq!(s.body, BODY);
            assert_eq!(s.headers, fixed_headers());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_resends_the_same_body_and_headers() {
        let fake =
            ScriptedClient::sequence(vec![reply(503, BOOM), reply(503, BOOM), reply(200, OK)]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        let seen = fake.requests();
        assert_eq!(seen.len(), 3);
        for s in &seen {
            assert_eq!(s.body, BODY);
            assert_eq!(s.headers, seen[0].headers);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_fetches_headers_for_every_attempt() {
        let fake =
            ScriptedClient::sequence(vec![reply(500, BOOM), reply(500, BOOM), reply(200, OK)]);
        let headers = CountingHeaders {
            calls: AtomicUsize::new(0),
            fail: 0,
        };
        anthropic(fake.clone(), 2)
            .send(
                &Uri::from_static(URI),
                Bytes::from_static(BODY),
                false,
                &headers,
                CallTimeout::None,
            )
            .await
            .unwrap();
        assert_eq!(headers.calls.load(Ordering::SeqCst), 3);
        let attempts: Vec<String> = fake
            .requests()
            .iter()
            .map(|r| r.header("x-attempt"))
            .collect();
        assert_eq!(attempts, ["0", "1", "2"]);
    }

    // ------------------------------------------------------------ status rules

    #[tokio::test(start_paused = true)]
    async fn send_retries_retryable_statuses() {
        for policy in [RetryPolicy::ANTHROPIC, RetryPolicy::OPENAI] {
            for status in [408u16, 409, 429, 500, 502, 503, 529] {
                let fake = ScriptedClient::sequence(vec![reply(status, BOOM), reply(200, OK)]);
                let got = send(&transport(fake.clone(), policy), false).await.unwrap();
                assert_eq!(got, (200, Bytes::from_static(OK.as_bytes())), "{status}");
                assert_eq!(fake.hits(), 2, "{status}");
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_returns_other_statuses_read() {
        for status in [400u16, 401, 403, 404, 422] {
            let fake = ScriptedClient::sequence(vec![reply(status, BOOM), reply(200, OK)]);
            let got = send(&anthropic(fake.clone(), 2), false).await.unwrap();
            assert_eq!(got, (status, Bytes::from_static(BOOM.as_bytes())));
            assert_eq!(fake.hits(), 1, "{status}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_max_retries_0_1_2() {
        for max_retries in [0u32, 1, 2] {
            let fake = ScriptedClient::always(reply(500, BOOM));
            let got = send(&anthropic(fake.clone(), max_retries), false)
                .await
                .unwrap();
            assert_eq!(fake.hits(), max_retries as usize + 1, "{max_retries}");
            assert_eq!(got, (500, Bytes::from_static(BOOM.as_bytes())));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_x_should_retry_overrides_status() {
        // true on a 2xx: retried.
        let fake = ScriptedClient::sequence(vec![
            reply(200, "first").header("x-should-retry", "true"),
            reply(200, OK),
        ]);
        assert_eq!(
            send(&anthropic(fake.clone(), 2), false).await.unwrap().1,
            OK.as_bytes()
        );
        assert_eq!(fake.hits(), 2);

        // true on a 400: retried.
        let fake = ScriptedClient::sequence(vec![
            reply(400, BOOM).header("x-should-retry", "true"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.hits(), 2);

        // false on a 503: not retried.
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).header("x-should-retry", "false"),
            reply(200, OK),
        ]);
        assert_eq!(
            send(&anthropic(fake.clone(), 2), false).await.unwrap().0,
            503
        );
        assert_eq!(fake.hits(), 1);

        // true on the last attempt: the response is returned.
        let fake = ScriptedClient::always(reply(200, "last").header("x-should-retry", "true"));
        assert_eq!(
            send(&anthropic(fake.clone(), 0), false).await.unwrap().1,
            "last".as_bytes()
        );
        assert_eq!(fake.hits(), 1);
    }

    // ------------------------------------------------------------ delays

    #[tokio::test(start_paused = true)]
    async fn send_backoff_bounds_with_jitter() {
        let fake = ScriptedClient::sequence(vec![
            reply(500, BOOM),
            reply(500, BOOM),
            reply(500, BOOM),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 3), false).await.unwrap();
        assert_eq!(fake.hits(), 4);
        for n in 0..3 {
            assert_backoff(fake.gap(n as usize), n, Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_ms() {
        let fake = ScriptedClient::sequence(vec![
            reply(429, BOOM).header("retry-after-ms", "1234"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.gap(0), Duration::from_millis(1234));
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_seconds() {
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).header("retry-after", "7"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(7));
    }

    fn http_date(from_now: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(from_now))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string()
    }

    #[tokio::test(start_paused = true)]
    async fn send_honours_retry_after_date() {
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).header("retry-after", http_date(30)),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        let gap = fake.gap(0);
        assert!(
            gap > Duration::from_secs(28) && gap <= Duration::from_secs(30),
            "{gap:?}"
        );
    }

    // ------------------------------------------------------------ OpenAI retry-after rule

    #[tokio::test(start_paused = true)]
    async fn openai_retry_after_beyond_the_limit_returns_the_response() {
        let cases: [(&str, &str, String); 4] = [
            ("seconds_121", "retry-after", "121".to_string()),
            ("seconds_overflow", "retry-after", "1e400".to_string()),
            ("ms_overflow", "retry-after-ms", "1e400".to_string()),
            ("date_beyond_two_minutes", "retry-after", http_date(180)),
        ];
        for (name, header, value) in cases {
            let fake = ScriptedClient::sequence(vec![
                reply(429, BOOM).header(header, value),
                reply(200, OK),
            ]);
            let got = send(&openai(fake.clone(), 2), false).await.unwrap();
            assert_eq!(fake.hits(), 1, "{name}");
            assert_eq!(got, (429, Bytes::from_static(BOOM.as_bytes())), "{name}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn openai_stop_on_a_retried_2xx_returns_that_2xx() {
        let fake = ScriptedClient::sequence(vec![
            reply(200, "first")
                .header("x-should-retry", "true")
                .header("retry-after", "121"),
            reply(200, OK),
        ]);
        let got = send(&openai(fake.clone(), 2), false).await.unwrap();
        assert_eq!(got, (200, Bytes::from_static(b"first")));
        assert_eq!(fake.hits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn openai_skipped_retry_after_falls_back_to_backoff() {
        for value in ["inf", "+Inf", "NaN", "-5"] {
            let fake = ScriptedClient::sequence(vec![
                reply(429, BOOM).header("retry-after", value),
                reply(200, OK),
            ]);
            send(&openai(fake.clone(), 2), false).await.unwrap();
            assert_eq!(fake.hits(), 2, "{value}");
            assert_backoff(fake.gap(0), 0, Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn openai_retry_after_within_the_limit_is_honoured() {
        let fake = ScriptedClient::sequence(vec![
            reply(429, BOOM).header("retry-after", "120"),
            reply(429, BOOM).header("retry-after", http_date(-60)),
            reply(200, OK),
        ]);
        send(&openai(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(120));
        assert_eq!(fake.gap(1), Duration::ZERO, "a past date waits zero");
    }

    #[tokio::test(start_paused = true)]
    async fn anthropic_honours_any_retry_after() {
        let fake = ScriptedClient::sequence(vec![
            reply(429, BOOM).header("retry-after", "121"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(121));
    }

    // ------------------------------------------------------------ no response

    #[tokio::test(start_paused = true)]
    async fn send_retries_connection_errors() {
        let fake = ScriptedClient::sequence(vec![
            FakeResponse::reset("connection refused"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.hits(), 2);
        assert_backoff(fake.gap(0), 0, Duration::ZERO);

        let fake = ScriptedClient::always(FakeResponse::reset("connection refused"));
        let err = send(&anthropic(fake.clone(), 2), false).await.unwrap_err();
        assert_eq!(fake.hits(), 3);
        let RestError::Transport(ModelError::Transport(cause)) = err else {
            panic!("want a transport error, got {err:?}");
        };
        assert_eq!(cause.to_string(), "connection refused");
    }

    #[tokio::test(start_paused = true)]
    async fn send_retries_header_failures() {
        let fake = ScriptedClient::always(reply(200, OK));
        let headers = CountingHeaders {
            calls: AtomicUsize::new(0),
            fail: 1,
        };
        let t = anthropic(fake.clone(), 2);
        let uri = Uri::from_static(URI);
        let reply = t
            .send(
                &uri,
                Bytes::from_static(BODY),
                false,
                &headers,
                CallTimeout::None,
            )
            .await
            .unwrap();
        assert!(matches!(reply, RestReply::Read(..)));
        assert_eq!(fake.hits(), 1);

        let headers = CountingHeaders {
            calls: AtomicUsize::new(0),
            fail: usize::MAX,
        };
        let err = t
            .send(
                &uri,
                Bytes::from_static(BODY),
                false,
                &headers,
                CallTimeout::None,
            )
            .await
            .unwrap_err();
        assert_eq!(headers.calls.load(Ordering::SeqCst), 3);
        assert_eq!(fake.hits(), 1, "nothing more was sent");
        let RestError::Headers(cause) = err else {
            panic!("want a header error, got {err:?}");
        };
        assert_eq!(cause.to_string(), "credentials: boom");
    }

    #[tokio::test(start_paused = true)]
    async fn send_response_header_timeout() {
        let fake = ScriptedClient::sequence(vec![FakeResponse::never(), reply(200, OK)]);
        let mut t = anthropic(fake.clone(), 2);
        t.response_header_timeout = Some(DEFAULT_RESPONSE_HEADER_TIMEOUT);
        send(&t, false).await.unwrap();
        assert_backoff(fake.gap(0), 0, DEFAULT_RESPONSE_HEADER_TIMEOUT);
        assert_eq!(fake.drops(), 1, "the timed-out send was dropped");

        let fake = ScriptedClient::always(FakeResponse::never());
        let mut t = anthropic(fake.clone(), 0);
        t.response_header_timeout = Some(DEFAULT_RESPONSE_HEADER_TIMEOUT);
        let err = send(&t, false).await.unwrap_err();
        assert!(matches!(err, RestError::ResponseHeaderTimeout), "{err:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn response_header_timeout_does_not_limit_a_streaming_body() {
        let fake = ScriptedClient::always(FakeResponse::sse("data: a\n\n").hang());
        let mut t = anthropic(fake, 0);
        t.response_header_timeout = Some(Duration::from_secs(1));
        let res = streaming(send_with(&t, true, CallTimeout::None).await.unwrap());
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err(),
            "the body stays open"
        );
    }

    // ------------------------------------------------------------ CallTimeout::PerAttempt

    #[tokio::test(start_paused = true)]
    async fn per_attempt_timeout_is_retried() {
        let fake = ScriptedClient::sequence(vec![
            FakeResponse::never(),
            FakeResponse::never(),
            reply(200, OK),
        ]);
        let got = send_timeout(
            &anthropic(fake.clone(), 2),
            false,
            CallTimeout::PerAttempt(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        assert_eq!(got.1, OK.as_bytes());
        assert_eq!(fake.hits(), 3);
        assert_backoff(fake.gap(0), 0, Duration::from_secs(1));
        assert_backoff(fake.gap(1), 1, Duration::from_secs(1));
        assert_eq!(fake.drops(), 2, "timed-out sends dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn per_attempt_timeout_on_last_attempt_is_deadline_exceeded() {
        for max_retries in [0u32, 2] {
            let fake = ScriptedClient::always(FakeResponse::never());
            let err = send_timeout(
                &anthropic(fake.clone(), max_retries),
                false,
                CallTimeout::PerAttempt(Duration::from_secs(1)),
            )
            .await
            .unwrap_err();
            assert_eq!(fake.hits(), max_retries as usize + 1);
            assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn per_attempt_timeout_covers_a_non_streaming_body() {
        let stalled = reply(200, "{").hang();
        let fake = ScriptedClient::sequence(vec![stalled.clone(), reply(200, OK)]);
        let got = send_timeout(
            &anthropic(fake.clone(), 1),
            false,
            CallTimeout::PerAttempt(Duration::from_secs(2)),
        )
        .await
        .unwrap();
        assert_eq!(got.1, OK.as_bytes());
        assert_eq!(fake.hits(), 2);

        let fake = ScriptedClient::always(stalled);
        let err = send_timeout(
            &anthropic(fake, 1),
            false,
            CallTimeout::PerAttempt(Duration::from_secs(2)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn per_attempt_timeout_keeps_running_on_a_streaming_body() {
        let fake = ScriptedClient::always(FakeResponse::sse("data: a\n\n").hang());
        let start = Instant::now();
        let res = streaming(
            send_with(
                &anthropic(fake, 2),
                true,
                CallTimeout::PerAttempt(Duration::from_secs(5)),
            )
            .await
            .unwrap(),
        );
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        let err = body.next().await.unwrap().unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert_deadline_body_error(err);
        assert!(
            body.next().await.is_none(),
            "the body ends after the deadline"
        );
    }

    /// A per-attempt timeout past `Instant`'s range is no deadline (Go saturates), never a
    /// panic.
    #[tokio::test(start_paused = true)]
    async fn duration_max_per_attempt_timeout_is_no_deadline() {
        let fake = ScriptedClient::always(reply(200, OK));
        let got = send_timeout(
            &anthropic(fake.clone(), 2),
            false,
            CallTimeout::PerAttempt(Duration::MAX),
        )
        .await
        .unwrap();
        assert_eq!(got.1, OK.as_bytes());
        assert_eq!(fake.hits(), 1);

        let fake = ScriptedClient::always(FakeResponse::sse("data: a\n\n").hang());
        let res = streaming(
            send_with(
                &anthropic(fake, 0),
                true,
                CallTimeout::PerAttempt(Duration::MAX),
            )
            .await
            .unwrap(),
        );
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err(),
            "no deadline ends the body"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_per_attempt_timeout_is_no_timeout() {
        let fake = ScriptedClient::always(FakeResponse::sse("data: a\n\n").hang());
        let res = streaming(
            send_with(
                &anthropic(fake, 0),
                true,
                CallTimeout::PerAttempt(Duration::ZERO),
            )
            .await
            .unwrap(),
        );
        let mut body = res.into_body();
        assert!(body.next().await.unwrap().is_ok());
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), body.next())
                .await
                .is_err()
        );
    }

    // ------------------------------------------------------------ CallTimeout::Call

    #[tokio::test(start_paused = true)]
    async fn call_deadline_on_the_first_attempt_is_not_retried() {
        let fake = ScriptedClient::sequence(vec![FakeResponse::never(), reply(200, OK)]);
        let start = Instant::now();
        let err = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(start + Duration::from_secs(1)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(fake.hits(), 1);
        assert_eq!(fake.drops(), 1, "the in-flight send was dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_cuts_a_backoff_sleep_short() {
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).header("retry-after", "60"),
            reply(200, OK),
        ]);
        let start = Instant::now();
        let err = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(start + Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert_eq!(fake.hits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_spans_every_attempt() {
        // Each attempt takes 3 s to answer: a per-attempt 5 s timeout would never fire, the
        // call's 5 s deadline ends the second attempt.
        let fake = ScriptedClient::always(
            reply(500, BOOM)
                .header("retry-after-ms", "0")
                .delay_headers(Duration::from_secs(3)),
        );
        let start = Instant::now();
        let err = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(start + Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert_eq!(fake.hits(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_covers_a_non_streaming_body() {
        let fake = ScriptedClient::sequence(vec![reply(200, "{").hang(), reply(200, OK)]);
        let start = Instant::now();
        let err = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(start + Duration::from_secs(2)),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        assert_eq!(fake.hits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_covers_a_streaming_body() {
        let fake = ScriptedClient::always(
            FakeResponse::sse("data: a\n\n")
                .delay_headers(Duration::from_secs(1))
                .hang(),
        );
        let start = Instant::now();
        let res = streaming(
            send_with(
                &openai(fake, 2),
                true,
                CallTimeout::Call(start + Duration::from_secs(5)),
            )
            .await
            .unwrap(),
        );
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
        let err = body.next().await.unwrap().unwrap_err();
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(5),
            "the call's deadline"
        );
        assert_deadline_body_error(err);
        assert!(body.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_already_passed_sends_nothing() {
        let fake = ScriptedClient::always(reply(200, OK));
        let err = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(Instant::now()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RestError::DeadlineExceeded), "{err:?}");
        assert_eq!(fake.hits(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn call_deadline_not_reached_returns_the_reply() {
        let fake = ScriptedClient::sequence(vec![reply(500, BOOM), reply(200, OK)]);
        let got = send_timeout(
            &openai(fake.clone(), 2),
            false,
            CallTimeout::Call(Instant::now() + Duration::from_secs(60)),
        )
        .await
        .unwrap();
        assert_eq!(got, (200, Bytes::from_static(OK.as_bytes())));
    }

    // ------------------------------------------------------------ dropped bodies

    #[tokio::test(start_paused = true)]
    async fn anthropic_retries_a_dropped_body() {
        let fake =
            ScriptedClient::sequence(vec![reply(200, "{").fail("unexpected EOF"), reply(200, OK)]);
        assert_eq!(
            send(&anthropic(fake.clone(), 2), false).await.unwrap().1,
            OK.as_bytes()
        );
        assert_eq!(fake.hits(), 2);
        assert_backoff(fake.gap(0), 0, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn anthropic_dropped_body_honours_retry_after() {
        let fake = ScriptedClient::sequence(vec![
            reply(200, "{")
                .fail("unexpected EOF")
                .header("retry-after-ms", "2000"),
            reply(200, OK),
        ]);
        send(&anthropic(fake.clone(), 2), false).await.unwrap();
        assert_eq!(fake.gap(0), Duration::from_secs(2));
    }

    fn assert_dropped(err: RestError, status: u16) {
        let RestError::DroppedBody { parts, source } = err else {
            panic!("want a dropped body, got {err:?}");
        };
        assert_eq!(parts.status.as_u16(), status);
        let ModelError::Transport(cause) = source else {
            panic!("want a transport cause, got {source:?}");
        };
        assert_eq!(cause.to_string(), "unexpected EOF");
    }

    #[tokio::test(start_paused = true)]
    async fn anthropic_dropped_body_not_retried() {
        // x-should-retry: false.
        let fake = ScriptedClient::sequence(vec![
            reply(200, "{")
                .fail("unexpected EOF")
                .header("x-should-retry", "false"),
            reply(200, OK),
        ]);
        let err = send(&anthropic(fake.clone(), 2), false).await.unwrap_err();
        assert_eq!(fake.hits(), 1);
        assert_dropped(err, 200);
        // The last attempt.
        let fake = ScriptedClient::always(reply(200, "{").fail("unexpected EOF"));
        let err = send(&anthropic(fake.clone(), 1), false).await.unwrap_err();
        assert_eq!(fake.hits(), 2);
        assert_dropped(err, 200);
        // An error status whose body drops on the last attempt.
        let fake = ScriptedClient::always(reply(500, "{").fail("unexpected EOF"));
        let err = send(&anthropic(fake.clone(), 0), false).await.unwrap_err();
        assert_dropped(err, 500);
    }

    #[tokio::test(start_paused = true)]
    async fn openai_never_retries_a_dropped_body() {
        // A non-streaming 2xx whose body drops: returned at once.
        let fake =
            ScriptedClient::sequence(vec![reply(200, "{").fail("unexpected EOF"), reply(200, OK)]);
        let err = send(&openai(fake.clone(), 2), false).await.unwrap_err();
        assert_eq!(fake.hits(), 1);
        assert_dropped(err, 200);
        // A retried 500 is dropped unread; the final 500's body drops: returned with its status.
        let fake = ScriptedClient::always(reply(500, "{").fail("unexpected EOF"));
        let err = send(&openai(fake.clone(), 2), false).await.unwrap_err();
        assert_eq!(fake.hits(), 3);
        assert_dropped(err, 500);
    }

    // ------------------------------------------------------------ streaming

    #[tokio::test(start_paused = true)]
    async fn send_streaming_returns_the_live_body() {
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).fail("unexpected EOF"),
            FakeResponse::sse("data: a\n\n").hang(),
        ]);
        let res = streaming(
            send_with(&anthropic(fake.clone(), 2), true, CallTimeout::None)
                .await
                .unwrap(),
        );
        // The retried 503's broken body was never read; the 2xx body is left to the caller.
        assert_eq!(fake.hits(), 2);
        let mut body = res.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), "data: a\n\n");
    }

    #[tokio::test(start_paused = true)]
    async fn send_streaming_error_status_reads_the_body() {
        let fake = ScriptedClient::always(reply(400, BOOM));
        let RestReply::Read(parts, body) = send_with(&anthropic(fake, 2), true, CallTimeout::None)
            .await
            .unwrap()
        else {
            panic!("want a read reply");
        };
        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(body, BOOM.as_bytes());
    }

    // ------------------------------------------------------------ cancellation by drop

    #[tokio::test(start_paused = true)]
    async fn dropping_send_during_backoff_sends_nothing_more() {
        let fake = ScriptedClient::sequence(vec![
            reply(503, BOOM).header("retry-after", "60"),
            reply(200, OK),
        ]);
        let t = anthropic(fake.clone(), 2);
        let res = tokio::time::timeout(Duration::from_secs(10), send(&t, false)).await;
        assert!(res.is_err(), "still backing off");
        assert_eq!(fake.hits(), 1);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(fake.hits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_send_drops_the_in_flight_request() {
        let fake = ScriptedClient::always(FakeResponse::never());
        let t = anthropic(fake.clone(), 2);
        let res = tokio::time::timeout(Duration::from_secs(1), send(&t, false)).await;
        assert!(res.is_err());
        assert_eq!(fake.drops(), 1);
        assert_eq!(fake.hits(), 1);
    }

    #[test]
    fn call_timeout_default_is_none() {
        assert_eq!(CallTimeout::default(), CallTimeout::None);
    }
}
