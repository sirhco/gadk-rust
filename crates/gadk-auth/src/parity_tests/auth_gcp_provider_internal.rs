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

//! Port of Go `auth/gcp/provider_internal_test.go` (package `gcp`).
//!
//! The lazy default-client tests run under paused time (`start_paused = true`): every bound
//! (`init_timeout`, `evict_timeout`, the caller's own deadline) is a `tokio::time` timer, so the
//! runtime auto-advances to it instead of waiting in real time. Tests that talk to a
//! `TestServer` run on the real clock, because a paused runtime would auto-advance the HTTP
//! client's own timers while it waits on a socket.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use futures::FutureExt;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::Credential;
use crate::gcp::client::{CLIENT_NONCE, Client, Config, Request, join_fields, new_client_nonce};
use crate::gcp::provider::{
    ClientInit, MAX_CACHED_LIFETIME, NewClientFn, Provider, ProviderConfig, ProviderScheme,
    REFRESH_COOLDOWN, build_provider, cache_until,
};
use crate::gcp::{Error, ErrorKind, is_kind};
use crate::providers::AdcSource;
use crate::store::{CredentialKey, CredentialStore, EXPIRY_SKEW, InMemoryCredentialStore};
use crate::testserver::{Reply, TestServer};

/// Go client_test.go `authProviderResource`.
const AUTH_PROVIDER_RESOURCE: &str = "projects/p/locations/l/authProviders/ap";

/// Go `newTestProvider`: a provider with no configured client, so `resolve_client` takes the
/// lazy default-client path.
fn new_test_provider() -> Provider {
    build_provider(ProviderConfig {
        scheme: ProviderScheme {
            name: AUTH_PROVIDER_RESOURCE.to_string(),
            ..ProviderScheme::default()
        },
        ..ProviderConfig::default()
    })
    .expect("NewProvider()")
}

/// A `NewClientFn` from a closure returning any `Send` future (the boxing is the seam's).
fn builder<F, Fut>(f: F) -> NewClientFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Arc<Client>, Error>> + Send + 'static,
{
    Arc::new(move || f().boxed())
}

/// Go `&Client{httpClient: http.DefaultClient}`: a real `Client` that never discovers ADC.
fn plain_client() -> Arc<Client> {
    Arc::new(
        Client::new(Config {
            http_client: Some(reqwest::Client::new().into()),
            ..Config::default()
        })
        .expect("NewClient()"),
    )
}

/// Go `blockingInit`: a client init that never returns (os.ReadFile on a stalled mount, a hung
/// resolver inside the metadata probe). The spawned attempt is aborted with the test's runtime.
fn blocking_init() -> NewClientFn {
    builder(futures::future::pending::<Result<Arc<Client>, Error>>)
}

/// A builder that counts its attempts and holds each one open until `release` is notified.
fn held_builder(built: Arc<Client>, release: Arc<Notify>, attempts: Arc<AtomicUsize>) -> NewClientFn {
    builder(move || {
        let built = Arc::clone(&built);
        let release = Arc::clone(&release);
        let attempts = Arc::clone(&attempts);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            release.notified().await;
            Ok(built)
        }
    })
}

