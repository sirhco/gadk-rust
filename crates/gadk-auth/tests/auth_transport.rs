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

//! Port of Go `auth/transport_test.go` (package `auth_test`).
//!
//! Go's stub `RoundTripper`s become `Base`, a terminal middleware registered after `Transport`
//! that never calls `next.run` (plan C14). Request and response bodies that Go tracks with
//! `closeTrackingBody` are `TrackedBody` streams (and, for a replayable body, a `LiveProbe` in the
//! request's extensions), whose drop is Rust's `Close`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::Stream;
use gadk_auth::{
    ApiKey, AuthContext, Bearer, ConsentRequiredError, Credential, CredentialFuture,
    CredentialProvider, Error, ProviderFn, RefreshingProvider, client, find_consent_in,
};
use http::{Extensions, HeaderMap};
use pretty_assertions::assert_eq;
use reqwest::{Body, Request, Response};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next};

const URL: &str = "https://example.test/";

// ---------------------------------------------------------------------------------------------
// Body tracking (Go `closeTrackingBody`).
// ---------------------------------------------------------------------------------------------

/// What a `TrackedBody` observed.
#[derive(Debug, Default)]
struct BodyProbe {
    /// The stream was polled past its last chunk (Go: `read == len(payload)`).
    finished: AtomicBool,
    /// The stream was dropped (Go: `closed`).
    dropped: AtomicBool,
}

impl BodyProbe {
    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }

    /// Go `closeTrackingBody.drained()`: read to the end and closed.
    fn drained(&self) -> bool {
        self.finished() && self.dropped()
    }
}

/// A body stream that records being read to the end and being dropped.
struct TrackedBody {
    chunks: VecDeque<Vec<u8>>,
    probe: Arc<BodyProbe>,
}

impl TrackedBody {
    fn new(payload: &str, probe: Arc<BodyProbe>) -> Self {
        let mut chunks = VecDeque::new();
        if !payload.is_empty() {
            chunks.push_back(payload.as_bytes().to_vec());
        }
        Self { chunks, probe }
    }
}

impl Stream for TrackedBody {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.chunks.pop_front() {
            Some(chunk) => Poll::Ready(Some(Ok(chunk))),
            None => {
                self.probe.finished.store(true, Ordering::SeqCst);
                Poll::Ready(None)
            }
        }
    }
}

impl Drop for TrackedBody {
    fn drop(&mut self) {
        self.probe.dropped.store(true, Ordering::SeqCst);
    }
}

/// A streamed (so not replayable) request body carrying `payload`.
fn streamed(payload: &str, probe: &Arc<BodyProbe>) -> Body {
    Body::wrap_stream(TrackedBody::new(payload, probe.clone()))
}

/// Counts the live copies of a request: one per request holding it in its extensions.
/// `Request::try_clone` clones the extensions, so the replay snapshot holds one too.
struct LiveProbe(Arc<AtomicUsize>);

impl LiveProbe {
    fn new(live: &Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self(live.clone())
    }
}

impl Clone for LiveProbe {
    fn clone(&self) -> Self {
        Self::new(&self.0)
    }
}

impl Drop for LiveProbe {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A replayable POST of `payload` whose live copies `live` counts.
fn watched_post(payload: &'static str, live: &Arc<AtomicUsize>) -> Request {
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri(URL)
        .extension(LiveProbe::new(live))
        .body(Body::from(payload))
        .unwrap();
    Request::try_from(req).unwrap()
}

/// A POST with a streamed body (Go: a body with no `GetBody`).
fn streamed_post(payload: &str, probe: &Arc<BodyProbe>) -> Request {
    let mut req = Request::new(http::Method::POST, URL.parse().unwrap());
    *req.body_mut() = Some(streamed(payload, probe));
    req
}

// ---------------------------------------------------------------------------------------------
// The base (Go `captureRT` / `sequenceRT`).
// ---------------------------------------------------------------------------------------------

/// One request as the base received it.
#[derive(Debug, Clone)]
struct Seen {
    headers: HeaderMap,
    body: String,
    /// Live copies of the request while the base ran, when a counter is attached.
    live: Option<usize>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// Go `sequenceRT` (and `captureRT`, which is a `sequenceRT` that always answers 200): answers
/// `statuses[i]` for call `i`, 200 once exhausted; never calls `next.run`.
struct Base {
    statuses: Vec<u16>,
    live: Option<Arc<AtomicUsize>>,
    seen: Mutex<Vec<Seen>>,
    resp_bodies: Mutex<Vec<Arc<BodyProbe>>>,
}

impl Base {
    fn new(statuses: &[u16]) -> Arc<Self> {
        Arc::new(Self {
            statuses: statuses.to_vec(),
            live: None,
            seen: Mutex::new(Vec::new()),
            resp_bodies: Mutex::new(Vec::new()),
        })
    }

