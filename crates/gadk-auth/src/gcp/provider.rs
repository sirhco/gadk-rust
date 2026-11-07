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

//! The per-user credential provider (port of ADK-Go `auth/gcp/provider.go`).
//!
//! [`new_provider`] returns a [`CredentialProvider`] that resolves the acting end user's
//! credential for one [`ProviderScheme`] through the Agent Identity / IAM Connector services,
//! caches it in a [`CredentialStore`] and force-refreshes it when a downstream rejects it.
//!
//! The acting user is read from [`AuthContext::identity`] at resolve time, so the provider must
//! run within an agent invocation. ADK does not authenticate the session's user id, and it now
//! decides whose credential is minted: wiring this up means trusting the embedding server.
//!
//! The HTTP client carrying the authenticated requests must not follow a cross-host redirect
//! (build it with `reqwest::redirect::Policy::none()` under [`crate::client`]); the ADC client
//! [`Client::new`] builds for itself refuses redirects too.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::credential::Credential;
use crate::go_fmt::{go_duration, hex_encode};
use crate::provider::{AuthContext, CredentialProvider, RefreshingProvider};
use crate::providers::AdcSource;
use crate::store::{CredentialKey, CredentialStore, InMemoryCredentialStore};

use super::client::{
    AUTH_PROVIDER_RESOURCE_RE, CONNECTOR_RESOURCE_RE, Client, Config, join_fields,
    validate_resource,
};
use super::{Error, ErrorKind};

/// Go `ProviderScheme` (provider.go:33): a GCP auth resource and the access it requests
/// (adk-python's `GcpAuthProviderScheme`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderScheme {
    /// The full resource name: `projects/*/locations/*/connectors/*` (IAM Connector) or
    /// `projects/*/locations/*/authProviders/*` (Agent Identity). [`new_provider`] accepts only
    /// those two shapes, which is stricter than [`Client::retrieve_credential`].
    pub name: String,
    /// The OAuth scopes requested for the credential.
    pub scopes: Vec<String>,
    /// The developer-hosted URI that finalizes managed-OAuth (3-legged) flows.
    pub continue_uri: String,
}

/// Go `ProviderConfig` (provider.go:51). `client: None` builds the ADC client lazily on first
/// use; `store: None` gives the provider a private `InMemoryCredentialStore`.
///
/// One store can back several providers: entries are keyed by the app, the end user, and
/// everything that decides what the service returns (the resource, the scopes, the continue
/// URI and the `Client`). A `Client` is identified per process, so a store shared across
/// processes never serves one process's entry to another. A cached credential lives until the
/// service's expiry or an hour, whichever comes first; to invalidate one sooner, pass
/// [`Client::cache_key`] to [`CredentialStore::delete`] (which needs a `Client` set here).
#[derive(Clone, Default)]
pub struct ProviderConfig {
    /// The resource to mint credentials for. Required.
    pub scheme: ProviderScheme,
    /// The client to retrieve with; `None` builds the ADC client lazily, bounded at 30s.
    pub client: Option<Arc<Client>>,
    /// Where retrieved credentials are cached; `None` is a private in-memory store.
    pub store: Option<Arc<dyn CredentialStore>>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("scheme", &self.scheme)
            .field("client", &self.client)
            .field("store", &self.store.as_ref().map(|_| "dyn CredentialStore"))
            .finish()
    }
}

/// Go `defaultInitTimeout` (provider.go:135): how long the waiters on one default-client
/// attempt wait. The build itself cannot be cancelled, so the bound is on the waiting side.
pub(crate) const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Go `defaultEvictTimeout` (provider.go:141): the bound on a detached store delete.
pub(crate) const DEFAULT_EVICT_TIMEOUT: Duration = Duration::from_secs(5);
/// Go `maxCachedLifetime` (provider.go:229): the longest a credential is cached, whatever expiry
/// the service reports.
pub(crate) const MAX_CACHED_LIFETIME: Duration = Duration::from_secs(3600);
/// Go `refreshCooldown` (provider.go:486): the shortest interval between two forced refreshes
/// of one credential at the service.
pub(crate) const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);