/// Go client_test.go `fakeADC`: an `authorized_user` key, which needs no key parsing, whose
/// token endpoint is a local server. It goes through `AdcSource::Json` because
/// `GOOGLE_APPLICATION_CREDENTIALS` cannot be set in-process without `unsafe`. Keep the server
/// alive for the test's length.
async fn fake_adc() -> (TestServer, AdcSource) {
    let token_srv = TestServer::start(|_| async {
        Reply::json(r#"{"access_token":"ADC-TOKEN","token_type":"Bearer","expires_in":1}"#)
    })
    .await;
    let key = serde_json::json!({
        "type": "authorized_user",
        "client_id": "c",
        "client_secret": "s",
        "refresh_token": "r",
        "token_uri": token_srv.url(),
    });
    (token_srv, AdcSource::Json(key))
}

fn pending_is_set(p: &Provider) -> bool {
    p.state.lock().pending.is_some()
}

/// Go's `panic("credentials discovery blew up")` builder: a typed fn, so the async block that
/// calls it has the builder's output type.
fn discovery_blows_up() -> Result<Arc<Client>, Error> {
    panic!("credentials discovery blew up")
}

// TestResolveClientBuildsDefaultClient drives the lazy ADC path end to end: discovery, the
// cached client, and a retrieval whose token is minted after the request that triggered
// construction is already gone.
// parity: adapted: the built Arc<Client> cannot be retargeted afterwards, so the injected builder builds an ADC client (fake authorized_user key through with_adc) whose agent identity endpoint is the TestServer; the triggering caller's future has completed before the retrieval mints a token
#[tokio::test]
async fn test_resolve_client_builds_default_client() {
    let (_token_srv, adc) = fake_adc().await;
    let srv =
        TestServer::sequence(&[r#"{"success":{"token":"tok","header":"Authorization: Bearer"}}"#])
            .await;

    let mut p = new_test_provider();
    let endpoint = srv.url().to_string();
    p.new_client = builder(move || {
        let adc = adc.clone();
        let endpoint = endpoint.clone();
        async move {
            Client::with_adc(
                Config {
                    agent_identity_endpoint: endpoint,
                    ..Config::default()
                },
                &adc,
            )
            .map(Arc::new)
        }
    });

    let c = p.resolve_client().await.expect("resolve_client()");
    c.retrieve_credential(Request {
        resource: AUTH_PROVIDER_RESOURCE.to_string(),
        user_id: "u".to_string(),
        ..Request::default()
    })
    .await
    .expect("RetrieveCredential()");
    assert_eq!(srv.calls(), 1, "service calls = {}, want 1", srv.calls());

    match p.resolve_client().await {
        Ok(again) if Arc::ptr_eq(&again, &c) => {}
        other => panic!("resolve_client() = {other:?}; want the cached client"),
    }
}

// TestResolveClientSingleFlight pins the concurrency design: many first callers share one build
// and all get the same client.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_single_flight() {
    const CALLERS: usize = 16;
    let attempts = Arc::new(AtomicUsize::new(0));
    let built = plain_client();
    let release = Arc::new(Notify::new());
    let mut p = new_test_provider();
    p.new_client = held_builder(Arc::clone(&built), Arc::clone(&release), Arc::clone(&attempts));
    let p = Arc::new(p);

    let handles: Vec<_> = (0..CALLERS)
        .map(|_| {
            let p = Arc::clone(&p);
            tokio::spawn(async move { p.resolve_client().await })
        })
        .collect();
    // Hold the flight open so every caller piles up on it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.notify_one();
    let mut got = Vec::with_capacity(CALLERS);
    for h in handles {
        got.push(h.await.expect("caller task"));
    }

    let n = attempts.load(Ordering::SeqCst);
    assert_eq!(
        n, 1,
        "init attempts = {n}, want 1 (concurrent callers must share one flight)"
    );
    for (i, r) in got.iter().enumerate() {
        match r {
            Ok(c) if Arc::ptr_eq(c, &built) => {}
            other => panic!("caller {i} = {other:?}; want the single shared client"),
        }
    }
}

// TestResolveClientHonorsCallerDeadline pins that a caller waiting on a slow init is bounded by
// its own deadline: a cold start must not stall every concurrent request.
// parity: adapted: the caller's deadline is tokio::time::timeout around resolve_client under paused time, so Go's context.DeadlineExceeded is the timeout's Elapsed; the timeout fires well before init_timeout
#[tokio::test(start_paused = true)]
async fn test_resolve_client_honors_caller_deadline() {
    let mut p = new_test_provider();
    p.new_client = blocking_init();

    let deadline = Duration::from_millis(50);
    let start = Instant::now();
    let res = tokio::time::timeout(deadline, p.resolve_client()).await;
    let elapsed = start.elapsed();
    assert!(
        res.is_err(),
        "resolve_client() = {res:?}, want the caller's deadline"
    );
    assert!(
        elapsed < p.init_timeout,
        "resolve_client() returned after {elapsed:?}, want the caller's {deadline:?} deadline, not the {:?} init bound",
        p.init_timeout
    );
}

// TestResolveClientBoundsWaitOnHungInit pins the init timeout: a lookup that never returns
// cannot be cancelled, so a caller with no deadline of its own is still released, and the
// attempt itself is kept.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_bounds_wait_on_hung_init() {
    let mut p = new_test_provider();
    p.new_client = blocking_init();
    p.init_timeout = Duration::from_millis(50);

    let err = p
        .resolve_client()
        .await
        .expect_err("resolve_client() = a client, want ErrClientUnavailable");
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "resolve_client() error = {err}, want ErrClientUnavailable"
    );
    assert_eq!(
        err.to_string(),
        "gcp: default credentials client unavailable: the attempt exceeded 50ms and is still running"
    );
    // A sentinel of its own: a caller must be able to tell it from a caller deadline.
    assert!(
        !err.to_string().contains("context deadline exceeded"),
        "resolve_client() error = {err}, want it distinguishable from a caller deadline"
    );
    // Retiring the attempt would start a fresh uncancellable lookup every init_timeout.
    assert!(
        pending_is_set(&p),
        "provider dropped the hung attempt; the next caller would start another lookup"
    );
}

