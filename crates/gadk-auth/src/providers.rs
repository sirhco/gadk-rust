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

//! The built-in providers and the lazy token source.
//!
//! [`static_token`], [`api_key`] and [`token_source`] resolve at once. [`adc`] and
//! [`service_account()`] build their token source lazily, on the first `credential` call: one
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
use gadk_agent::go_quote;
use google_cloud_auth::credentials::idtoken::{self, IDTokenCredentials};
use google_cloud_auth::credentials::service_account::{self, AccessSpecifier};
#[cfg(test)]
use google_cloud_auth::credentials::user_account;
use google_cloud_auth::credentials::{AccessTokenCredentials, Builder as AdcBuilder};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::credential::{ApiKey, Bearer, Credential, OAuth2, Token, TokenSource};
use crate::error::{Error, SharedError};
use crate::provider::{AuthContext, CredentialFuture, CredentialProvider, ProviderFn};

/// A provider returning a fresh `Bearer { token }` per call.
pub fn static_token(token: impl Into<String>) -> Arc<dyn CredentialProvider> {
    let token = token.into();
    ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
        resolved(Arc::new(Bearer {
            token: token.clone(),
        }))
    })
    .into_provider()
}

/// A header API key provider, where `name` is the header (for example `X-Api-Key`).
pub fn api_key(name: impl Into<String>, value: impl Into<String>) -> Arc<dyn CredentialProvider> {
    let (name, value) = (name.into(), value.into());
    ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
        resolved(Arc::new(ApiKey {
            name: name.clone(),
            value: value.clone(),
        }))
    })
    .into_provider()
}

/// A provider returning `OAuth2 { token_source }` per call, so a fresh (self-refreshing) token is
/// minted at apply time. This covers client-credentials (2-legged) OAuth.
pub fn token_source(ts: Arc<dyn TokenSource>) -> Arc<dyn CredentialProvider> {
    ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
        resolved(Arc::new(OAuth2 {
            token_source: Arc::clone(&ts),
        }))
    })
    .into_provider()
}

/// An already-resolved credential as a [`CredentialFuture`].
fn resolved(cred: Arc<dyn Credential>) -> CredentialFuture {
    std::future::ready(Ok(cred)).boxed()
}

/// Google Application Default Credentials, discovered lazily on the first `credential` call and
/// reused thereafter. With no scopes the cloud-platform scope is used.
pub fn adc(scopes: &[&str]) -> Arc<dyn CredentialProvider> {
    adc_with(
        AdcSource::Discover,
        scopes.iter().map(|s| (*s).to_owned()).collect(),
    )
}

/// Mints tokens for a service account, from an explicit key or from ADC. The token source is built
/// lazily on the first `credential` call and reused thereafter:
/// - `audience` set: a Google-signed ID token for it (from `json_key` when given, else ADC);
/// - `json_key` set: an access token for `scopes`, which are required;
/// - neither: an ADC access token for `scopes` (cloud-platform when empty).
pub fn service_account(cfg: ServiceAccountConfig) -> Arc<dyn CredentialProvider> {
    service_account_with(cfg, AdcSource::Discover)
}

/// The configuration of [`service_account()`]. An empty `json_key` means no explicit key.
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
    // The key is a secret: it prints as "<redacted>" when set.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let json_key = if self.json_key.is_empty() {
            ""
        } else {
            "<redacted>"
        };
        f.debug_struct("ServiceAccountConfig")
            .field("json_key", &json_key)
            .field("scopes", &self.scopes)
            .field("audience", &self.audience)
            .finish()
    }
}

/// The default ADC scope, as in adk-python and gcloud.
pub(crate) const SCOPE_CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where ADC is read from. `Discover` is google-cloud-auth's real lookup. `Json` stands in for a
/// `GOOGLE_APPLICATION_CREDENTIALS` file, because tests cannot set the variable
/// (`env::set_var` is `unsafe`). Supported `"type"`s: `authorized_user`, `service_account`.
#[derive(Clone, Debug)]
pub(crate) enum AdcSource {
    Discover,
    #[cfg(test)]
    Json(Value),
}

