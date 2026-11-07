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

//! Port of Go `auth/providers_internal_test.go` (Task 2).
//!
//! Every `TestLazyTokenSource*` case runs on paused Tokio time. Go's `initTimeout` override is
//! the `bound` argument of `lazy_token_source`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt as _;
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::credential::{OAuth2, StaticTokenSource, Token, TokenSource};
use crate::error::{Error, SharedError};
use crate::provider::AuthContext;
use crate::providers::{INIT_TIMEOUT, InitFuture, lazy_token_source};

/// Go `t.Context()`: no invocation identity.
fn cx() -> AuthContext {
    AuthContext::anonymous()
}

/// Go `oauth2.StaticTokenSource(&oauth2.Token{AccessToken: "tok"})`.
fn tok_source() -> Arc<dyn TokenSource> {
    Arc::new(StaticTokenSource(Token {
        access_token: "tok".into(),
        token_type: String::new(),
    }))
}

/// An init that has already finished.
fn ready(result: Result<Arc<dyn TokenSource>, Error>) -> InitFuture {
    std::future::ready(result).boxed()
}

/// Go's `release` channel: `wait` parks until `open`, which releases every waiter at once (Go
/// `close(release)`).
#[derive(Clone)]
struct Gate(Arc<watch::Sender<bool>>);

impl Gate {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    fn open(&self) {
        self.0.send_replace(true);
    }

    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_memoizes_success() {
    let calls = Arc::new(AtomicUsize::new(0));
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            move |_: CancellationToken| -> InitFuture {
                calls.fetch_add(1, Ordering::SeqCst);
                ready(Ok(tok_source()))
            }
        },
        INIT_TIMEOUT,
    );

    for i in 0..3 {
        let cred = p
            .credential(&cx())
            .await
            .unwrap_or_else(|e| panic!("call {i}: Credential() error = {e}"));
        assert!(
            cred.downcast_ref::<OAuth2>().is_some(),
            "call {i}: got {cred:?}, want OAuth2 with a token source"
        );
    }
    let calls = calls.load(Ordering::SeqCst);
    assert_eq!(
        calls, 1,
        "init called {calls} times, want 1 (success should be memoized)"
    );
}

#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_retries_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            move |_: CancellationToken| -> InitFuture {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return ready(Err(Error::msg("boom")));
                }
                ready(Ok(tok_source()))
            }
        },
        INIT_TIMEOUT,
    );

    match p.credential(&cx()).await {
        Ok(cred) => panic!("first call = {cred:?}, want boom"),
        Err(err) => assert!(
            matches!(&err, Error::Other(_)) && err.to_string() == "boom",
            "first call error = {err:?}, want boom"
        ),
    }
    if let Err(err) = p.credential(&cx()).await {
        panic!("second call error = {err}, want nil (failure must not be memoized)");
    }
    let calls = calls.load(Ordering::SeqCst);
    assert_eq!(calls, 2, "init called {calls} times, want 2");
}

/// Concurrent callers must trigger init once.
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_single_flight() {
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Gate::new();
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            let release = release.clone();
            move |_: CancellationToken| -> InitFuture {
                calls.fetch_add(1, Ordering::SeqCst);
                let release = release.clone();
                async move {
                    release.wait().await; // block in init so concurrent callers pile up
                    Ok(tok_source())
                }
                .boxed()
            }
        },
        INIT_TIMEOUT,
    );

    const N: usize = 50;
    let callers: Vec<_> = (0..N)
        .map(|_| {
            let p = Arc::clone(&p);
            tokio::spawn(async move { p.credential(&cx()).await.map(|_| ()) })
        })
        .collect();
    // Paused time moves only once every task is parked, so when this sleep returns all N
    // callers are waiting on the attempt (Go: the goroutines pile up behind `<-release`).
    tokio::time::sleep(Duration::from_millis(1)).await;
    release.open();

    for (i, caller) in callers.into_iter().enumerate() {
        if let Err(err) = caller.await.unwrap() {
            panic!("goroutine {i}: Credential() error = {err}");
        }
    }
    let got = calls.load(Ordering::SeqCst);
    assert_eq!(got, 1, "init called {got} times, want 1 (single-flight)");
}

/// A caller that gives up returns promptly instead of blocking on a slow init, while the detached
/// init still completes so a later caller reuses it (one init overall).
// parity: adapted: a Rust caller gives up by dropping its future (driven with `tokio::time::timeout`), so there is no `context.Canceled` error to assert; the detached init still completes and is reused once.
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_honors_caller_cancellation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Gate::new();
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            let release = release.clone();
            move |_: CancellationToken| -> InitFuture {
                calls.fetch_add(1, Ordering::SeqCst);
                let release = release.clone();
                async move {
                    release.wait().await; // hold init open until the test releases it
                    Ok(tok_source())
                }
                .boxed()
            }
        },
        INIT_TIMEOUT,
    );

    // A caller whose deadline has already passed (Go: an already-canceled context) must not
    // block on the init: `timeout(ZERO, …)` polls the call once, which starts the attempt, then
    // drops it.
    let start = Instant::now();
    let gave_up = tokio::time::timeout(Duration::ZERO, p.credential(&cx())).await;
    assert!(
        gave_up.is_err(),
        "Credential(canceled) = {gave_up:?}, want the caller to give up"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "the canceled caller waited {:?} on the init",
        start.elapsed()
    );

    // Let the detached init finish; a fresh caller then reuses the memoized source.
    release.open();
    let cred = p
        .credential(&cx())
        .await
        .unwrap_or_else(|e| panic!("Credential() error = {e}"));
    assert!(
        cred.downcast_ref::<OAuth2>().is_some(),
        "got {cred:?}, want OAuth2 with a token source"
    );
    let got = calls.load(Ordering::SeqCst);
    assert_eq!(
        got, 1,
        "init called {got} times, want 1 (detached init reused)"
    );
}