// TestResolveClientPublishesLateClient pins that a lookup which lands after every waiter gave up
// is not wasted: the next caller gets its client instead of starting another lookup.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_publishes_late_client() {
    let built = plain_client();
    let release = Arc::new(Notify::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut p = new_test_provider();
    p.init_timeout = Duration::from_millis(20);
    p.new_client = held_builder(Arc::clone(&built), Arc::clone(&release), Arc::clone(&attempts));

    let err = p
        .resolve_client()
        .await
        .expect_err("resolve_client() = a client, want ErrClientUnavailable");
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "resolve_client() error = {err}, want ErrClientUnavailable"
    );
    release.notify_one();

    // The attempt finishes on its own task, so wait for it to publish.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut got = None;
    while Instant::now() < deadline {
        if let Ok(c) = p.resolve_client().await {
            got = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    match &got {
        Some(c) if Arc::ptr_eq(c, &built) => {}
        other => panic!("resolve_client() = {other:?}, want the late-landing client"),
    }
    let n = attempts.load(Ordering::SeqCst);
    assert_eq!(
        n, 1,
        "init attempts = {n}, want 1 (the late client must be used, not rebuilt)"
    );
}

// TestRunInitSurvivesAbruptBuilder pins that a builder which does not return normally still
// releases the waiters and the in-flight slot.
// parity: adapted: the panic case is kept (the panic surfaces through the inner spawned task's JoinError as ClientUnavailable with "building it panicked: <payload>"); the runtime.Goexit case has no Rust counterpart
#[tokio::test(start_paused = true)]
async fn test_run_init_survives_abrupt_builder() {
    let cases: [(&str, NewClientFn, &str); 1] = [(
        "panic",
        builder(|| async { discovery_blows_up() }),
        "blew up", // surfaced, not swallowed
    )];
    for (name, build, want_msg) in cases {
        let mut p = new_test_provider();
        p.init_timeout = Duration::from_secs(5); // long enough that a wedge shows up as one
        p.new_client = build;

        let Ok(res) = tokio::time::timeout(Duration::from_secs(2), p.resolve_client()).await
        else {
            panic!("{name}: resolve_client() blocked; an abrupt builder wedged the provider");
        };
        let err = res.expect_err("resolve_client() = a client, want the builder's failure");
        assert!(
            err.to_string().contains(want_msg),
            "{name}: resolve_client() error = {err}, want it to report {want_msg:?}"
        );
        assert_eq!(
            err.to_string(),
            "gcp: default credentials client unavailable: building it panicked: credentials discovery blew up"
        );
        // A caller behind a transport cannot switch on a message.
        assert!(
            is_kind(&err, ErrorKind::ClientUnavailable),
            "{name}: resolve_client() error = {err}, want ErrorKind::ClientUnavailable"
        );
        assert!(
            !pending_is_set(&p),
            "{name}: provider kept the dead attempt; every later call would fail on it instead of retrying"
        );
    }
}

// TestResolveClientFailsFastOnceTheAttemptBoundPasses pins that a stuck lookup costs the bound
// once, not once per request.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_fails_fast_once_the_attempt_bound_passes() {
    let mut p = new_test_provider();
    p.new_client = blocking_init();
    p.init_timeout = Duration::from_millis(200);

    let start = Instant::now();
    let err = p.resolve_client().await.expect_err("first resolve_client()");
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "first resolve_client() error = {err}, want ErrClientUnavailable"
    );
    let first = start.elapsed();

    let start = Instant::now();
    let err = p.resolve_client().await.expect_err("second resolve_client()");
    let second = start.elapsed();
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "second resolve_client() error = {err}, want ErrClientUnavailable"
    );
    assert!(
        second <= p.init_timeout / 4,
        "second caller waited {second:?} against a {:?} bound (first paid {first:?}): once the attempt's bound has passed, a later caller must not wait again",
        p.init_timeout
    );
}