/// An access-token source over Application Default Credentials for `scopes` (cloud-platform when
/// empty). Builder errors are exactly `Error::FindDefaultCredentials(_)`. Synchronous, but must run
/// inside a Tokio runtime: the built credentials spawn their token-refresh task.
pub(crate) fn adc_token_source(
    source: &AdcSource,
    scopes: &[String],
) -> Result<Arc<dyn TokenSource>, Error> {
    let scopes = if scopes.is_empty() {
        vec![SCOPE_CLOUD_PLATFORM.to_owned()]
    } else {
        scopes.to_vec()
    };
    let built = match source {
        AdcSource::Discover => AdcBuilder::default()
            .with_scopes(scopes)
            .build_access_token_credentials()
            .map_err(SharedError::new),
        #[cfg(test)]
        AdcSource::Json(json) => match credential_type(json) {
            "authorized_user" => user_account::Builder::new(json.clone())
                .with_scopes(scopes)
                .build_access_token_credentials()
                .map_err(SharedError::new),
            "service_account" => service_account::Builder::new(json.clone())
                .with_access_specifier(AccessSpecifier::from_scopes(scopes))
                .build_access_token_credentials()
                .map_err(SharedError::new),
            // The injected source's error for an unrecognised credential type.
            other => Err(SharedError::from(format!(
                "unknown credential type: {}",
                go_quote(other)
            ))),
        },
    };
    let creds = built.map_err(Error::FindDefaultCredentials)?;
    Ok(Arc::new(AccessTokenSource(creds)))
}

/// `adc` with an injected source.
pub(crate) fn adc_with(source: AdcSource, scopes: Vec<String>) -> Arc<dyn CredentialProvider> {
    // google-cloud-auth's builders take no context and their token caches refresh on their own
    // tasks, so the init token has nothing to reach.
    lazy_token_source(
        move |_cancel: CancellationToken| -> InitFuture {
            std::future::ready(adc_token_source(&source, &scopes)).boxed()
        },
        INIT_TIMEOUT,
    )
}

/// `service_account` with an injected ADC source for its fallback branch.
pub(crate) fn service_account_with(
    cfg: ServiceAccountConfig,
    source: AdcSource,
) -> Arc<dyn CredentialProvider> {
    lazy_token_source(
        move |_cancel: CancellationToken| -> InitFuture {
            std::future::ready(service_account_token_source(&cfg, &source)).boxed()
        },
        INIT_TIMEOUT,
    )
}

/// The init of [`service_account`]: builds the token source its configuration asks for.
fn service_account_token_source(
    cfg: &ServiceAccountConfig,
    source: &AdcSource,
) -> Result<Arc<dyn TokenSource>, Error> {
    if !cfg.audience.is_empty() {
        return id_token_source(cfg, source).map_err(Error::IdTokenSource);
    }
    if !cfg.json_key.is_empty() {
        // An explicit-key access token is scope-bound, so no scopes means unusable. This runs on
        // the first `credential` call, not in `service_account`, which cannot fail. adk-python
        // rejects the same case at the same point (service_account_exchanger.py).
        if cfg.scopes.is_empty() {
            return Err(Error::ServiceAccountScopesRequired);
        }
        let creds = service_account_key(&cfg.json_key)
            .and_then(|key| {
                service_account::Builder::new(key)
                    .with_access_specifier(AccessSpecifier::from_scopes(cfg.scopes.clone()))
                    .build_access_token_credentials()
                    .map_err(SharedError::new)
            })
            .map_err(Error::CredentialsFromJson)?;
        return Ok(Arc::new(AccessTokenSource(creds)));
    }
    adc_token_source(source, &cfg.scopes)
}

