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

//! The built-in providers and the lazy token source (port of ADK-Go `auth/providers.go:124-291`).
//!
//! [`static_token`], [`api_key`] and [`token_source`] resolve at once. [`adc`] and
//! [`service_account`] build their token source lazily, on the first `credential` call: one
//! detached init attempt at a time, each bounded at 30 s, a failed attempt retried by the next
//! call, and a success reused from then on. Tokens are minted when a credential is applied, not
//! when it is resolved.
//!
//! Call these providers inside a Tokio runtime: the lazy init is `tokio::spawn`ed, and
//! google-cloud-auth's token caches spawn their own refresh tasks.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::credential::{Credential, OAuth2, TokenSource};
use crate::error::Error;
use crate::provider::{AuthContext, CredentialProvider};

/// Go `StaticToken` (providers.go:125): a fresh `Bearer { token }` per call.
#[allow(unused_variables, clippy::todo)]
pub fn static_token(token: impl Into<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `APIKey` (providers.go:133).
#[allow(unused_variables, clippy::todo)]
pub fn api_key(name: impl Into<String>, value: impl Into<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `TokenSourceProvider` (providers.go:142): `OAuth2 { token_source }` per call.
#[allow(unused_variables, clippy::todo)]
pub fn token_source(ts: Arc<dyn TokenSource>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ADC(scopes...)` (providers.go:168): lazy, scopes default to cloud-platform when empty.
#[allow(unused_variables, clippy::todo)]
pub fn adc(scopes: &[&str]) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ServiceAccount` (providers.go:193): lazy; audience → ID token; key → access token
/// (scopes required); neither → ADC with `scopes`.
#[allow(unused_variables, clippy::todo)]
pub fn service_account(cfg: ServiceAccountConfig) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// Go `ServiceAccountConfig` (providers.go:177). An empty `json_key` is Go's `len(JSONKey) == 0`.
#[derive(Clone, Default)]
pub struct ServiceAccountConfig {
    /// A service-account key in JSON form; empty means Application Default Credentials.
    pub json_key: Vec<u8>,
    /// Scopes for an OAuth2 access token; ignored when `audience` is set.
    pub scopes: Vec<String>,
    /// When non-empty, an ID token for this audience instead of an access token.
    pub audience: String,
}

impl std::fmt::Debug for ServiceAccountConfig {
    #[allow(unused_variables, clippy::todo)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("Task 2")
    }
}

/// providers.go:152 `scopeCloudPlatform`.
#[allow(dead_code)]
pub(crate) const SCOPE_CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where ADC is read from. `Discover` is google-cloud-auth's real lookup. `Json` stands in for a
/// `GOOGLE_APPLICATION_CREDENTIALS` file, because tests cannot set the variable
/// (`env::set_var` is `unsafe`). Supported `"type"`s: `authorized_user`, `service_account`.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum AdcSource {
    Discover,
    Json(serde_json::Value),
}

/// Go `adcTokenSource` (providers.go:154): an access-token source for `scopes` (cloud-platform
/// when empty). Builder errors are exactly `Error::FindDefaultCredentials(_)`. Synchronous.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn adc_token_source(
    source: &AdcSource,
    scopes: &[String],
) -> Result<Arc<dyn TokenSource>, Error> {
    todo!("Task 2")
}

/// `adc` with an injected source.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn adc_with(source: AdcSource, scopes: Vec<String>) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

/// `service_account` with an injected ADC source for its fallback branch.
#[allow(dead_code, unused_variables, clippy::todo)]
pub(crate) fn service_account_with(
    cfg: ServiceAccountConfig,
    source: AdcSource,
) -> Arc<dyn CredentialProvider> {
    todo!("Task 2")
}

// ---- Lazy token source (providers.go:226-291) ----

/// providers.go:227 `initTimeout`: bounds a hung init so it fails and is retried, not wedged
/// forever. A parameter of [`lazy_token_source`] in Rust, so tests pass a short bound.
#[allow(dead_code)] // Task 2 Cycle 2 removes this allow
pub(crate) const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a lazy init resolves to.
#[allow(dead_code)] // Task 2 Cycle 2 removes this allow
pub(crate) type InitFuture = BoxFuture<'static, Result<Arc<dyn TokenSource>, Error>>;

/// One init attempt (Go `*attempt`). Every waiter awaits its own clone and gets a clone of the
/// result (Go `close(a.done)`). Dropping a clone gives up that waiter only: the attempt runs on
/// its own task.
type Attempt = Shared<BoxFuture<'static, Result<Arc<dyn TokenSource>, Error>>>;

/// Go's `mu`-guarded pair (providers.go:238-242).
#[derive(Default)]
struct LazyState {
    /// Go `ts`: the source of the first attempt that succeeded within its bound.
    ts: Option<Arc<dyn TokenSource>>,
    /// Go `cur`: the in-flight attempt, for single flight. The attempt clears it when it ends.
    cur: Option<Attempt>,
}

/// Go `lazyTokenSource`'s provider.
struct LazyTokenSource<F> {
    init: Arc<F>,
    bound: Duration,
    state: Arc<Mutex<LazyState>>,
}

/// Go `lazyTokenSource` (providers.go:232). One shared attempt, `tokio::spawn`ed. The bound is a
/// timer inside the attempt: when it fires the token is cancelled, and the attempt's result
/// becomes `Error::InitTimeout { bound, init_error }` (the init's text when it failed); a
/// source landing past the bound is discarded. A failure resets the slot. Each success is an
/// `OAuth2 { token_source }` over the memoized source.
///
/// Waiters are cancelled only by dropping their own future; that never stops the attempt, whose
/// result is still memoized for the next call. The token handed to `init` is cancelled only when
/// the bound fires, never because init returned, since a token source may keep using it.
#[allow(dead_code)] // Task 2 Cycle 2 removes this allow
pub(crate) fn lazy_token_source<F>(init: F, bound: Duration) -> Arc<dyn CredentialProvider>
where
    F: Fn(CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    Arc::new(LazyTokenSource {
        init: Arc::new(init),
        bound,
        state: Arc::new(Mutex::new(LazyState::default())),
    })
}