/// Go `provider.newClient` (no context argument): builds the default client.
pub(crate) type NewClientFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Arc<Client>, Error>> + Send + Sync>;

/// The default builder: `Client::with_adc(Config::default(), &adc)` on a blocking thread,
/// because ADC discovery reads files synchronously. A panic inside it is re-raised on the
/// calling task, where `spawn_init` reports it as the attempt's failure.
pub(crate) fn default_new_client(adc: AdcSource) -> NewClientFn {
    Arc::new(move || {
        let adc = adc.clone();
        async move {
            match tokio::task::spawn_blocking(move || Client::with_adc(Config::default(), &adc))
                .await
            {
                Ok(built) => built.map(Arc::new),
                Err(join) => match join.try_into_panic() {
                    Ok(payload) => std::panic::resume_unwind(payload),
                    Err(_) => Err(Error::new("building it did not complete")),
                },
            }
        }
        .boxed()
    })
}

/// Go `provider` (provider.go:233). Tests build one with `build_provider`, set fields, then
/// wrap it in `Arc`.
pub(crate) struct Provider {
    pub(crate) scheme: ProviderScheme,
    pub(crate) store: Arc<dyn CredentialStore>,
    /// Go `newClient`, `initTimeout` and `evictTimeout` are fields so tests can drive the
    /// failure and hang paths a real ADC lookup and a real store cannot be made to hit.
    pub(crate) new_client: NewClientFn,
    pub(crate) init_timeout: Duration,
    pub(crate) evict_timeout: Duration,
    /// The wall clock `cache_until` reads (provider.go:640, `time.Now()`).
    pub(crate) clock: Arc<dyn gadk_platform::TimeProvider>,
    /// Shared with the spawned init task.
    pub(crate) state: Arc<Mutex<ProviderState>>,
}

/// Go `provider`'s mutex-guarded fields.
pub(crate) struct ProviderState {
    /// The configured or published client.
    pub(crate) client: Option<Arc<Client>>,
    /// The in-flight lazy init, shared by concurrent callers.
    pub(crate) pending: Option<Arc<ClientInit>>,
    /// When each refresh slot was last force-refreshed, for the cooldown.
    pub(crate) refreshed: HashMap<String, Instant>,
    /// When `refreshed` was last swept; `None` until the first forced refresh (Go's zero time).
    pub(crate) refresh_swept: Option<Instant>,
}

/// Go `clientInit` (provider.go:262): one attempt at building the default client. The
/// deadline is the attempt's, shared by every waiter, so a caller arriving after it has passed
/// fails at once.
pub(crate) struct ClientInit {
    pub(crate) deadline: Instant,
    /// `None` until the attempt lands; then its result, sent exactly once.
    outcome: watch::Sender<Option<Result<Arc<Client>, Error>>>,
}

impl ClientInit {
    pub(crate) fn new(deadline: Instant) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            outcome: watch::channel(None).0,
        })
    }

    /// An attempt that already landed (TestResolveClientPrefersALandedResultOverAnExpiredBound).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn landed(result: Result<Arc<Client>, Error>, deadline: Instant) -> Arc<Self> {
        Arc::new(Self {
            deadline,
            outcome: watch::channel(Some(result)).0,
        })
    }

    /// Go `in.result()` after `<-in.done`: `None` while the attempt is still running.
    fn result(&self) -> Option<Result<Arc<Client>, Error>> {
        self.outcome.borrow().clone()
    }

    /// Waits for the attempt to land. `wait_for` checks the current value first.
    async fn landed_result(&self) -> Option<Result<Arc<Client>, Error>> {
        let mut rx = self.outcome.subscribe();
        let landed = rx.wait_for(Option::is_some).await.is_ok();
        if landed { self.result() } else { None }
    }

    fn publish(&self, result: Result<Arc<Client>, Error>) {
        self.outcome.send_replace(Some(result));
    }
}

