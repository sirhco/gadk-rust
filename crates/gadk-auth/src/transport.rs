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

//! The refresh-and-retry transport (port of ADK-Go `auth/transport.go`) as a
//! `reqwest_middleware::Middleware`.
//!
//! [`Transport`] resolves a credential per request through a [`CredentialProvider`] and applies
//! it to the outgoing request's headers. The provider runs on every request, so per-user
//! credentials are never shared across users. Refresh and caching belong to the provider: a
//! token-source-backed one refreshes itself, and a network-backed one can cache in a
//! [`CredentialStore`](crate::CredentialStore).
//!
//! When the provider is a [`RefreshingProvider`](crate::RefreshingProvider) and the response is
//! a 401 or 403, `Transport` refreshes the credential and retries once, provided the request can
//! be replayed (`reqwest::Request::try_clone` succeeds; a streamed body cannot). The retry is not
//! restricted by method, so a server that acts on a request and only then answers 401 or 403 sees
//! the same body twice.

use std::sync::Arc;

use http::Extensions;
use reqwest::StatusCode;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware, Next};

use crate::credential::Credential;
use crate::error::{ConsentRequiredError, Error, find_consent};
use crate::provider::{AuthContext, CredentialProvider};

/// Go `Transport` (transport.go:39) as a `reqwest_middleware::Middleware`. Go's per-request
/// context is the `AuthContext` fixed at construction, so a transport for a per-user provider
/// (such as the gcp one) must be built per invocation identity and not shared across users.
///
/// Errors reach the caller as `reqwest_middleware::Error::Middleware` wrapping a
/// [`crate::Error`]: `auth: resolve credential: …` when the provider fails, `auth: apply
/// credential: …` when the credential cannot be applied. A consent error from a refresh is
/// returned as the provider gave it. Recover a consent error with [`find_consent_in`].
#[derive(Clone)]
pub struct Transport {
    provider: Arc<dyn CredentialProvider>,
    cx: AuthContext,
}

impl Transport {
    /// A transport applying `provider`'s credential, resolved for `cx`, to every request.
    pub fn new(provider: Arc<dyn CredentialProvider>, cx: AuthContext) -> Self {
        Self { provider, cx }
    }
}

impl std::fmt::Debug for Transport {
    /// The context and whether the provider refreshes. The provider itself is not printed: it
    /// is not `Debug`, and it may hold secrets.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("cx", &self.cx)
            .field("refreshing", &self.provider.as_refreshing().is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Middleware for Transport {
    /// Go `RoundTrip` (transport.go:47).
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        // On an error return `req` is dropped, which closes its body (Go's deferred Close).
        let cred = self
            .provider
            .credential(&self.cx)
            .await
            .map_err(|e| middleware_error(Error::ResolveCredential(Box::new(e))))?;

        // The replay snapshot: the caller's request as it arrived, taken before anything is
        // applied or sent, so a retry cannot inherit a header the first credential set. Only a
        // refreshing provider can retry, so only then is it taken (Go calls GetBody only then).
        // `None` is a streamed body, which cannot be replayed.
        let refresher = self.provider.as_refreshing();
        let replay = match refresher {
            Some(_) => req.try_clone(),
            None => None,
        };

        let resp = apply_and_send(req, cred.as_ref(), &mut *extensions, next.clone()).await?;

        // One refresh-and-retry on a downstream auth rejection, when the provider supports
        // refresh and the request can be replayed.
        if !is_auth_rejected(resp.status()) {
            return Ok(resp);
        }
        let (Some(refresher), Some(replay)) = (refresher, replay) else {
            return Ok(resp);
        };

        // Refresh first: the rejected response stays unread until we know what to do with it.
        match refresher.refresh(&self.cx, &cred).await {
            Ok(fresh) => {
                drain(resp).await;
                apply_and_send(replay, fresh.as_ref(), extensions, next).await
            }
            Err(err) => {
                drop(replay);
                // One refusal is worth surfacing: interactive consent is something the caller
                // can act on, and `credential` reports it the same way, so the two paths
                // through this Transport agree on it. Every other failure degrades to the
                // rejection the downstream actually sent, untouched, which is more informative
                // than "we could not refresh".
                if find_consent(&err).is_some() {
                    drain(resp).await;
                    Err(middleware_error(err))
                } else {
                    Ok(resp)
                }
            }
        }
    }
}

/// Go `applyAndSend` (transport.go:131): applies `cred` to `req`'s headers and sends it on. On an
/// apply error nothing is sent, and `req` (with its body) is dropped.
async fn apply_and_send<'a>(
    mut req: reqwest::Request,
    cred: &dyn Credential,
    extensions: &'a mut Extensions,
    next: Next<'a>,
) -> reqwest_middleware::Result<reqwest::Response> {
    if let Err(err) = cred.apply(req.headers_mut()).await {
        return Err(middleware_error(Error::ApplyCredential(Box::new(err))));
    }
    next.run(req, extensions).await
}