// TestResolveClientDiscoveryFailureIsMatchable pins that discovery failing outright carries the
// same sentinel as the timeout.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_discovery_failure_is_matchable() {
    let mut p = new_test_provider();
    p.new_client = builder(|| async { Err(Error::new("no credentials on this host")) });

    let err = p.resolve_client().await.expect_err("resolve_client()");
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "resolve_client() error = {err}, want it to wrap ErrClientUnavailable"
    );
    assert!(
        err.to_string().contains("no credentials on this host"),
        "resolve_client() error = {err}, want the underlying cause kept"
    );
    assert_eq!(
        err.to_string(),
        "gcp: default credentials client unavailable: no credentials on this host"
    );
}

// TestResolveClientRetriesFailedInit pins that a failed discovery is not cached.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_retries_failed_init() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let mut p = new_test_provider();
    p.new_client = builder(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            if n == 1 {
                Err(Error::new("no credentials"))
            } else {
                Ok(plain_client())
            }
        }
    });

    assert!(
        p.resolve_client().await.is_err(),
        "resolve_client() = a client, want the discovery failure"
    );
    if let Err(err) = p.resolve_client().await {
        panic!("resolve_client() after a failed attempt: error = {err}, want a retry");
    }
    let got = attempts.load(Ordering::SeqCst);
    assert_eq!(
        got, 2,
        "init attempts = {got}, want 2 (a failure must not be cached)"
    );
}

// parity: skipped TestResolveClientRejectsNilClient: the builder returns Result<Arc<Client>, Error>, so Go's (nil, nil) cannot happen
// parity: skipped TestNewProviderKeepsWiringContextValuesNotCancellation: new_provider takes no context, and Rust has no context values
// parity: skipped TestNewProviderKeepsWiringContextOnlyWhenLazy: no wiring context exists to retain
// parity: skipped TestDefaultBuilderPassesItsArgumentToNewClient: it pins that oauth2.HTTPClient travels on the context into NewClient; Rust has neither the context nor that hook

// TestResolveClientBoundIsPerAttemptNotPerWaiter pins that the bound belongs to the attempt: a
// waiter arriving while a lookup is already stuck is released when that lookup's bound expires.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_bound_is_per_attempt_not_per_waiter() {
    let mut p = new_test_provider();
    p.new_client = blocking_init();
    p.init_timeout = Duration::from_millis(400);
    let p = Arc::new(p);

    let start = Instant::now();
    let first = {
        let p = Arc::clone(&p);
        tokio::spawn(async move {
            let _ = p.resolve_client().await;
            start.elapsed()
        })
    };
    // Wait for the first caller to have created the attempt rather than sleeping on it.
    while !pending_is_set(&p) {
        tokio::task::yield_now().await;
    }

    // Arrive partway through the attempt's bound, so this caller shares it.
    tokio::time::sleep(p.init_timeout / 2).await;
    let late_start = Instant::now();
    let res = p.resolve_client().await;
    let late = late_start.elapsed();
    match &res {
        Err(err) if is_kind(err, ErrorKind::ClientUnavailable) => {}
        other => panic!("late resolve_client() = {other:?}, want ErrClientUnavailable"),
    }
    match tokio::time::timeout(Duration::from_secs(2), first).await {
        Ok(joined) => {
            joined.expect("first caller task");
        }
        Err(_) => panic!("the first caller never returned"),
    }
    assert!(
        late < p.init_timeout,
        "a caller arriving halfway through the bound waited {late:?}, a full bound being {:?}: the bound must be the attempt's, not the waiter's",
        p.init_timeout
    );
}

// TestResolveClientPrefersALandedResultOverAnExpiredBound pins that a caller arriving with both
// the result and the bound ready gets the client. Rust's `select!` is `biased` toward the
// result, so one trial is deterministic; the 200 trials are Go's and are kept.
#[tokio::test(start_paused = true)]
async fn test_resolve_client_prefers_a_landed_result_over_an_expired_bound() {
    let built = plain_client();
    // Paused time starts at the runtime's creation; move past an hour so "an hour ago" exists.
    tokio::time::advance(Duration::from_secs(2 * 3600)).await;
    for i in 0..200 {
        let p = new_test_provider();
        // The attempt has already landed and its bound has already passed.
        let init = ClientInit::landed(
            Ok(Arc::clone(&built)),
            Instant::now() - Duration::from_secs(3600),
        );
        p.state.lock().pending = Some(init);

        match p.resolve_client().await {
            Ok(c) if Arc::ptr_eq(&c, &built) => {}
            other => panic!(
                "trial {i}: resolve_client() = {other:?}; want the landed client, because a result that is already there beats a bound that has already passed"
            ),
        }
    }
}