/// `new_provider` minus the `Arc` (Go `NewProvider`, provider.go:181).
pub(crate) fn build_provider(cfg: ProviderConfig) -> Result<Provider, Error> {
    let name = cfg.scheme.name.as_str();
    if name.is_empty() {
        return Err(Error::new("gcp: NewProvider requires a scheme Name"));
    }
    // A malformed name is a wiring mistake, so catch it here rather than on every request from
    // inside a transport.
    validate_resource(name).map_err(|e| e.prefixed("gcp: NewProvider"))?;
    if !CONNECTOR_RESOURCE_RE.is_match(name) && !AUTH_PROVIDER_RESOURCE_RE.is_match(name) {
        return Err(Error::new(format!(
            "gcp: NewProvider: scheme Name {} is neither projects/*/locations/*/connectors/* nor projects/*/locations/*/authProviders/*",
            gadk_agent::go_quote(name)
        )));
    }
    let store = cfg
        .store
        .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new()));
    Ok(Provider {
        // Owned: the caller cannot mutate the scopes afterwards (Go's slices.Clone).
        scheme: cfg.scheme,
        store,
        new_client: default_new_client(AdcSource::Discover),
        init_timeout: DEFAULT_INIT_TIMEOUT,
        evict_timeout: DEFAULT_EVICT_TIMEOUT,
        clock: Arc::new(Utc::now),
        state: Arc::new(Mutex::new(ProviderState {
            client: cfg.client,
            pending: None,
            refreshed: HashMap::new(),
            refresh_swept: None,
        })),
    })
}

/// Go `NewProvider` (provider.go:181) without the wiring context. Errors: "gcp: NewProvider
/// requires a scheme Name"; "gcp: NewProvider: <validate_resource error>"; "gcp: NewProvider:
/// scheme Name %q is neither projects/*/locations/*/connectors/* nor
/// projects/*/locations/*/authProviders/*". The provider's `as_refreshing()` is `Some`.
///
/// Building the default client (when `cfg.client` is `None`) happens on first use, not here.
pub fn new_provider(cfg: ProviderConfig) -> Result<Arc<dyn CredentialProvider>, Error> {
    build_provider(cfg).map(|p| Arc::new(p) as Arc<dyn CredentialProvider>)
}

/// Go `cacheUntil` (provider.go:658): the instant up to which a credential expiring at
/// `expires_at` may be cached as of `now`, or `None` when it may not be cached at all (no
/// expiry, or no more than `EXPIRY_SKEW` left: the store would refuse to serve it). The far end
/// is clamped to `MAX_CACHED_LIFETIME`, not rejected.
pub(crate) fn cache_until(
    now: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let expires_at = expires_at?;
    if crate::store::expired(now, expires_at) {
        return None;
    }
    match TimeDelta::from_std(MAX_CACHED_LIFETIME)
        .ok()
        .and_then(|cap| now.checked_add_signed(cap))
    {
        Some(capped) if expires_at > capped => Some(capped),
        _ => Some(expires_at),
    }
}

/// Go `refreshSlot` (provider.go:476): what a forced refresh acts on at the service. Coarser
/// than the cache key on purpose: the app name and the `Client` are cache dimensions the
/// service never sees, so counting them would multiply the cooldown's bound.
pub(crate) fn refresh_slot(user_id: &str, scheme: &ProviderScheme) -> String {
    let mut scopes: Vec<&str> = scheme.scopes.iter().map(String::as_str).collect();
    scopes.sort_unstable();
    let count = scopes.len().to_string();
    let mut fields = vec![user_id, scheme.name.as_str(), count.as_str()];
    fields.extend(scopes);
    fields.push(scheme.continue_uri.as_str());
    hex_encode(Sha256::digest(join_fields(&fields).as_bytes()).as_slice())
}