/// An ID-token source for `cfg.audience`: from the service-account key when one is given, else from
/// ADC.
fn id_token_source(
    cfg: &ServiceAccountConfig,
    source: &AdcSource,
) -> Result<Arc<dyn TokenSource>, SharedError> {
    let audience = cfg.audience.clone();
    let built = if !cfg.json_key.is_empty() {
        let key = service_account_key(&cfg.json_key)?;
        idtoken::service_account::Builder::new(audience, key).build()
    } else {
        match source {
            AdcSource::Discover => idtoken::Builder::new(audience).build(),
            #[cfg(test)]
            AdcSource::Json(json) => match credential_type(json) {
                "service_account" => {
                    idtoken::service_account::Builder::new(audience, json.clone()).build()
                }
                // The injected source's error for an unsupported credentials type.
                other => {
                    return Err(SharedError::from(format!(
                        "idtoken: unsupported credentials type: {}",
                        go_quote(other)
                    )));
                }
            },
        }
    };
    let creds = built.map_err(SharedError::new)?;
    Ok(Arc::new(IdTokenSource(creds)))
}

/// Parses a service-account key: valid JSON whose `"type"` is `service_account`.
fn service_account_key(json_key: &[u8]) -> Result<Value, SharedError> {
    let key: Value = serde_json::from_slice(json_key).map_err(SharedError::new)?;
    let found = credential_type(&key);
    if found != "service_account" {
        return Err(SharedError::from(format!(
            "google: expected credential type {}, found {}",
            go_quote("service_account"),
            go_quote(found)
        )));
    }
    Ok(key)
}

