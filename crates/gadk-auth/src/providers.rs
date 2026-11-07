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
use gadk_agent::go_quote;
use google_cloud_auth::credentials::idtoken::{self, IDTokenCredentials};
use google_cloud_auth::credentials::service_account::{self, AccessSpecifier};
use google_cloud_auth::credentials::{AccessTokenCredentials, Builder as AdcBuilder, user_account};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::credential::{ApiKey, Bearer, Credential, OAuth2, Token, TokenSource};
use crate::error::{Error, SharedError};
use crate::provider::{AuthContext, CredentialFuture, CredentialProvider, ProviderFn};

/// Go `StaticToken` (providers.go:125): a fresh `Bearer { token }` per call.
pub fn static_token(token: impl Into<String>) -> Arc<dyn CredentialProvider> {
    let token = token.into();
    ProviderFn::new(move |_: &AuthContext| -> CredentialFuture {
        resolved(Arc::new(Bearer {
            token: token.clone(),
        }))
    })
    .into_provider()
}

/// Go `APIKey` (providers.go:133): a header API key credential, where `name` is the header (for
/// example `X-Api-Key`).
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

/// Go `TokenSourceProvider` (providers.go:142): `OAuth2 { token_source }` per call, so a fresh
/// (self-refreshing) token is minted at apply time. This covers client-credentials (2-legged)
/// OAuth. Go's `auth: nil token source` cannot occur: `ts` is a non-optional `Arc`.
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

/// Go `ADC(scopes...)` (providers.go:168): Google Application Default Credentials, discovered
/// lazily on the first `credential` call and reused thereafter. With no scopes the
/// cloud-platform scope is used.
pub fn adc(scopes: &[&str]) -> Arc<dyn CredentialProvider> {
    adc_with(
        AdcSource::Discover,
        scopes.iter().map(|s| (*s).to_owned()).collect(),
    )
}

/// Go `ServiceAccount` (providers.go:193): mints tokens for a service account, from an explicit
/// key or from ADC. The token source is built lazily on the first `credential` call and reused
/// thereafter:
/// - `audience` set: a Google-signed ID token for it (from `json_key` when given, else ADC);
/// - `json_key` set: an access token for `scopes`, which are required;
/// - neither: an ADC access token for `scopes` (cloud-platform when empty).
pub fn service_account(cfg: ServiceAccountConfig) -> Arc<dyn CredentialProvider> {
    service_account_with(cfg, AdcSource::Discover)
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

/// providers.go:152 `scopeCloudPlatform`: the default ADC scope, as in adk-python and gcloud.
pub(crate) const SCOPE_CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where ADC is read from. `Discover` is google-cloud-auth's real lookup. `Json` stands in for a
/// `GOOGLE_APPLICATION_CREDENTIALS` file, because tests cannot set the variable
/// (`env::set_var` is `unsafe`). Supported `"type"`s: `authorized_user`, `service_account`.
// `Json` is built only by tests and the gcp client's ADC seam, so a non-test build without
// those sees it as never constructed.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum AdcSource {
    Discover,
    Json(Value),
}

/// Go `adcTokenSource` (providers.go:154): an access-token source for `scopes` (cloud-platform
/// when empty). Builder errors are exactly `Error::FindDefaultCredentials(_)`. Synchronous, but
/// must run inside a Tokio runtime: the built credentials spawn their token-refresh task.
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
        AdcSource::Json(json) => match credential_type(json) {
            "authorized_user" => user_account::Builder::new(json.clone())
                .with_scopes(scopes)
                .build_access_token_credentials()
                .map_err(SharedError::new),
            "service_account" => service_account::Builder::new(json.clone())
                .with_access_specifier(AccessSpecifier::from_scopes(scopes))
                .build_access_token_credentials()
                .map_err(SharedError::new),
            // golang.org/x/oauth2/google/google.go:244.
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

/// The init of Go `ServiceAccount` (providers.go:194-222).
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

/// Go `idtoken.NewTokenSource(ctx, cfg.Audience, opts...)` (providers.go:196-204): from the key
/// when one is given (Go `option.WithAuthCredentialsJSON(option.ServiceAccount, key)`), else
/// from ADC.
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
            AdcSource::Json(json) => match credential_type(json) {
                "service_account" => {
                    idtoken::service_account::Builder::new(audience, json.clone()).build()
                }
                // google.golang.org/api/idtoken/idtoken.go `unsupported credentials type`.
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

/// Go `google.CredentialsFromJSONWithType(…, google.ServiceAccount, …)`'s parse and type check
/// (golang.org/x/oauth2/google/default.go:310-316).
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

/// The `"type"` of a credentials JSON file; `""` when absent (Go's zero `credentialsFile.Type`).
fn credential_type(json: &Value) -> &str {
    json.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// Go `creds.TokenSource`: an access-token source over google-cloud-auth credentials, which
/// cache and refresh the token themselves.
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

/// Go `idtoken.NewTokenSource`'s source: the ID token travels as a bearer token.
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

// ---- Lazy token source (providers.go:226-291) ----

/// providers.go:227 `initTimeout`: bounds a hung init so it fails and is retried, not wedged
/// forever. A parameter of [`lazy_token_source`] in Rust, so tests pass a short bound.
pub(crate) const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a lazy init resolves to.
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