// Two Clients left to Application Default Credentials are still two cache dimensions.
// parity: adapted: both clients come from with_adc with a fake authorized_user key instead of GOOGLE_APPLICATION_CREDENTIALS
#[tokio::test]
async fn test_adc_clients_do_not_share_a_cache_slot() {
    let (_token_srv, adc) = fake_adc().await;
    let new_adc_client = || Client::with_adc(Config::default(), &adc).expect("NewClient()");
    let (first, second) = (new_adc_client(), new_adc_client());
    assert_ne!(
        first.cache_slot, second.cache_slot,
        "two ADC-built Clients share a cache slot, so one would be served the other's credential"
    );
}

// A Client's cache slot must not repeat in another process.
#[test]
fn test_client_cache_slot_is_process_unique() {
    // 128 bits, hex-encoded; spelled out rather than derived from NONCE_BYTES.
    assert!(
        CLIENT_NONCE.len() >= 32,
        "clientNonce is {} hex characters, want at least 32 (128 bits)",
        CLIENT_NONCE.len()
    );
    assert_ne!(
        *CLIENT_NONCE,
        new_client_nonce(),
        "clientNonce is fixed; two processes constructing Clients in the same order would collide"
    );
    let c = Client::new(Config {
        http_client: Some(reqwest::Client::new().into()),
        ..Config::default()
    })
    .expect("NewClient()");
    assert!(
        c.cache_slot.contains(CLIENT_NONCE.as_str()),
        "cache slot {:?} does not carry the per-process nonce",
        c.cache_slot
    );
}

// join_fields must be injective: no two distinct field lists may encode alike.
#[test]
fn test_join_fields_is_injective() {
    fn build<'a>(
        prefix: &mut Vec<&'a str>,
        depth: usize,
        alphabet: &[&'a str],
        lists: &mut Vec<Vec<&'a str>>,
    ) {
        lists.push(prefix.clone());
        if depth == 0 {
            return;
        }
        for &f in alphabet {
            prefix.push(f);
            build(prefix, depth - 1, alphabet, lists);
            prefix.pop();
        }
    }

    // Every field list over this alphabet, at every length up to 3. The long entries keep the
    // length prefix from being a single self-punctuating digit.
    let long = "a".repeat(19);
    let long319 = format!("319{long}");
    let alphabet = [
        "",
        "a",
        ",",
        "|",
        ":",
        "0",
        "1:",
        "a,b",
        "a|b",
        "23",
        long.as_str(),
        long319.as_str(),
    ];
    let mut lists = Vec::new();
    build(&mut Vec::new(), 3, &alphabet, &mut lists);

    let mut seen: HashMap<String, Vec<&str>> = HashMap::with_capacity(lists.len());
    for l in &lists {
        let enc = join_fields(l);
        if let Some(prev) = seen.insert(enc.clone(), l.clone()) {
            panic!("join_fields({prev:?}) and join_fields({l:?}) both encode to {enc:?}");
        }
    }
    assert_eq!(seen.len(), lists.len());
}

// Client::new is called concurrently in production, and a slot handed out twice is a
// cross-principal cache hit.
#[test]
fn test_new_client_slots_are_unique_under_concurrency() {
    const N: usize = 32;
    let slots: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..N)
            .map(|_| {
                s.spawn(|| {
                    Client::new(Config {
                        http_client: Some(reqwest::Client::new().into()),
                        ..Config::default()
                    })
                    .expect("NewClient()")
                    .cache_slot
                    .clone()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("builder thread"))
            .collect()
    });

    let mut seen = HashSet::with_capacity(N);
    for s in &slots {
        assert!(!s.is_empty(), "a Client was built with an empty cache slot");
        assert!(seen.insert(s.clone()), "cache slot {s:?} was handed to two Clients");
    }
}