/// A hung init is bounded, so it fails with a deadline and the provider recovers: a later call
/// retries instead of waiting on the same stuck init forever.
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_bounds_hung_init() {
    let bound = Duration::from_millis(20); // Go: initTimeout = 20 * time.Millisecond
    let calls = Arc::new(AtomicUsize::new(0));
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            move |cancel: CancellationToken| -> InitFuture {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    cancel.cancelled().await; // hang until the bound fires
                    Err(Error::msg("context canceled")) // Go: return nil, ctx.Err()
                }
                .boxed()
            }
        },
        bound,
    );

    for i in 0..2 {
        match p.credential(&cx()).await {
            Ok(cred) => panic!("call {i}: Credential() = {cred:?}, want context deadline exceeded"),
            Err(err) => {
                assert!(
                    matches!(err, Error::InitTimeout { .. }),
                    "call {i}: Credential() error = {err:?}, want context deadline exceeded"
                );
                assert_eq!(
                    err.to_string(),
                    "auth: credential initialization exceeded 20ms: context deadline exceeded: context canceled"
                );
            }
        }
    }
    let got = calls.load(Ordering::SeqCst);
    assert_eq!(
        got, 2,
        "init called {got} times, want 2 (each call retries after a bounded hang)"
    );
}

/// The token init receives stays live after init returns, since the token source refreshes on
/// it.
// parity: adapted: init receives a `CancellationToken` instead of a context; the test asserts the token is not cancelled after init returns.
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_init_context_outlives_init() {
    let bound = Duration::from_millis(100); // Go: initTimeout = 100 * time.Millisecond
    let init_token: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
    let p = lazy_token_source(
        {
            let init_token = Arc::clone(&init_token);
            move |cancel: CancellationToken| -> InitFuture {
                *init_token.lock() = Some(cancel);
                ready(Ok(tok_source()))
            }
        },
        bound,
    );
    if let Err(err) = p.credential(&cx()).await {
        panic!("Credential() error = {err}");
    }

    let token = init_token.lock().clone().expect("init ran");
    tokio::select! {
        () = token.cancelled() => panic!("init context done after init returned"),
        () = tokio::time::sleep(bound * 3) => {}
    }
}

/// A source returned after the bound fired is not cached.
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_discards_source_built_past_timeout() {
    let bound = Duration::from_millis(10); // Go: initTimeout = 10 * time.Millisecond
    let calls = Arc::new(AtomicUsize::new(0));
    let p = lazy_token_source(
        {
            let calls = Arc::clone(&calls);
            move |cancel: CancellationToken| -> InitFuture {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return async move {
                        cancel.cancelled().await;
                        Ok(tok_source())
                    }
                    .boxed();
                }
                ready(Ok(tok_source()))
            }
        },
        bound,
    );

    match p.credential(&cx()).await {
        Ok(cred) => panic!("first Credential() = {cred:?}, want context deadline exceeded"),
        Err(err) => {
            assert!(
                matches!(err, Error::InitTimeout { .. }),
                "first Credential() error = {err:?}, want context deadline exceeded"
            );
            assert_eq!(
                err.to_string(),
                "auth: credential initialization exceeded 10ms: context deadline exceeded"
            );
        }
    }
    if let Err(err) = p.credential(&cx()).await {
        panic!("second Credential() error = {err}, want a retried init to succeed");
    }
    let got = calls.load(Ordering::SeqCst);
    assert_eq!(
        got, 2,
        "init called {got} times, want 2 (the late source must not be cached)"
    );
}

/// A timed-out init keeps init's error text but does not report as the init's own failure.
// parity: adapted: the init error is kept as text inside `Error::InitTimeout`; Go's "not `context.Canceled`" check becomes "the variant is `InitTimeout` and the text names `find default credentials`".
#[tokio::test(start_paused = true)]
async fn test_lazy_token_source_keeps_init_error_text_past_timeout() {
    let bound = Duration::from_millis(100); // Go: initTimeout = 100 * time.Millisecond
    let p = lazy_token_source(
        move |cancel: CancellationToken| -> InitFuture {
            async move {
                cancel.cancelled().await;
                // Go: fmt.Errorf("auth: find default credentials: %w", ctx.Err())
                Err(Error::FindDefaultCredentials(SharedError::from(
                    "context canceled",
                )))
            }
            .boxed()
        },
        bound,
    );

    let err = match p.credential(&cx()).await {
        Ok(cred) => panic!("Credential() = {cred:?}, want context deadline exceeded"),
        Err(err) => err,
    };
    assert!(
        matches!(err, Error::InitTimeout { .. }),
        "Credential() error = {err:?}, want it to wrap context deadline exceeded"
    );
    // Go `!errors.Is(err, context.Canceled)`: the init's error is text, not a source, so nothing
    // walking the chain finds the init's own failure.
    assert!(
        std::error::Error::source(&err).is_none(),
        "Credential() error = {err:?}, want a timeout not to report as a cancellation"
    );
    assert!(
        err.to_string().contains("find default credentials"),
        "Credential() error = {err}, want it to name what init was doing"
    );
    assert_eq!(
        err.to_string(),
        "auth: credential initialization exceeded 100ms: context deadline exceeded: auth: find default credentials: context canceled"
    );
}