    /// A base that also records how many copies of the request were alive when it ran.
    fn watching(statuses: &[u16], live: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            statuses: statuses.to_vec(),
            live: Some(live.clone()),
            seen: Mutex::new(Vec::new()),
            resp_bodies: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn called(&self) -> bool {
        self.calls() > 0
    }

    /// The `Authorization` header of each call, `""` when absent.
    fn auths(&self) -> Vec<String> {
        self.seen()
            .iter()
            .map(|s| s.header("authorization").unwrap_or_default().to_string())
            .collect()
    }

    fn bodies(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.body).collect()
    }

    /// The response bodies handed out, in order.
    fn resp_bodies(&self) -> Vec<Arc<BodyProbe>> {
        self.resp_bodies.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Middleware for Base {
    async fn handle(
        &self,
        req: Request,
        _extensions: &mut Extensions,
        _next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let live = self.live.as_ref().map(|l| l.load(Ordering::SeqCst));
        let body = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let status = {
            let mut seen = self.seen.lock().unwrap();
            let status = self.statuses.get(seen.len()).copied().unwrap_or(200);
            seen.push(Seen {
                headers: req.headers().clone(),
                body,
                live,
            });
            status
        };
        let probe = Arc::new(BodyProbe::default());
        self.resp_bodies.lock().unwrap().push(probe.clone());
        let resp = http::Response::builder()
            .status(status)
            .body(Body::wrap_stream(TrackedBody::new("rejected", probe)))
            .unwrap();
        Ok(Response::from(resp))
    }
}

/// `gadk_auth::client` over `provider`, with `base` as the terminal middleware.
fn client_over(provider: Arc<dyn CredentialProvider>, base: &Arc<Base>) -> ClientWithMiddleware {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    ClientBuilder::from_client(client(http, provider, AuthContext::anonymous()))
        .with_arc(base.clone() as Arc<dyn Middleware>)
        .build()
}

// ---------------------------------------------------------------------------------------------
// Providers (Go `ProviderFunc`, `StaticToken`, `refreshProvider`, `errRefreshProvider`,
// `consentRefreshProvider`).
// ---------------------------------------------------------------------------------------------

fn bearer(token: &str) -> Arc<dyn Credential> {
    Arc::new(Bearer {
        token: token.into(),
    })
}

/// Go `auth.ProviderFunc` over a synchronous result.
fn provider_fn(
    f: impl Fn() -> Result<Arc<dyn Credential>, Error> + Send + Sync + 'static,
) -> Arc<dyn CredentialProvider> {
    ProviderFn::new(move |_cx: &AuthContext| -> CredentialFuture {
        let result = f();
        Box::pin(async move { result })
    })
    .into_provider()
}

/// Go `auth.StaticToken(token)`: a fresh `Bearer` per call. A local stand-in, because
/// `gadk_auth::static_token` is filled in by a task running in the same batch.
fn static_bearer(token: &str) -> Arc<dyn CredentialProvider> {
    let token = token.to_string();
    provider_fn(move || Ok(bearer(&token)))
}

/// Go `boom := errors.New("boom")`, as a type a chain walk can recognise (Go's `errors.Is`).
#[derive(Debug)]
struct Boom;

impl fmt::Display for Boom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("boom")
    }
}

impl std::error::Error for Boom {}

/// Go `errors.Is(err, target)` for a target type: walks the anyhow chain of a middleware error.
fn chain_has<E: std::error::Error + 'static>(err: &reqwest_middleware::Error) -> bool {
    match err {
        reqwest_middleware::Error::Middleware(e) => e.chain().any(|cause| cause.is::<E>()),
        reqwest_middleware::Error::Reqwest(_) => false,
    }
}

/// Go `refreshProvider`: yields `cred` first and `fresh` after (from `credential` and from
/// `refresh`), and records what it was told was rejected.
struct RefreshProvider {
    cred: Arc<dyn Credential>,
    fresh: Arc<dyn Credential>,
    resolves: AtomicUsize,
    refreshes: AtomicUsize,
    rejected: Mutex<Option<Arc<dyn Credential>>>,
}

impl RefreshProvider {
    fn new(cred: Arc<dyn Credential>, fresh: Arc<dyn Credential>) -> Arc<Self> {
        Arc::new(Self {
            cred,
            fresh,
            resolves: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            rejected: Mutex::new(None),
        })
    }

    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    fn rejected(&self) -> Option<Arc<dyn Credential>> {
        self.rejected.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl CredentialProvider for RefreshProvider {
    // Yields a different credential on every call after the first, so a Transport that
    // re-derived the rejected credential instead of passing the one it applied would hand
    // `refresh` a value no request ever carried.
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
        if self.resolves.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(self.cred.clone())
        } else {
            Ok(self.fresh.clone())
        }
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for RefreshProvider {
    async fn refresh(
        &self,
        _cx: &AuthContext,
        rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, Error> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        *self.rejected.lock().unwrap() = Some(rejected.clone());
        Ok(self.fresh.clone())
    }
}

/// Go `errRefreshProvider`: `refresh` always fails.
struct ErrRefreshProvider {
    cred: Arc<dyn Credential>,
}

#[async_trait::async_trait]
impl CredentialProvider for ErrRefreshProvider {
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
        Ok(self.cred.clone())
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for ErrRefreshProvider {
    async fn refresh(
        &self,
        _cx: &AuthContext,
        _rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, Error> {
        Err(Error::msg("refresh boom"))
    }
}

/// Go `fmt.Errorf("gcp: %w", consent)`.
#[derive(Debug)]
struct GcpWrap(ConsentRequiredError);

impl fmt::Display for GcpWrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gcp: {}", self.0)
    }
}

impl std::error::Error for GcpWrap {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Go `consentRefreshProvider`: refuses to refresh because the end user must consent again.
struct ConsentRefreshProvider {
    cred: Arc<dyn Credential>,
}

#[async_trait::async_trait]
impl CredentialProvider for ConsentRefreshProvider {
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
        Ok(self.cred.clone())
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for ConsentRefreshProvider {
    async fn refresh(
        &self,
        _cx: &AuthContext,
        _rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, Error> {
        Err(Error::other(GcpWrap(ConsentRequiredError {
            auth_uri: "https://consent.example".into(),
            nonce: "n".into(),
            ..ConsentRequiredError::default()
        })))
    }
}

// ---------------------------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------------------------

// parity: adapted: the middleware owns the request, so Go's "the caller's request was not mutated" check has no Rust counterpart; the applied header is still asserted.
#[tokio::test]
async fn test_transport_applies_credential() {
    let base = Base::new(&[]);
    let http = client_over(static_bearer("abc"), &base);

    http.get(URL).send().await.unwrap();

    assert!(base.called(), "base transport was not called");
    assert_eq!(base.auths(), vec!["Bearer abc".to_string()]);
}

#[tokio::test]
async fn test_transport_provider_error() {
    let base = Base::new(&[]);
    let http = client_over(provider_fn(|| Err(Error::other(Boom))), &base);

    let err = http.get(URL).send().await.unwrap_err();

    assert!(
        chain_has::<Boom>(&err),
        "RoundTrip() error = {err}, want boom"
    );
    assert_eq!(err.to_string(), "auth: resolve credential: boom");
    assert!(
        !base.called(),
        "base transport must not be called when the provider errors"
    );
}

#[tokio::test]
async fn test_transport_consent_required_propagates() {
    let base = Base::new(&[]);
    let http = client_over(
        provider_fn(|| {
            Err(ConsentRequiredError {
                auth_uri: "https://consent.example".into(),
                key: "k".into(),
                ..ConsentRequiredError::default()
            }
            .into())
        }),
        &base,
    );

    let err = http.get(URL).send().await.unwrap_err();

    let consent = find_consent_in(&err)
        .unwrap_or_else(|| panic!("RoundTrip() error = {err}, want a ConsentRequiredError"));
    assert_eq!(consent.auth_uri, "https://consent.example");
    assert_eq!(consent.key, "k");
    assert!(
        !base.called(),
        "base transport must not be called when consent is required"
    );
    // The wrap builds a new message, so keeping the URI out of the consent error's text is not
    // enough on its own. Asserted exactly rather than by absence of the URI.
    assert_eq!(
        err.to_string(),
        "auth: resolve credential: auth: interactive consent required"
    );
}

// parity: skipped TestTransportNilProvider: Transport holds a non-optional Arc<dyn CredentialProvider> (spec R7), so a Transport without a provider cannot be built.

// parity: adapted: request bodies close by being dropped; a streamed body's source observes the drop, and the base is not called.
#[tokio::test]
async fn test_transport_closes_body_on_error() {
    let probe = Arc::new(BodyProbe::default());
    let base = Base::new(&[]);
    let http = client_over(provider_fn(|| Err(Error::msg("boom"))), &base);

    let result = http.execute(streamed_post("", &probe)).await;

    assert!(result.is_err(), "RoundTrip() = nil error, want error");
    assert!(
        probe.dropped(),
        "RoundTrip must close req.Body when it returns an error"
    );
    assert!(
        !base.called(),
        "the request was sent although the provider failed"
    );
}

// parity: adapted: drop-based close, for the first attempt (a streamed body's source sees the drop) and for the retry snapshot (no live copy of the request is left).
#[tokio::test]
async fn test_transport_closes_body_when_apply_fails() {
    // first attempt
    {
        let probe = Arc::new(BodyProbe::default());
        let base = Base::new(&[]);
        // No token, so apply fails.
        let http = client_over(
            provider_fn(|| Ok(Arc::new(Bearer::default()) as Arc<dyn Credential>)),
            &base,
        );

        let err = http.execute(streamed_post("", &probe)).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            "auth: apply credential: auth: bearer credential missing token"
        );
        assert!(!base.called(), "the request was sent although Apply failed");
        assert!(probe.dropped(), "req.Body not closed after Apply failed");
    }
    // retry
    {
        let live = Arc::new(AtomicUsize::new(0));
        let base = Base::watching(&[401], &live);
        // The fresh credential has no token, so the retry's apply fails.
        let p = RefreshProvider::new(bearer("stale"), Arc::new(Bearer::default()));
        let http = client_over(p.clone(), &base);

        let err = http.execute(watched_post("x", &live)).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            "auth: apply credential: auth: bearer credential missing token"
        );
        assert_eq!(
            base.calls(),
            1,
            "base calls = {}, want 1 (the retry must not be sent)",
            base.calls()
        );
        assert_eq!(
            base.seen()[0].live,
            Some(2),
            "the first send should run alongside exactly one replay snapshot"
        );
        assert_eq!(
            live.load(Ordering::SeqCst),
            0,
            "the replayed body was not closed after Apply failed"
        );
    }
}

// parity: skipped TestTransportPassesThroughANilResponse: a reqwest_middleware::Next returns Result<Response>, so a base returning neither a response nor an error cannot be built.

#[tokio::test]
async fn test_transport_refreshes_and_retries_on401() {
    let base = Base::new(&[401, 200]);
    let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
    let http = client_over(p.clone(), &base);

    let resp = http.get(URL).send().await.unwrap();

    assert_eq!(resp.status(), 200, "want 200 after refresh+retry");
    assert_eq!(base.calls(), 2, "base calls");
    assert_eq!(p.refreshes(), 1, "refreshes");
    assert_eq!(
        base.auths(),
        vec!["Bearer stale".to_string(), "Bearer fresh".to_string()]
    );
}

#[tokio::test]
async fn test_transport_retry_replays_body() {
    let base = Base::new(&[403, 200]);
    let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
    let http = client_over(p.clone(), &base);

    http.post(URL).body("payload").send().await.unwrap();

    assert_eq!(base.calls(), 2, "base calls");
    for (i, b) in base.bodies().iter().enumerate() {
        assert_eq!(b, "payload", "call {i} body (body must be replayed)");
    }
}

#[tokio::test]
async fn test_transport_no_refresh_without_refreshing_provider() {
    let base = Base::new(&[401]);
    let http = client_over(static_bearer("x"), &base);

    let resp = http.get(URL).send().await.unwrap();

    assert_eq!(
        resp.status(),
        401,
        "want 401 (a non-refreshing provider is not retried)"
    );
    assert_eq!(base.calls(), 1, "base calls");
}

#[tokio::test]
async fn test_transport_no_retry_non_replayable_body() {
    let base = Base::new(&[401, 200]);
    let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
    let http = client_over(p.clone(), &base);
    let probe = Arc::new(BodyProbe::default());

    // A streamed body cannot be replayed (Go: GetBody = nil).
    let resp = http
        .execute(streamed_post("payload", &probe))
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        401,
        "want 401 (non-replayable body is not retried)"
    );
    assert_eq!(
        (base.calls(), p.refreshes()),
        (1, 0),
        "base calls and refreshes; want 1 and 0"
    );
}

// parity: adapted: Go's three GetBody failure shapes collapse to Request::try_clone() == None (a streamed body), and the "refresh returns nil" provider cannot be built.
#[tokio::test]
async fn test_transport_no_retry_when_get_body_yields_nothing() {
    type MakeProvider = Box<dyn Fn() -> Arc<dyn CredentialProvider>>;
    let providers: Vec<(&str, MakeProvider)> = vec![
        (
            "refresh fails",
            Box::new(|| -> Arc<dyn CredentialProvider> {
                Arc::new(ErrRefreshProvider {
                    cred: bearer("stale"),
                })
            }),
        ),
        (
            "refresh succeeds",
            Box::new(|| -> Arc<dyn CredentialProvider> {
                RefreshProvider::new(bearer("stale"), bearer("fresh"))
            }),
        ),
    ];
    for (name, provider) in providers {
        let base = Base::new(&[401, 200]);
        let http = client_over(provider(), &base);
        let probe = Arc::new(BodyProbe::default());

        let resp = http
            .execute(streamed_post("payload", &probe))
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            401,
            "streamed body/{name}: want 401 (nothing to replay, so no retry)"
        );
        assert_eq!(base.calls(), 1, "streamed body/{name}: base calls");
    }
}

#[tokio::test]
async fn test_transport_retry_still_rejected() {
    // Both the original and the refreshed credential are rejected: after one refresh+retry the
    // second 401 is returned, with no further retries.
    let base = Base::new(&[401, 401]);
    let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
    let http = client_over(p.clone(), &base);

    let resp = http.get(URL).send().await.unwrap();

    assert_eq!(resp.status(), 401, "want 401 (retry also rejected)");
    assert_eq!(base.calls(), 2, "base calls (one retry only)");
    assert_eq!(p.refreshes(), 1, "refreshes");
}

// parity: adapted: the replay snapshot is dropped rather than Closed (no live copy of the request is left); the original 401 body is still readable.
#[tokio::test]
async fn test_transport_refresh_error_surfaces_original_and_closes_replay() {
    let live = Arc::new(AtomicUsize::new(0));
    let base = Base::watching(&[401, 200], &live);
    let http = client_over(
        Arc::new(ErrRefreshProvider {
            cred: bearer("stale"),
        }),
        &base,
    );

    let resp = http.execute(watched_post("payload", &live)).await.unwrap();

    assert_eq!(
        resp.status(),
        401,
        "want 401 (refresh error surfaces the original rejection)"
    );
    assert_eq!(base.calls(), 1, "base calls (no retry after refresh error)");
    assert_eq!(
        base.seen()[0].live,
        Some(2),
        "the first send should run alongside exactly one replay snapshot"
    );
    assert_eq!(
        live.load(Ordering::SeqCst),
        0,
        "replay body was not closed after refresh error (leak)"
    );
    // The rejected response is handed back untouched: nothing read it, and its body is intact.
    let body = base.resp_bodies()[0].clone();
    assert!(
        !body.finished(),
        "the surfaced response was read before the caller saw it"
    );
    assert_eq!(resp.text().await.unwrap(), "rejected");
}

// parity: skipped TestTransportRefreshNilCredentialNoPanic: refresh returns Arc<dyn Credential>, never nil (spec R7).

// Refresh is told which credential the downstream rejected. Without it a provider has to guess
// from its own cache, which by then may hold a different credential entirely.
#[tokio::test]
async fn test_transport_passes_the_rejected_credential_to_refresh() {
    let base = Base::new(&[401, 200]);
    let stale = bearer("stale");
    let p = RefreshProvider::new(stale.clone(), bearer("fresh"));
    let http = client_over(p.clone(), &base);

    let _resp = http.get("https://example.com/").send().await.unwrap();

    let rejected = p.rejected().expect("Refresh was not called");
    assert!(
        Arc::ptr_eq(&rejected, &stale),
        "Refresh was told {rejected:?} was rejected, want {stale:?}"
    );
    assert_eq!(
        rejected.downcast_ref::<Bearer>(),
        Some(&Bearer {
            token: "stale".into()
        })
    );
}

// A refresh that needs interactive consent is the one refusal worth surfacing: it is
// actionable, and `credential` reports it the same way, so both paths through one Transport
// agree. Every other refresh failure degrades to the downstream's own rejection.
#[tokio::test]
async fn test_transport_surfaces_consent_from_refresh() {
    let base = Base::new(&[401, 200]);
    let http = client_over(
        Arc::new(ConsentRefreshProvider {
            cred: bearer("stale"),
        }),
        &base,
    );

    let err = match http.get("https://example.com/").send().await {
        Ok(resp) => panic!(
            "RoundTrip() returned a response ({}) alongside the consent error",
            resp.status()
        ),
        Err(err) => err,
    };

    let consent = find_consent_in(&err)
        .unwrap_or_else(|| panic!("RoundTrip() error = {err}, want a ConsentRequiredError"));
    assert_eq!(
        consent.auth_uri, "https://consent.example",
        "AuthURI, want the one the provider reported"
    );
    // Returned as refresh gave it, not wrapped again.
    assert_eq!(err.to_string(), "gcp: auth: interactive consent required");
    assert_eq!(
        base.calls(),
        1,
        "base calls (no retry once consent is required)"
    );
}

// The retry is built from the caller's request, not from the first attempt, so a header the
// first credential set cannot survive into it.
#[tokio::test]
async fn test_transport_retry_does_not_inherit_the_first_credentials_header() {
    let base = Base::new(&[401, 200]);
    let p = RefreshProvider::new(
        Arc::new(ApiKey {
            name: "X-First".into(),
            value: "one".into(),
        }),
        Arc::new(ApiKey {
            name: "X-Second".into(),
            value: "two".into(),
        }),
    );
    let http = client_over(p.clone(), &base);

    let _resp = http.get("https://example.com/").send().await.unwrap();

    let seen = base.seen();
    assert_eq!(seen.len(), 2, "base calls");
    let retry = &seen[1];
    assert_eq!(
        retry.header("X-Second"),
        Some("two"),
        "retry X-Second, want the refreshed credential"
    );
    assert_eq!(
        retry.header("X-First"),
        None,
        "retry still carries X-First from the rejected credential"
    );
}

// The first response's body is read out and closed before the retry, or its connection is not
// returned to the pool.
#[tokio::test]
async fn test_transport_drains_the_rejected_response_before_retrying() {
    let base = Base::new(&[401, 200]);
    let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
    let http = client_over(p.clone(), &base);

    let _resp = http.get("https://example.com/").send().await.unwrap();

    let bodies = base.resp_bodies();
    assert_eq!(bodies.len(), 2, "responses");
    assert!(
        bodies[0].drained(),
        "the rejected response was not drained and closed before the retry"
    );
}

// The same holds on the consent path, which returns no response for the caller to close.
#[tokio::test]
async fn test_transport_drains_the_rejected_response_on_consent() {
    let base = Base::new(&[401, 200]);
    let http = client_over(
        Arc::new(ConsentRefreshProvider {
            cred: bearer("stale"),
        }),
        &base,
    );

    let result = http.get("https://example.com/").send().await;

    assert!(
        result.is_err(),
        "RoundTrip() = nil error, want the consent error"
    );
    let bodies = base.resp_bodies();
    assert_eq!(bodies.len(), 1, "responses");
    assert!(
        bodies[0].drained(),
        "the rejected response was not drained and closed, and nobody else can close it"
    );
}

// Only an auth rejection triggers the refresh. Anything else — success, a server error, a rate
// limit — is returned as it came, or a refresh a new token cannot help becomes a duplicate
// request and a needless re-mint.
#[tokio::test]
async fn test_transport_refreshes_only_on_auth_rejection() {
    for status in [200u16, 400, 429, 500] {
        let base = Base::new(&[status, 200]);
        let p = RefreshProvider::new(bearer("stale"), bearer("fresh"));
        let http = client_over(p.clone(), &base);

        let resp = http.get("https://example.com/").send().await.unwrap();

        assert_eq!(
            resp.status(),
            status,
            "{status}: want {status} returned untouched"
        );
        assert_eq!(base.calls(), 1, "{status}: base calls (no retry)");
        assert_eq!(p.refreshes(), 0, "{status}: refreshes");
    }
}