/// Go `isAuthRejected` (transport.go:143).
fn is_auth_rejected(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

/// transport.go:164 `maxDrainBytes`.
pub(crate) const MAX_DRAIN_BYTES: usize = 4 << 10;

/// Go `drain` (transport.go:169): reads the body so the connection can be reused, then closes
/// it by dropping the response. The read is capped: an auth-rejection body is small, and a
/// pathological oversized one simply is not drained (and so its connection is not reused)
/// rather than read whole. Reads stop at a chunk boundary, so the last chunk may carry the
/// total past `MAX_DRAIN_BYTES` (Go's `io.LimitReader` stops at exactly 4096).
async fn drain(mut resp: reqwest::Response) {
    let mut read = 0usize;
    while read < MAX_DRAIN_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => read += chunk.len(),
            Ok(None) | Err(_) => break,
        }
    }
}

fn middleware_error(err: Error) -> reqwest_middleware::Error {
    reqwest_middleware::Error::middleware(err)
}

/// `ClientBuilder::new(base).with(Transport::new(provider, cx)).build()`: an HTTP client that
/// applies `provider`'s credential, resolved for `cx`, to every request.
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use gadk_auth::{AuthContext, adc, client};
///
/// // Application Default Credentials, cloud-platform scope.
/// let http = client(reqwest::Client::new(), adc(&[]), AuthContext::anonymous());
/// let resp = http
///     .get("https://storage.googleapis.com/storage/v1/b?project=my-project")
///     .send()
///     .await?;
/// println!("{}", resp.status());
/// # Ok(())
/// # }
/// ```
pub fn client(
    base: reqwest::Client,
    provider: Arc<dyn CredentialProvider>,
    cx: AuthContext,
) -> ClientWithMiddleware {
    ClientBuilder::new(base)
        .with(Transport::new(provider, cx))
        .build()
}