impl Provider {
    /// Go `resolveClient` (provider.go:672): the configured client, or the default one built
    /// on first use. Concurrent callers share one attempt and each waits on that attempt's
    /// remaining bound; a caller with a deadline of its own wraps this in `tokio::time::timeout`.
    /// A failed attempt is not cached, so the next call retries; a hung one is kept, and its
    /// client is published the moment it lands.
    pub(crate) async fn resolve_client(&self) -> Result<Arc<Client>, Error> {
        let init = {
            let mut state = self.state.lock();
            if let Some(client) = &state.client {
                return Ok(Arc::clone(client));
            }
            let existing = state.pending.clone();
            match existing {
                Some(init) => init,
                None => {
                    let init = ClientInit::new(Instant::now() + self.init_timeout);
                    state.pending = Some(Arc::clone(&init));
                    self.spawn_init(Arc::clone(&init));
                    init
                }
            }
        };

        // A result that has already landed wins over an expired bound.
        if let Some(result) = init.result() {
            return result;
        }
        tokio::select! {
            biased;
            landed = init.landed_result() => {
                landed.unwrap_or_else(|| Err(self.attempt_exceeded()))
            }
            () = tokio::time::sleep_until(init.deadline) => {
                // The bound and the result can be ready at once; the result wins.
                match init.result() {
                    Some(result) => result,
                    None => Err(self.attempt_exceeded()),
                }
            }
        }
    }

    /// provider.go:718.
    fn attempt_exceeded(&self) -> Error {
        Error::with_kind(
            ErrorKind::ClientUnavailable,
            format!(
                "{}: the attempt exceeded {} and is still running",
                ErrorKind::ClientUnavailable,
                go_duration(self.init_timeout)
            ),
        )
    }

    /// Go `runInit` (provider.go:733) on a task the provider owns. The builder runs on an inner
    /// task, so a panic in it (even one raised before its future is first polled) surfaces as a
    /// `JoinError` and becomes this attempt's failure instead of a wedged slot.
    fn spawn_init(&self, init: Arc<ClientInit>) {
        let new_client = Arc::clone(&self.new_client);
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let built = tokio::spawn(async move { (*new_client)().await }).await;
            let unavailable = ErrorKind::ClientUnavailable;
            let result = match built {
                Ok(Ok(client)) => Ok(client),
                Ok(Err(err)) => {
                    Err(Error::with_kind(unavailable, format!("{unavailable}: {err}")).caused_by(err))
                }
                Err(join) => match join.try_into_panic() {
                    Ok(payload) => Err(Error::with_kind(
                        unavailable,
                        format!("{unavailable}: building it panicked: {}", panic_text(&*payload)),
                    )),
                    Err(_) => Err(Error::with_kind(
                        unavailable,
                        format!("{unavailable}: building it did not complete"),
                    )),
                },
            };
            publish(&state, &init, result);
        });
    }

    /// Go `allowRefresh` (provider.go:502): whether `slot` may be force-refreshed now,
    /// recording the attempt when it may (whether or not the fetch then succeeds). The map is
    /// swept at most once per cooldown.
    pub(crate) fn allow_refresh(&self, slot: &str) -> bool {
        let now = Instant::now();
        let mut state = self.state.lock();
        if let Some(at) = state.refreshed.get(slot)
            && now.duration_since(*at) < REFRESH_COOLDOWN
        {
            return false;
        }
        if let Some(swept) = state.refresh_swept
            && now.duration_since(swept) >= REFRESH_COOLDOWN
        {
            state
                .refreshed
                .retain(|_, at| now.duration_since(*at) < REFRESH_COOLDOWN);
            state.refresh_swept = Some(now);
        }
        if state.refresh_swept.is_none() {
            state.refresh_swept = Some(now);
        }
        state.refreshed.insert(slot.to_string(), now);
        true
    }

    /// Go `dropEntry` (provider.go:466): deletes `key`'s entry on a spawned task, bounded by
    /// `evict_timeout`. Detached, so a caller who drops its future does not cancel the delete;
    /// bounded, so a store that never answers cannot hold the caller. The store's error is
    /// ignored: nothing here can act on it.
    pub(crate) async fn drop_entry(&self, key: &CredentialKey) {
        let store = Arc::clone(&self.store);
        let key = key.clone();
        let bound = self.evict_timeout;
        let task = tokio::spawn(async move {
            let _ = tokio::time::timeout(bound, store.delete(&key)).await;
        });
        let _ = task.await;
    }
}