impl<F> LazyTokenSource<F>
where
    F: Fn(CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    /// Go's `go func() { … }()` (providers.go:254-277): starts the attempt on its own task, so
    /// no waiter giving up can stop it.
    fn spawn_attempt(&self) -> Attempt {
        let task = tokio::spawn(run_attempt(
            Arc::clone(&self.init),
            Arc::clone(&self.state),
            self.bound,
        ));
        async move {
            task.await.unwrap_or_else(|join| {
                // run_attempt catches init's panics, so only a runtime shutting down gets here.
                Err(Error::msg(format!(
                    "auth: credential initialization did not finish: {join}"
                )))
            })
        }
        .boxed()
        .shared()
    }
}

#[async_trait::async_trait]
impl<F> CredentialProvider for LazyTokenSource<F>
where
    F: Fn(CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    async fn credential(&self, _cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
        let attempt = {
            let mut st = self.state.lock();
            if let Some(ts) = &st.ts {
                return Ok(oauth2(Arc::clone(ts)));
            }
            st.cur.get_or_insert_with(|| self.spawn_attempt()).clone()
        };
        // Go's `select` on `a.done` and `ctx.Done()`: a caller that gives up drops this future.
        attempt.await.map(oauth2)
    }
}

/// Go `OAuth2Credential{TokenSource: ts}`.
fn oauth2(token_source: Arc<dyn TokenSource>) -> Arc<dyn Credential> {
    Arc::new(OAuth2 { token_source })
}

/// One attempt's body (providers.go:254-277).
async fn run_attempt<F>(
    init: Arc<F>,
    state: Arc<Mutex<LazyState>>,
    bound: Duration,
) -> Result<Arc<dyn TokenSource>, Error>
where
    F: Fn(CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    // Go `ictx`: the token source may refresh on it, so it must outlive init. A timer bounds init
    // instead of a deadline because a timer can be stopped.
    let cancel = CancellationToken::new();
    let init_cancel = cancel.clone();
    // A panicking init fails this attempt like an error would, so the slot still resets.
    let attempt = AssertUnwindSafe(async move { (*init)(init_cancel).await }).catch_unwind();
    let timer = tokio::time::sleep(bound);
    tokio::pin!(attempt, timer);
    let mut fired = false;
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut attempt => break outcome,
            () = &mut timer, if !fired => {
                // Go `time.AfterFunc(initTimeout, cancel)`: init keeps running and returns when
                // it sees the cancellation.
                fired = true;
                cancel.cancel();
            }
        }
    };
    let mut result = outcome
        .unwrap_or_else(|_panic| Err(Error::msg("auth: credential initialization panicked")));
    if fired {
        // Go `!timer.Stop()`: the token is cancelled, so a source init returned is unusable. The
        // init's error is kept as text (Go `%v`), not as a source: it may report the
        // cancellation, and the caller must see a timeout.
        result = Err(Error::InitTimeout {
            bound,
            init_error: result.err().map(|e| e.to_string()),
        });
    }
    let mut st = state.lock();
    if let Ok(ts) = &result {
        st.ts = Some(Arc::clone(ts));
    }
    st.cur = None; // reset so a failure is retried
    drop(st);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod lazy_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::time::Instant;

    use super::*;
    use crate::credential::{StaticTokenSource, Token};

    fn tok() -> Arc<dyn TokenSource> {
        Arc::new(StaticTokenSource(Token {
            access_token: "tok".into(),
            token_type: String::new(),
        }))
    }

    fn ready(result: Result<Arc<dyn TokenSource>, Error>) -> InitFuture {
        std::future::ready(result).boxed()
    }

    #[tokio::test(start_paused = true)]
    async fn the_bound_is_per_attempt_not_per_waiter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let p = lazy_token_source(
            {
                let calls = Arc::clone(&calls);
                move |cancel: CancellationToken| -> InitFuture {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        cancel.cancelled().await;
                        Err(Error::msg("context canceled"))
                    }
                    .boxed()
                }
            },
            INIT_TIMEOUT,
        );
        let start = Instant::now();
        let first = tokio::spawn({
            let p = Arc::clone(&p);
            async move { p.credential(&AuthContext::anonymous()).await.map(|_| ()) }
        });
        // A second waiter joins halfway through the attempt; its wait ends with the attempt's
        // bound, not 30 s after it joined.
        tokio::time::sleep(INIT_TIMEOUT / 2).await;
        let second = p.credential(&AuthContext::anonymous()).await.map(|_| ());
        let waited = start.elapsed();
        assert!(
            waited >= INIT_TIMEOUT && waited < INIT_TIMEOUT + INIT_TIMEOUT / 4,
            "the second waiter returned after {waited:?}, want the attempt's own 30s bound"
        );
        for r in [first.await.unwrap(), second] {
            assert!(matches!(r, Err(Error::InitTimeout { .. })), "{r:?}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_panicking_init_fails_and_the_next_call_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let p = lazy_token_source(
            {
                let calls = Arc::clone(&calls);
                move |_: CancellationToken| -> InitFuture {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("init blew up");
                    }
                    ready(Ok(tok()))
                }
            },
            INIT_TIMEOUT,
        );
        let err = p
            .credential(&AuthContext::anonymous())
            .await
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.to_string(), "auth: credential initialization panicked");
        p.credential(&AuthContext::anonymous())
            .await
            .map(|_| ())
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