// TestCacheUntilBoundary pins the caching floor at a fixed clock, equality included.
// parity: adapted: Go's zero time.Time{} ("no expiry") is None, and Go's (time, ok) pair is an Option
#[test]
fn test_cache_until_boundary() {
    let now = Utc
        .with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
        .single()
        .expect("2030-01-01");
    let skew = TimeDelta::from_std(EXPIRY_SKEW).expect("ExpirySkew");
    let cap = TimeDelta::from_std(MAX_CACHED_LIFETIME).expect("maxCachedLifetime");
    let ns = TimeDelta::nanoseconds(1);
    let far = Utc
        .with_ymd_and_hms(9999, 12, 31, 23, 59, 59)
        .single()
        .expect("9999-12-31");
    #[allow(clippy::type_complexity)]
    let rows: [(&str, Option<DateTime<Utc>>, Option<DateTime<Utc>>); 8] = [
        ("no expiry", None, None),
        ("already past", Some(now - ns), None),
        ("a nanosecond inside the margin", Some(now + skew - ns), None),
        ("exactly the margin", Some(now + skew), None),
        (
            "a nanosecond beyond the margin",
            Some(now + skew + ns),
            Some(now + skew + ns),
        ),
        ("exactly the cap", Some(now + cap), Some(now + cap)),
        ("a nanosecond beyond the cap", Some(now + cap + ns), Some(now + cap)),
        ("far future", Some(far), Some(now + cap)),
    ];
    for (name, expires_at, want) in rows {
        assert_eq!(cache_until(now, expires_at), want, "{name}: cacheUntil()");
    }
}

// The sweep is the only thing that removes entries from `refreshed`: it must drop a slot whose
// cooldown has run out and keep one still inside it.
#[tokio::test(start_paused = true)]
async fn test_allow_refresh_sweeps_only_expired_slots() {
    // Move the paused clock past two cooldowns so `now - 2 * REFRESH_COOLDOWN` exists.
    tokio::time::advance(4 * REFRESH_COOLDOWN).await;
    let now = Instant::now();
    let p = new_test_provider();
    {
        let mut state = p.state.lock();
        state
            .refreshed
            .insert("expired".to_string(), now - 2 * REFRESH_COOLDOWN);
        state
            .refreshed
            .insert("live".to_string(), now - REFRESH_COOLDOWN / 2);
        state.refresh_swept = Some(now - 2 * REFRESH_COOLDOWN);
    }

    assert!(
        p.allow_refresh("new"),
        "allow_refresh() for a slot never refreshed = false, want true"
    );
    assert!(
        !p.state.lock().refreshed.contains_key("expired"),
        "a slot past its cooldown survived the sweep"
    );
    assert!(
        !p.allow_refresh("live"),
        "allow_refresh() for a slot inside its cooldown = true after a sweep, want false"
    );
}

/// Sets its flag when dropped: observes the bound dropping the store's `delete` future.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Go `hangingStore`: never answers a `delete`.
struct HangingStore {
    inner: InMemoryCredentialStore,
    delete_dropped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl CredentialStore for HangingStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, crate::Error> {
        self.inner.get(key).await
    }

    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), crate::Error> {
        self.inner.set(key, cred, expires_at).await
    }

    async fn delete(&self, _key: &CredentialKey) -> Result<(), crate::Error> {
        let _observer = SetOnDrop(Arc::clone(&self.delete_dropped));
        futures::future::pending::<()>().await;
        Ok(())
    }
}

// The eviction runs detached, so nothing else bounds it. A store that never answers must not
// hold the request that triggered the refresh.
// parity: adapted: the bound drops the store's delete future instead of cancelling its context, so the test asserts drop_entry returns within the bound and the hanging delete observed its drop, not context.DeadlineExceeded
#[tokio::test(start_paused = true)]
async fn test_drop_entry_does_not_wait_on_a_store_forever() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut p = new_test_provider();
    p.store = Arc::new(HangingStore {
        inner: InMemoryCredentialStore::new(),
        delete_dropped: Arc::clone(&dropped),
    });
    p.evict_timeout = Duration::from_millis(20);

    let finished = tokio::time::timeout(
        Duration::from_secs(5),
        p.drop_entry(&CredentialKey::default()),
    )
    .await;
    assert!(
        finished.is_ok(),
        "drop_entry did not return; the detached delete is unbounded"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "the store's delete was never released by the evict bound"
    );
}