/// Go `publish` (provider.go:775): caches a successful client, frees the in-flight slot so a
/// failed attempt is retried, and releases the waiters, all under the lock.
fn publish(state: &Mutex<ProviderState>, init: &ClientInit, result: Result<Arc<Client>, Error>) {
    let mut state = state.lock();
    if let Ok(client) = &result {
        state.client = Some(Arc::clone(client));
    }
    state.pending = None;
    init.publish(result);
}

/// Go's `%v` of a recovered panic value.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

// ---- Credential and Refresh (Cycle 2 replaces this section) ----

#[async_trait::async_trait]
impl CredentialProvider for Provider {
    #[allow(unused_variables, clippy::todo)]
    async fn credential(&self, cx: &AuthContext) -> Result<Arc<dyn Credential>, crate::Error> {
        todo!("Task 9, cycle 2")
    }

    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl RefreshingProvider for Provider {
    #[allow(unused_variables, clippy::todo)]
    async fn refresh(
        &self,
        cx: &AuthContext,
        rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, crate::Error> {
        todo!("Task 9, cycle 2")
    }
}

// ---- end of Credential and Refresh ----

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::gcp::client::{DEFAULT_AGENT_IDENTITY_URL, DEFAULT_CONNECTOR_URL, DEFAULT_POLL_TIMEOUT};

    const RESOURCE: &str = "projects/p/locations/l/authProviders/ap";

    fn cfg_for(name: &str) -> ProviderConfig {
        ProviderConfig {
            scheme: ProviderScheme {
                name: name.to_string(),
                ..ProviderScheme::default()
            },
            ..ProviderConfig::default()
        }
    }

    // The provider internal tests replace the builder, so nothing else drives the default
    // one: it builds a Client against the production endpoints (it sends nothing).
    #[tokio::test]
    async fn default_builder_targets_the_production_endpoints() {
        let key = serde_json::json!({
            "type": "authorized_user",
            "client_id": "c",
            "client_secret": "s",
            "refresh_token": "r",
            "token_uri": "http://127.0.0.1:9/token",
        });
        let build = default_new_client(AdcSource::Json(key));
        let client = (*build)().await.expect("default builder");
        assert_eq!(client.agent_identity_url, DEFAULT_AGENT_IDENTITY_URL);
        assert_eq!(client.connector_url, DEFAULT_CONNECTOR_URL);
        assert_eq!(client.poll_timeout, DEFAULT_POLL_TIMEOUT);
    }

    #[test]
    fn refresh_slot_is_the_length_prefixed_join_of_user_and_scheme() {
        let a = ProviderScheme {
            name: "n".to_string(),
            scopes: vec!["b".to_string(), "a".to_string()],
            continue_uri: "c".to_string(),
        };
        let reordered = ProviderScheme {
            scopes: vec!["a".to_string(), "b".to_string()],
            ..a.clone()
        };
        let want = hex_encode(
            Sha256::digest(join_fields(&["u", "n", "2", "a", "b", "c"]).as_bytes()).as_slice(),
        );
        assert_eq!(refresh_slot("u", &a), want);
        assert_eq!(refresh_slot("u", &reordered), want, "scope order is not a dimension");
        assert_ne!(refresh_slot("v", &a), want, "the user is a dimension");
        assert_eq!(want.len(), 64);
    }

    #[test]
    fn provider_config_debug_shows_the_scheme() {
        assert_eq!(
            format!("{:?}", cfg_for(RESOURCE)),
            r#"ProviderConfig { scheme: ProviderScheme { name: "projects/p/locations/l/authProviders/ap", scopes: [], continue_uri: "" }, client: None, store: None }"#
        );
    }
}