/// Finds a consent error in what `ClientWithMiddleware::send` returned: `Middleware(e)` walks
/// `e.chain()`, `Reqwest(e)` goes through `find_consent`.
///
/// `reqwest_middleware::Error::Middleware` is `transparent` over `anyhow::Error`, whose
/// `source()` skips the `auth::Error` layer, so `find_consent` on the outer error would miss a
/// consent error held directly by that layer. `chain()` starts at the `auth::Error` itself.
pub fn find_consent_in(err: &reqwest_middleware::Error) -> Option<&ConsentRequiredError> {
    match err {
        reqwest_middleware::Error::Middleware(e) => e
            .chain()
            .find_map(|cause| cause.downcast_ref::<ConsentRequiredError>()),
        reqwest_middleware::Error::Reqwest(e) => find_consent(e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fmt;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use futures::Stream;
    use gadk_agent::Identity;

    use super::*;
    use crate::credential::Bearer;
    use crate::provider::{CredentialFuture, ProviderFn, RefreshingProvider};
    use crate::testserver::{Reply, TestServer};

    fn consent() -> ConsentRequiredError {
        ConsentRequiredError {
            auth_uri: "https://consent.example/authorize?login_hint=alice".into(),
            nonce: "n-1".into(),
            key: "k-1".into(),
        }
    }

    fn bearer(token: &str) -> Arc<dyn Credential> {
        Arc::new(Bearer {
            token: token.into(),
        })
    }

    /// A foreign error wrapping a consent error (Go `fmt.Errorf("gcp: %w", consent)`).
    #[derive(Debug)]
    struct Wrapped(ConsentRequiredError);

    impl fmt::Display for Wrapped {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "gcp: {}", self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// Yields `stale`; `refresh` answers `fresh`.
    struct Flip {
        stale: Arc<dyn Credential>,
        fresh: Result<Arc<dyn Credential>, Error>,
        refreshes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl CredentialProvider for Flip {
        async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
            Ok(self.stale.clone())
        }

        fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
            Some(self)
        }
    }

    #[async_trait::async_trait]
    impl RefreshingProvider for Flip {
        async fn refresh(
            &self,
            _cx: &AuthContext,
            _rejected: &Arc<dyn Credential>,
        ) -> Result<Arc<dyn Credential>, Error> {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            self.fresh.clone()
        }
    }

    #[tokio::test]
    async fn find_consent_in_recovers_consent_from_send() {
        let provider = ProviderFn::new(|_cx: &AuthContext| -> CredentialFuture {
            Box::pin(async { Err::<Arc<dyn Credential>, Error>(Error::from(consent())) })
        })
        .into_provider();
        let http = client(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            provider,
            AuthContext::anonymous(),
        );

        // The provider fails before anything is sent, so the address is never contacted.
        let err = http.get("http://127.0.0.1:9/").send().await.unwrap_err();

        assert!(err.is_middleware(), "want a middleware error, got {err:?}");
        assert_eq!(find_consent_in(&err), Some(&consent()));
        assert_eq!(
            err.to_string(),
            "auth: resolve credential: auth: interactive consent required"
        );
    }

    #[tokio::test]
    async fn find_consent_in_recovers_consent_from_refresh() {
        let server = TestServer::start(|_req| async { Reply::status(401, "rejected") }).await;
        let provider = Arc::new(Flip {
            stale: bearer("stale"),
            fresh: Err(Error::other(Wrapped(consent()))),
            refreshes: AtomicUsize::new(0),
        });
        let http = client(server.client(), provider.clone(), AuthContext::anonymous());

        let err = http
            .get(format!("{}/v1/thing", server.url()))
            .send()
            .await
            .unwrap_err();

        assert_eq!(find_consent_in(&err), Some(&consent()));
        assert_eq!(err.to_string(), "gcp: auth: interactive consent required");
        assert_eq!(server.calls(), 1, "no retry once consent is required");
        assert_eq!(provider.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn find_consent_in_finds_nothing_without_consent() {
        let middleware = reqwest_middleware::Error::middleware(Error::ResolveCredential(Box::new(
            Error::msg("boom"),
        )));
        assert_eq!(find_consent_in(&middleware), None);

        let reqwest_err = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get("not a url")
            .build()
            .unwrap_err();
        assert_eq!(
            find_consent_in(&reqwest_middleware::Error::Reqwest(reqwest_err)),
            None
        );
    }

    #[tokio::test]
    async fn client_retries_through_a_real_server() {
        let hits = Arc::new(AtomicUsize::new(0));
        let server = TestServer::start(move |_req| {
            let i = hits.fetch_add(1, Ordering::SeqCst);
            async move {
                if i == 0 {
                    Reply::status(401, "rejected")
                } else {
                    Reply::ok("ok")
                }
            }
        })
        .await;
        let provider = Arc::new(Flip {
            stale: bearer("stale"),
            fresh: Ok(bearer("fresh")),
            refreshes: AtomicUsize::new(0),
        });
        let http = client(server.client(), provider.clone(), AuthContext::anonymous());

        let resp = http
            .post(format!("{}/v1/thing", server.url()))
            .body("payload")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "ok");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].header("authorization"), Some("Bearer stale"));
        assert_eq!(reqs[1].header("authorization"), Some("Bearer fresh"));
        assert_eq!(reqs[0].text(), "payload");
        assert_eq!(reqs[1].text(), "payload");
        assert_eq!(provider.refreshes.load(Ordering::SeqCst), 1);
    }

    /// What a `Chunks` stream observed.
    #[derive(Default)]
    struct Seen {
        chunks_read: AtomicUsize,
        finished: AtomicBool,
        dropped: AtomicBool,
    }

    /// `left` chunks of `size` bytes each.
    struct Chunks {
        left: usize,
        size: usize,
        seen: Arc<Seen>,
    }

    impl Stream for Chunks {
        type Item = Result<Vec<u8>, std::io::Error>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.left == 0 {
                self.seen.finished.store(true, Ordering::SeqCst);
                return Poll::Ready(None);
            }
            self.left -= 1;
            self.seen.chunks_read.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Some(Ok(vec![b'x'; self.size])))
        }
    }

    impl Drop for Chunks {
        fn drop(&mut self) {
            self.seen.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn response(left: usize, size: usize, seen: &Arc<Seen>) -> reqwest::Response {
        let body = reqwest::Body::wrap_stream(Chunks {
            left,
            size,
            seen: seen.clone(),
        });
        reqwest::Response::from(http::Response::new(body))
    }

    #[tokio::test]
    async fn drain_stops_at_max_drain_bytes() {
        let seen = Arc::new(Seen::default());

        drain(response(16, 1024, &seen)).await;

        assert_eq!(
            seen.chunks_read.load(Ordering::SeqCst),
            MAX_DRAIN_BYTES / 1024,
            "drain read past the cap"
        );
        assert!(
            !seen.finished.load(Ordering::SeqCst),
            "an oversized body is not read whole"
        );
        assert!(
            seen.dropped.load(Ordering::SeqCst),
            "the response was not closed"
        );
    }

    #[tokio::test]
    async fn drain_reads_a_small_body_to_the_end() {
        let seen = Arc::new(Seen::default());

        drain(response(1, 8, &seen)).await;

        assert_eq!(seen.chunks_read.load(Ordering::SeqCst), 1);
        assert!(
            seen.finished.load(Ordering::SeqCst),
            "a small body is read to the end"
        );
        assert!(
            seen.dropped.load(Ordering::SeqCst),
            "the response was not closed"
        );
    }

    #[test]
    fn debug_shows_the_context_not_the_provider() {
        let plain = ProviderFn::new(|_cx: &AuthContext| -> CredentialFuture {
            Box::pin(async { Ok::<Arc<dyn Credential>, Error>(bearer("secret-token")) })
        })
        .into_provider();
        assert_eq!(
            format!("{:?}", Transport::new(plain, AuthContext::anonymous())),
            "Transport { cx: AuthContext { identity: None }, refreshing: false, .. }"
        );

        let refreshing: Arc<dyn CredentialProvider> = Arc::new(Flip {
            stale: bearer("secret-token"),
            fresh: Ok(bearer("secret-token")),
            refreshes: AtomicUsize::new(0),
        });
        let cx = AuthContext::for_identity(Identity {
            user_id: "u".into(),
            app_name: "a".into(),
            session_id: "s".into(),
        });
        let got = format!("{:?}", Transport::new(refreshing, cx));
        assert_eq!(
            got,
            "Transport { cx: AuthContext { identity: Some(Identity { user_id: \"u\", \
             app_name: \"a\", session_id: \"s\" }) }, refreshing: true, .. }"
        );
        assert!(!got.contains("secret-token"));
    }
}