/// The `"type"` of a credentials JSON file; `""` when absent.
fn credential_type(json: &Value) -> &str {
    json.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// An access-token source over google-cloud-auth credentials, which cache and refresh the token
/// themselves.
struct AccessTokenSource(AccessTokenCredentials);

#[async_trait::async_trait]
impl TokenSource for AccessTokenSource {
    async fn token(&self) -> Result<Token, Error> {
        let token = self.0.access_token().await.map_err(Error::other)?;
        Ok(Token {
            access_token: token.token,
            token_type: "Bearer".to_owned(),
        })
    }
}

/// An ID-token source: the ID token travels as a bearer token.
struct IdTokenSource(IDTokenCredentials);

#[async_trait::async_trait]
impl TokenSource for IdTokenSource {
    async fn token(&self) -> Result<Token, Error> {
        let id_token = self.0.id_token().await.map_err(Error::other)?;
        Ok(Token {
            access_token: id_token,
            token_type: "Bearer".to_owned(),
        })
    }
}

// ---- Lazy token source ----

/// Bounds a hung init so it fails and is retried, not wedged forever. A parameter of
/// [`lazy_token_source`] in Rust, so tests pass a short bound.
pub(crate) const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a lazy init resolves to.
pub(crate) type InitFuture = BoxFuture<'static, Result<Arc<dyn TokenSource>, Error>>;

/// One init attempt. Every waiter awaits its own clone and gets a clone of the result. Dropping a
/// clone gives up that waiter only: the attempt runs on its own task.
type Attempt = Shared<BoxFuture<'static, Result<Arc<dyn TokenSource>, Error>>>;

/// The lazy source's shared state: the memoized source and the in-flight attempt.
#[derive(Default)]
struct LazyState {
    /// The source of the first attempt that succeeded within its bound.
    ts: Option<Arc<dyn TokenSource>>,
    /// The in-flight attempt, for single flight. The attempt clears it when it ends; one whose task
    /// was dropped unfinished is marked abandoned and replaced by the next call.
    cur: Option<(Attempt, AbandonGuard)>,
}

/// Owned by an attempt's task. Dropped before it is disarmed (the runtime shut down with the
/// attempt pending), it marks the attempt abandoned, so the next call starts a fresh one rather
/// than waiting forever. A flag rather than a lock, because tokio may drop a task's future inside
/// `spawn`, while the caller holds the state lock.
#[derive(Clone, Default)]
struct AbandonGuard(Arc<std::sync::atomic::AtomicBool>);

impl AbandonGuard {
    fn abandoned(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// [`AbandonGuard`]'s task-side half: sets the flag on drop unless [`Self::disarm`]ed.
struct AbandonOnDrop(Option<AbandonGuard>);

impl AbandonOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        if let Some(guard) = self.0.take() {
            guard.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// The provider over a lazily initialised token source.
struct LazyTokenSource<F> {
    init: Arc<F>,
    bound: Duration,
    state: Arc<Mutex<LazyState>>,
}

/// A provider over a lazily initialised token source. One shared attempt, `tokio::spawn`ed. The
/// bound is a timer inside the attempt: when it fires the token is cancelled, and the attempt's
/// result becomes `Error::InitTimeout { bound, init_error }` (the init's text when it failed); a
/// source landing past the bound is discarded. A failure resets the slot. Each success is an
/// `OAuth2 { token_source }` over the memoized source.
///
/// Waiters are cancelled only by dropping their own future; that never stops the attempt, whose
/// result is still memoized for the next call. The token handed to `init` is cancelled only when
/// the bound fires, never because init returned, since a token source may keep using it.
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
    /// Starts the attempt on its own task, so no waiter giving up can stop it.
    fn spawn_attempt(&self) -> (Attempt, AbandonGuard) {
        let guard = AbandonGuard::default();
        let on_drop = AbandonOnDrop(Some(guard.clone()));
        let attempt = run_attempt(Arc::clone(&self.init), Arc::clone(&self.state), self.bound);
        let task = tokio::spawn(async move {
            let result = attempt.await;
            // run_attempt has cleared `cur` itself.
            on_drop.disarm();
            result
        });
        let attempt = async move {
            task.await.unwrap_or_else(|join| {
                // run_attempt catches init's panics, so only a runtime shutting down gets here.
                Err(Error::msg(format!(
                    "auth: credential initialization did not finish: {join}"
                )))
            })
        }
        .boxed()
        .shared();
        (attempt, guard)
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
            if st.cur.as_ref().is_some_and(|(_, guard)| guard.abandoned()) {
                st.cur = None;
            }
            st.cur.get_or_insert_with(|| self.spawn_attempt()).0.clone()
        };
        // A caller that gives up drops this future; the attempt keeps running.
        attempt.await.map(oauth2)
    }
}

/// An [`OAuth2`] credential over `token_source`.
fn oauth2(token_source: Arc<dyn TokenSource>) -> Arc<dyn Credential> {
    Arc::new(OAuth2 { token_source })
}

/// One attempt's body: runs `init` under the bound and records the outcome.
async fn run_attempt<F>(
    init: Arc<F>,
    state: Arc<Mutex<LazyState>>,
    bound: Duration,
) -> Result<Arc<dyn TokenSource>, Error>
where
    F: Fn(CancellationToken) -> InitFuture + Send + Sync + 'static,
{
    // The token source may refresh on this token, so it must outlive init. A timer bounds init
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
                // Init keeps running and returns when it sees the cancellation.
                fired = true;
                cancel.cancel();
            }
        }
    };
    let mut result = outcome
        .unwrap_or_else(|_panic| Err(Error::msg("auth: credential initialization panicked")));
    if fired {
        // The token is cancelled, so a source init returned is unusable. The init's error is kept
        // as text, not as a source: it may report the cancellation, and the caller must see a
        // timeout.
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

    /// A current-thread runtime; `paused` starts its clock paused, so a timer fires at once.
    fn runtime(paused: bool) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(paused)
            .build()
            .unwrap()
    }

    #[test]
    fn an_attempt_dropped_with_its_runtime_is_retried_not_wedged() {
        let calls = Arc::new(AtomicUsize::new(0));
        let p = lazy_token_source(
            {
                let calls = Arc::clone(&calls);
                move |_cancel: CancellationToken| -> InitFuture {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        std::future::pending().boxed()
                    } else {
                        ready(Ok(tok()))
                    }
                }
            },
            INIT_TIMEOUT,
        );
        let first = runtime(false);
        let cx = AuthContext::anonymous();
        first.block_on(async {
            let mut call = p.credential(&cx);
            assert!(call.as_mut().now_or_never().is_none());
            // Let the attempt's task start init before the runtime goes away.
            for _ in 0..100 {
                if calls.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
        // The runtime shuts down with the attempt still pending (a short-lived runtime, as in a
        // process-global provider used from several of them).
        drop(first);
        let second = runtime(true);
        let r = second.block_on(p.credential(&cx));
        assert!(r.is_ok(), "a fresh attempt should run, got {r:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod builtin_tests {
    use serde_json::json;

    use super::*;

    /// The error a provider's first `credential` call returns.
    async fn credential_err(p: Arc<dyn CredentialProvider>) -> Error {
        match p.credential(&AuthContext::anonymous()).await {
            Ok(cred) => panic!("credential() = {cred:?}, want an error"),
            Err(e) => e,
        }
    }

    #[test]
    fn service_account_config_debug_redacts_the_key() {
        let cfg = ServiceAccountConfig {
            json_key: br#"{"private_key":"s3cr3t"}"#.to_vec(),
            scopes: vec!["s".into()],
            audience: "aud".into(),
        };
        assert_eq!(
            format!("{cfg:?}"),
            r#"ServiceAccountConfig { json_key: "<redacted>", scopes: ["s"], audience: "aud" }"#
        );
        assert_eq!(
            format!("{:?}", ServiceAccountConfig::default()),
            r#"ServiceAccountConfig { json_key: "", scopes: [], audience: "" }"#
        );
    }

    #[tokio::test]
    async fn static_token_resolves_a_fresh_credential_per_call() {
        let p = static_token("abc");
        let a = p.credential(&AuthContext::anonymous()).await.unwrap();
        let b = p.credential(&AuthContext::anonymous()).await.unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(
            a.downcast_ref::<Bearer>(),
            Some(&Bearer {
                token: "abc".into()
            })
        );
    }

    #[test]
    fn adc_json_rejects_an_unknown_credential_type() {
        let Err(err) = adc_token_source(&AdcSource::Json(json!({"type": "external_x"})), &[])
        else {
            panic!("adc_token_source() succeeded for an unknown type");
        };
        assert!(matches!(err, Error::FindDefaultCredentials(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            r#"auth: find default credentials: unknown credential type: "external_x""#
        );
    }

    #[tokio::test]
    async fn service_account_key_must_be_a_service_account() {
        let err = credential_err(service_account(ServiceAccountConfig {
            json_key: br#"{"type":"authorized_user"}"#.to_vec(),
            scopes: vec![SCOPE_CLOUD_PLATFORM.into()],
            ..Default::default()
        }))
        .await;
        assert_eq!(
            err.to_string(),
            r#"auth: credentials from json: google: expected credential type "service_account", found "authorized_user""#
        );
    }

    #[tokio::test]
    async fn id_token_needs_a_service_account() {
        // From ADC.
        let err = credential_err(service_account_with(
            ServiceAccountConfig {
                audience: "https://run.example".into(),
                ..Default::default()
            },
            AdcSource::Json(json!({"type": "authorized_user"})),
        ))
        .await;
        assert!(matches!(err, Error::IdTokenSource(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            r#"auth: id token source: idtoken: unsupported credentials type: "authorized_user""#
        );
        // From an explicit key that is not JSON.
        let err = credential_err(service_account(ServiceAccountConfig {
            json_key: b"not-json".to_vec(),
            audience: "https://run.example".into(),
            ..Default::default()
        }))
        .await;
        assert!(matches!(err, Error::IdTokenSource(_)), "{err:?}");
        assert!(
            err.to_string().starts_with("auth: id token source: "),
            "{err}"
        );
    }
}
