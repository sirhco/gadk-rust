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

//! Port of Go `auth/gcp/provider_test.go` (package `gcp_test`).
//!
//! Every test talks to in-process `TestServer`s on the real clock. The only real waits are
//! short: the 20 ms poll timeout in `test_sentinels_carry_the_resource`. The 5 s figures are
//! failure guards, not waits.

#![cfg(feature = "gcp")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use common::{RecordedRequest, Reply, TestServer};
use gadk_auth::gcp::{
    Client, Config, ErrorKind, ProviderConfig, ProviderScheme, find_api_error, is_kind,
    new_provider,
};
use gadk_auth::{
    AuthContext, Bearer, Credential, CredentialKey, CredentialProvider, CredentialStore, Error,
    Identity, InMemoryCredentialStore, RefreshingProvider, find_consent,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::watch;

const TEST_RESOURCE: &str = "projects/p/locations/l/authProviders/ap";

// ---------------------------------------------------------------- helpers

/// Go `adkContext`: an invocation identity for `user` under the app name "app". Go's session
/// service mints a random session id, and so does this.
fn adk_context(user: &str) -> AuthContext {
    adk_context_in("app", user)
}

/// Go `adkContextIn`.
fn adk_context_in(app: &str, user: &str) -> AuthContext {
    AuthContext::for_identity(Identity {
        user_id: user.to_string(),
        app_name: app.to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
    })
}

/// Go `userlessADKContext`: an invocation whose session carries no user. The ids are
/// distinctive, so the substring leak check cannot fire on ordinary prose.
fn userless_adk_context() -> AuthContext {
    AuthContext::for_identity(Identity {
        user_id: String::new(),
        app_name: "app-zqx7".to_string(),
        session_id: "sid-zqx7".to_string(),
    })
}

fn scheme(name: &str) -> ProviderScheme {
    ProviderScheme {
        name: name.to_string(),
        ..ProviderScheme::default()
    }
}

/// Go `cfgFor`.
fn cfg_for(name: &str) -> ProviderConfig {
    ProviderConfig {
        scheme: scheme(name),
        ..ProviderConfig::default()
    }
}

fn client_with(cfg: Config) -> Arc<Client> {
    Arc::new(Client::new(cfg).expect("NewClient()"))
}

/// A client whose Agent Identity endpoint is `srv`.
fn agent_identity_client(srv: &TestServer) -> Arc<Client> {
    client_with(Config {
        http_client: Some(srv.client().into()),
        agent_identity_endpoint: srv.url().to_string(),
        ..Config::default()
    })
}

fn provider_with(
    scheme: ProviderScheme,
    client: Arc<Client>,
    store: Option<Arc<dyn CredentialStore>>,
) -> Arc<dyn CredentialProvider> {
    new_provider(ProviderConfig {
        scheme,
        client: Some(client),
        store,
    })
    .expect("NewProvider()")
}

/// Go `newProvider`: a provider whose client targets `srv`.
fn provider_for(srv: &TestServer, scheme: ProviderScheme) -> Arc<dyn CredentialProvider> {
    provider_with(scheme, agent_identity_client(srv), None)
}

/// Go's `p.(auth.RefreshingProvider)`.
fn refreshing(p: &Arc<dyn CredentialProvider>) -> &dyn RefreshingProvider {
    p.as_refreshing()
        .expect("gcp provider does not implement auth.RefreshingProvider")
}

/// The bearer token `cred` carries, if it is a `Bearer`.
fn bearer(cred: &Arc<dyn Credential>) -> Option<String> {
    cred.downcast_ref::<Bearer>().map(|b| b.token.clone())
}

/// Go `bearerToken`: fails unless `cred` is a bearer credential.
fn bearer_token(cred: &Arc<dyn Credential>) -> String {
    bearer(cred).unwrap_or_else(|| panic!("credential = {cred:?}, want auth.BearerCredential"))
}

fn body_json(r: &RecordedRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

/// The `forceRefreshToken` a request carried ("" when absent).
fn force_refresh_token(r: &RecordedRequest) -> String {
    body_json(r)["forceRefreshToken"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn in_an_hour() -> String {
    (Utc::now() + TimeDelta::hours(1)).to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// The cache key the provider writes under for `user` in app "app".
fn key_for(client: &Client, scheme: &ProviderScheme, user: &str) -> CredentialKey {
    client.cache_key(
        scheme,
        &Identity {
            app_name: "app".to_string(),
            user_id: user.to_string(),
            session_id: String::new(),
        },
    )
}

async fn is_cached(store: &dyn CredentialStore, key: &CredentialKey) -> bool {
    store.get(key).await.expect("store Get()").is_some()
}

/// Go `echoRequest`: what a request told the service.
#[derive(Debug, Clone, Default)]
struct EchoRequest {
    /// Carries the resource name, which is not in the body.
    path: String,
    user_id: String,
    scopes: Vec<String>,
    continue_uri: String,
    /// `X-Caller-Identity`, set by `IdentityMiddleware`.
    caller: String,
}

impl EchoRequest {
    fn of(r: &RecordedRequest) -> Self {
        let body = body_json(r);
        let text = |k: &str| body[k].as_str().unwrap_or_default().to_string();
        EchoRequest {
            path: r.path.clone(),
            user_id: text("userId"),
            scopes: body["scopes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            continue_uri: text("continueUri"),
            caller: r
                .header("X-Caller-Identity")
                .unwrap_or_default()
                .to_string(),
        }
    }

    /// Go `echoRequest.token`: everything the service was told, length-prefixed.
    fn token(&self) -> String {
        let count = self.scopes.len().to_string();
        let mut fields = vec![
            "tok",
            self.path.as_str(),
            self.caller.as_str(),
            self.user_id.as_str(),
            count.as_str(),
        ];
        fields.extend(self.scopes.iter().map(String::as_str));
        fields.push(self.continue_uri.as_str());
        fields.iter().map(|f| format!("{}:{f}", f.len())).collect()
    }
}

/// Go `newEchoServer`: answers every retrieval with a token naming what the request asked
/// for, expiring at `expire_time` (RFC 3339; empty for no expiry). It records each request.
async fn new_echo_server(expire_time: &str) -> TestServer {
    let expire_time = expire_time.to_string();
    TestServer::start(move |r: RecordedRequest| {
        let req = EchoRequest::of(&r);
        let mut resp = json!({"token": req.token(), "header": "Authorization: Bearer"});
        if !expire_time.is_empty() {
            resp["expireTime"] = json!(expire_time);
        }
        let body = json!({ "success": resp }).to_string();
        async move { Reply::ok(body) }
    })
    .await
}

/// Go `echoServer.requireServed`: every token in `got` is the one minted for the only request.
fn require_served(e: &TestServer, got: &[String]) {
    let requests = e.requests();
    assert_eq!(
        requests.len(),
        1,
        "service requests = {}, want 1",
        requests.len()
    );
    let want = EchoRequest::of(&requests[0]).token();
    for (i, tok) in got.iter().enumerate() {
        assert_eq!(
            tok, &want,
            "resolve {i} served {tok:?}, want the token the service minted, {want:?}"
        );
    }
}

/// Go `identityTransport`: stamps a caller identity on every request, standing in for the
/// distinct service-account credentials a caller-supplied HTTP client carries.
struct IdentityMiddleware {
    who: String,
}

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for IdentityMiddleware {
    async fn handle(
        &self,
        mut req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        req.headers_mut().insert(
            "X-Caller-Identity",
            http::HeaderValue::from_str(&self.who).expect("caller identity header"),
        );
        next.run(req, extensions).await
    }
}

/// Go `newSharingProvider`: a provider against `e` sharing `store` with every provider built
/// the same way. Two providers given the same `who` share one `Client`.
fn new_sharing_provider(
    e: &TestServer,
    store: &Arc<InMemoryCredentialStore>,
    clients: &mut HashMap<String, Arc<Client>>,
    who: &str,
    scheme: ProviderScheme,
) -> Arc<dyn CredentialProvider> {
    let client = clients
        .entry(who.to_string())
        .or_insert_with(|| {
            let http = reqwest_middleware::ClientBuilder::new(e.client())
                .with(IdentityMiddleware {
                    who: who.to_string(),
                })
                .build();
            client_with(Config {
                http_client: Some(http),
                agent_identity_endpoint: e.url().to_string(),
                ..Config::default()
            })
        })
        .clone();
    let store: Arc<dyn CredentialStore> = store.clone();
    provider_with(scheme, client, Some(store))
}

/// Go `recordingStore`'s counters and last write.
#[derive(Default)]
struct Recorded {
    gets: usize,
    sets: usize,
    last_key: CredentialKey,
    last_expires: Option<DateTime<Utc>>,
    /// The task the last `set` ran on (Go records whether its context was cancellable).
    set_task: Option<tokio::task::Id>,
}

/// Go `recordingStore`: reports every call and what it was handed, and can fail either way.
#[derive(Default)]
struct RecordingStore {
    inner: InMemoryCredentialStore,
    get_err: Option<&'static str>,
    set_err: Option<&'static str>,
    seen: Mutex<Recorded>,
}

#[async_trait::async_trait]
impl CredentialStore for RecordingStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, Error> {
        self.seen.lock().gets += 1;
        if let Some(msg) = self.get_err {
            return Err(Error::msg(msg));
        }
        self.inner.get(key).await
    }

    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        {
            let mut seen = self.seen.lock();
            seen.sets += 1;
            seen.last_key = key.clone();
            seen.last_expires = Some(expires_at);
            seen.set_task = tokio::task::try_id();
        }
        if let Some(msg) = self.set_err {
            return Err(Error::msg(msg));
        }
        self.inner.set(key, cred, expires_at).await
    }

    async fn delete(&self, key: &CredentialKey) -> Result<(), Error> {
        self.inner.delete(key).await
    }
}

/// Go `newStoreProvider`: a provider against `e` backed by `store`.
fn new_store_provider(
    e: &TestServer,
    store: Arc<dyn CredentialStore>,
) -> Arc<dyn CredentialProvider> {
    provider_with(scheme(TEST_RESOURCE), agent_identity_client(e), Some(store))
}

/// Go `refreshFixture`'s results: a provider whose service answers retrieval n with `tok<n>`,
/// expiring in an hour, under the given header.
struct RefreshFixture {
    p: Arc<dyn CredentialProvider>,
    store: Arc<InMemoryCredentialStore>,
    client: Arc<Client>,
    srv: TestServer,
}

impl RefreshFixture {
    fn rp(&self) -> &dyn RefreshingProvider {
        refreshing(&self.p)
    }

    /// Go's `seen()`: the call count and the last request's `forceRefreshToken`.
    fn seen(&self) -> (usize, String) {
        let requests = self.srv.requests();
        let hint = requests.last().map(force_refresh_token).unwrap_or_default();
        (requests.len(), hint)
    }
}

async fn refresh_fixture(header: &'static str) -> RefreshFixture {
    let calls = Arc::new(AtomicUsize::new(0));
    let srv = TestServer::start(move |_r: RecordedRequest| {
        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"success": {
            "token": format!("tok{n}"),
            "header": header,
            "expireTime": in_an_hour(),
        }})
        .to_string();
        async move { Reply::ok(body) }
    })
    .await;
    let client = agent_identity_client(&srv);
    let store = Arc::new(InMemoryCredentialStore::new());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let p = provider_with(scheme(TEST_RESOURCE), Arc::clone(&client), Some(dyn_store));
    assert!(
        p.as_refreshing().is_some(),
        "the gcp provider does not implement auth.RefreshingProvider"
    );
    RefreshFixture {
        p,
        store,
        client,
        srv,
    }
}

/// Go `replacingServer`'s state.
struct Replacing {
    forced: usize,
    current: usize,
}

/// Go `replacingServer`: keeps returning the token it already issued, and mints a replacement
/// only when sent that token as the force-refresh hint. `hold`, when set, blocks each forced
/// retrieval until it reads `true`.
async fn replacing_server(
    hold: Option<watch::Receiver<bool>>,
) -> (TestServer, Arc<Mutex<Replacing>>) {
    let state = Arc::new(Mutex::new(Replacing {
        forced: 0,
        current: 1,
    }));
    let shared = Arc::clone(&state);
    let srv = TestServer::start(move |r: RecordedRequest| {
        let hint = force_refresh_token(&r);
        let (tok, is_forced) = {
            let mut s = shared.lock();
            let is_forced = !hint.is_empty() && hint == format!("tok{}", s.current);
            if is_forced {
                s.forced += 1;
                s.current += 1;
            }
            (format!("tok{}", s.current), is_forced)
        };
        let body = json!({"success": {
            "token": tok,
            "header": "Authorization: Bearer",
            "expireTime": in_an_hour(),
        }})
        .to_string();
        let hold = hold.clone();
        async move {
            if is_forced && let Some(mut hold) = hold {
                let _ = hold.wait_for(|released| *released).await;
            }
            Reply::ok(body)
        }
    })
    .await;
    (srv, state)
}

/// Go's `seen()` for a replacing server: (calls, forced).
fn replacing_seen(srv: &TestServer, state: &Mutex<Replacing>) -> (usize, usize) {
    (srv.calls(), state.lock().forced)
}

/// Go `replacingProvider`.
fn replacing_provider(
    srv: &TestServer,
    store: Arc<dyn CredentialStore>,
) -> (Arc<dyn CredentialProvider>, Arc<Client>) {
    let client = agent_identity_client(srv);
    let p = provider_with(scheme(TEST_RESOURCE), Arc::clone(&client), Some(store));
    (p, client)
}

/// Go reads a `*auth.BearerCredential` back from `pointerStore`, a type `credentialToken`
/// cannot read. This is that type: a bearer the provider's token reader does not recognise.
#[derive(Debug)]
struct PointerBearer(Bearer);

#[async_trait::async_trait]
impl Credential for PointerBearer {
    async fn apply(&self, headers: &mut http::HeaderMap) -> Result<(), Error> {
        self.0.apply(headers).await
    }
}

/// Go `pointerStore`: hands back a stored bearer as a `PointerBearer`.
#[derive(Default)]
struct PointerStore(InMemoryCredentialStore);

#[async_trait::async_trait]
impl CredentialStore for PointerStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, Error> {
        let Some(cred) = self.0.get(key).await? else {
            return Ok(None);
        };
        if let Some(b) = cred.downcast_ref::<Bearer>() {
            let wrapped: Arc<dyn Credential> = Arc::new(PointerBearer(b.clone()));
            return Ok(Some(wrapped));
        }
        Ok(Some(cred))
    }

    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        self.0.set(key, cred, expires_at).await
    }

    async fn delete(&self, key: &CredentialKey) -> Result<(), Error> {
        self.0.delete(key).await
    }
}

/// Go `swappingStore`'s state.
#[derive(Default)]
struct Swap {
    /// Gets still to answer with `pinned` before swapping.
    armed: usize,
    pinned: Option<Arc<dyn Credential>>,
    /// What every later get sees, as if somebody else had written.
    swapped: Option<Arc<dyn Credential>>,
    deletes: usize,
}

/// Go `swappingStore`: lets a test place a credential change between two of the provider's
/// reads.
#[derive(Default)]
struct SwappingStore {
    inner: InMemoryCredentialStore,
    swap: Mutex<Swap>,
}

#[async_trait::async_trait]
impl CredentialStore for SwappingStore {
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, Error> {
        let answer = {
            let mut s = self.swap.lock();
            if s.armed > 0 {
                s.armed -= 1;
                Some(s.pinned.clone())
            } else {
                s.swapped.clone().map(Some)
            }
        };
        match answer {
            Some(answer) => Ok(answer),
            None => self.inner.get(key).await,
        }
    }

    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        self.inner.set(key, cred, expires_at).await
    }

    async fn delete(&self, key: &CredentialKey) -> Result<(), Error> {
        self.swap.lock().deletes += 1;
        self.inner.delete(key).await
    }
}

// ---------------------------------------------------------------- tests

// TestProviderCredential drives two users through one shared provider: serving one user's
// credential to another is the failure that matters. It also pins scopes and continueUri on the
// wire.
#[tokio::test]
async fn test_provider_credential() {
    let srv = TestServer::start(|r: RecordedRequest| {
        // Echo the caller back, so a credential served to the wrong user shows up.
        let user = EchoRequest::of(&r).user_id;
        let body =
            format!(r#"{{"success":{{"token":"tok-{user}","header":"Authorization: Bearer"}}}}"#);
        async move { Reply::ok(body) }
    })
    .await;

    let mut scopes = vec!["s1".to_string(), "s2".to_string()];
    let p = provider_for(
        &srv,
        ProviderScheme {
            name: TEST_RESOURCE.to_string(),
            scopes: scopes.clone(),
            continue_uri: "https://example.test/continue".to_string(),
        },
    );
    scopes[0] = "mutated".to_string(); // the provider holds its own copy

    for user in ["alice", "bob"] {
        let cred = p
            .credential(&adk_context(user))
            .await
            .unwrap_or_else(|e| panic!("Credential({user:?}) error = {e}"));
        assert_eq!(
            bearer(&cred),
            Some(format!("tok-{user}")),
            "credential for {user:?} = {cred:?}, want bearer \"tok-{user}\""
        );
    }
    let seen: Vec<EchoRequest> = srv.requests().iter().map(EchoRequest::of).collect();
    let users: Vec<&str> = seen.iter().map(|r| r.user_id.as_str()).collect();
    assert_eq!(
        users,
        ["alice", "bob"],
        "service saw users {users:?}, want [alice bob]"
    );
    let last = seen.last().expect("a request");
    assert_eq!(
        last.scopes,
        ["s1", "s2"],
        "body scopes = {:?}, want [s1 s2] (caller's later mutation must not leak)",
        last.scopes
    );
    assert_eq!(
        last.continue_uri, "https://example.test/continue",
        "body continueUri = {:?}, want the scheme's",
        last.continue_uri
    );
}

// TestProviderCredentialConcurrent drives four users through one shared provider at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_provider_credential_concurrent() {
    let srv = TestServer::start(|r: RecordedRequest| {
        let user = EchoRequest::of(&r).user_id;
        // With an expiry, so these tasks drive the cache write and read too.
        let body = format!(
            r#"{{"success":{{"token":"tok-{user}","header":"Authorization: Bearer","expireTime":"2999-01-01T00:00:00Z"}}}}"#
        );
        async move { Reply::ok(body) }
    })
    .await;

    // Several scopes, so the sort in the slot derivation runs concurrently.
    let p = provider_for(
        &srv,
        ProviderScheme {
            name: TEST_RESOURCE.to_string(),
            scopes: vec!["b".to_string(), "a".to_string(), "c".to_string()],
            ..ProviderScheme::default()
        },
    );
    let users = ["alice", "bob", "carol", "dave"];
    let ctxs: Vec<AuthContext> = users.iter().map(|u| adk_context(u)).collect();

    let mut set = tokio::task::JoinSet::new();
    for i in 0..users.len() * 8 {
        let p = Arc::clone(&p);
        let cx = ctxs[i % users.len()].clone();
        set.spawn(async move { (i, p.credential(&cx).await) });
    }
    while let Some(joined) = set.join_next().await {
        let (i, res) = joined.expect("credential task");
        let user = users[i % users.len()];
        let cred = res.unwrap_or_else(|e| panic!("Credential({user}) error = {e}"));
        let want = format!("tok-{user}");
        assert_eq!(
            bearer(&cred),
            Some(want.clone()),
            "credential {i} = {cred:?}, want bearer {want:?}"
        );
    }
}

// TestProviderErrorAttribution pins that a failed retrieval names the resource, names no
// caller-supplied id, and keeps the APIError matchable.
#[tokio::test]
async fn test_provider_error_attribution() {
    let srv = TestServer::start(|_| async { Reply::status(403, "denied") }).await;

    let p = provider_for(&srv, scheme(TEST_RESOURCE));
    let cx = adk_context("alice@example.test");
    let session_id = cx.identity.as_ref().expect("identity").session_id.clone();
    let err = p
        .credential(&cx)
        .await
        .expect_err("Credential() = a credential, want the service failure");
    let text = err.to_string();
    assert!(
        text.contains(TEST_RESOURCE),
        "Credential() error = {text}, want it to name the resource"
    );
    for unwanted in ["alice@example.test", session_id.as_str()] {
        assert!(
            !text.contains(unwanted),
            "Credential() error = {text}, want the caller-supplied {unwanted:?} kept out of it"
        );
    }
    assert_eq!(
        find_api_error(&err).map(|a| a.status_code),
        Some(403),
        "Credential() error = {text}, want a wrapped ApiError with status 403"
    );
}

// TestServiceEchoIsRedacted: a service quoting back the acting user must have it scrubbed from
// both the message and the ApiError body field.
#[tokio::test]
async fn test_service_echo_is_redacted() {
    const USER: &str = "alice@example.test";
    let srv = TestServer::start(|_| async {
        Reply::status(
            400,
            format!(r#"{{"error":{{"message":"invalid userId: {USER}"}}}}"#),
        )
    })
    .await;

    let p = provider_for(&srv, scheme(TEST_RESOURCE));
    let err = p
        .credential(&adk_context(USER))
        .await
        .expect_err("Credential() = a credential, want the service failure");
    let text = err.to_string();
    assert!(
        !text.contains(USER),
        "Credential() error = {text}, want the acting user redacted out of the service's echoed body"
    );
    let api = find_api_error(&err)
        .filter(|a| a.status_code == 400)
        .unwrap_or_else(|| {
            panic!("Credential() error = {text}, want a wrapped ApiError with status 400")
        });
    assert!(
        text.to_lowercase().contains("invalid userid"),
        "Credential() error = {text}, want the service's message kept apart from the id"
    );
    assert!(
        !api.body.contains(USER),
        "ApiError.body = {:?}, want the acting user redacted from the field too",
        api.body
    );
}

// TestConnectorOperationErrorIsRedacted covers a connector's terminal failure inside a 200,
// which never becomes an ApiError.
#[tokio::test]
async fn test_connector_operation_error_is_redacted() {
    const USER: &str = "alice@example.test";
    let srv = TestServer::start(|_| async {
        Reply::json(format!(
            r#"{{"done":true,"error":{{"code":3,"message":"invalid userId: {USER}"}}}}"#
        ))
    })
    .await;

    let client = client_with(Config {
        http_client: Some(srv.client().into()),
        connector_endpoint: srv.url().to_string(),
        ..Config::default()
    });
    let p = provider_with(scheme("projects/p/locations/l/connectors/c"), client, None);
    let err = p
        .credential(&adk_context(USER))
        .await
        .expect_err("Credential() = a credential, want the operation failure");
    let text = err.to_string();
    assert!(
        !text.contains(USER),
        "Credential() error = {text}, want the acting user redacted"
    );
    assert!(
        text.to_lowercase().contains("invalid userid"),
        "Credential() error = {text}, want the service's message kept apart from the id"
    );
}

// TestALongEchoStillRedacts pins that redaction runs before the 1 KiB cap.
#[tokio::test]
async fn test_a_long_echo_still_redacts() {
    const USER: &str = "alice@example.test";
    // Padding sized so the identifier lands across the 1 KiB cap.
    let srv =
        TestServer::start(|_| async { Reply::status(400, format!("{}{USER}", "x".repeat(1015))) })
            .await;

    let p = provider_for(&srv, scheme(TEST_RESOURCE));
    let err = p
        .credential(&adk_context(USER))
        .await
        .expect_err("Credential() = a credential, want the service failure");
    let text = err.to_string();
    for unwanted in [USER, &USER[..8]] {
        assert!(
            !text.contains(unwanted),
            "Credential() error = {text}, want {unwanted:?} gone: redaction must run before the cap"
        );
    }
}

// TestAnEscapedEchoIsRedacted: an identifier echoed with `\u0040` for the @ is redacted too.
#[tokio::test]
async fn test_an_escaped_echo_is_redacted() {
    let srv = TestServer::start(|_| async {
        Reply::status(
            400,
            r#"{"error":{"message":"invalid userId: alice\u0040example.test"}}"#,
        )
    })
    .await;

    let p = provider_for(&srv, scheme(TEST_RESOURCE));
    let err = p
        .credential(&adk_context("alice@example.test"))
        .await
        .expect_err("Credential() = a credential, want the service failure");
    assert!(
        !err.to_string().contains("example.test"),
        "Credential() error = {err}, want the escaped identifier redacted too"
    );
}

// TestSentinelsCarryTheResource: the consent-rejected and poll-timeout sentinels name the
// resource exactly once.
#[tokio::test]
async fn test_sentinels_carry_the_resource() {
    for (name, body, want) in [
        (
            "consent rejected",
            r#"{"consentRejected":{}}"#,
            ErrorKind::ConsentRejected,
        ),
        ("poll timeout", r#"{"pending":{}}"#, ErrorKind::PollTimeout),
    ] {
        let srv = TestServer::start(move |_| async move { Reply::json(body) }).await;
        // A short poll timeout: this test is about the error's text, not the wait.
        let client = client_with(Config {
            http_client: Some(srv.client().into()),
            agent_identity_endpoint: srv.url().to_string(),
            poll_timeout: Duration::from_millis(20),
            ..Config::default()
        });
        let p = provider_with(scheme(TEST_RESOURCE), client, None);
        let err = p
            .credential(&adk_context("alice@example.test"))
            .await
            .expect_err("Credential() = a credential, want the sentinel");
        assert!(
            is_kind(&err, want),
            "{name}: Credential() error = {err}, want is_kind {want:?}"
        );
        let n = err.to_string().matches(TEST_RESOURCE).count();
        assert_eq!(
            n, 1,
            "{name}: Credential() error = {err} names the resource {n} times, want exactly 1: one client serves several resources, and this sentinel used to name none"
        );
    }
}

// TestProviderNoActingUser covers both identity failures: rejected before any service call,
// distinguishable, and naming no caller-supplied id.
#[tokio::test]
async fn test_provider_no_acting_user() {
    #[allow(clippy::type_complexity)]
    let rows: [(&str, fn() -> AuthContext, &str, &[&str]); 2] = [
        (
            "not an ADK context",
            AuthContext::anonymous,
            "no ADK invocation identity",
            &[],
        ),
        (
            "invocation without a user",
            userless_adk_context,
            "carries no user",
            &["app-zqx7", "sid-zqx7"],
        ),
    ];
    for (name, ctx, want_msg, leaks) in rows {
        // Counted below: no identity means no service call.
        let srv = TestServer::start(|_| async { Reply::status(500, "") }).await;

        let p = provider_for(&srv, scheme(TEST_RESOURCE));
        let err = p
            .credential(&ctx())
            .await
            .expect_err("Credential() = a credential, want ErrNoActingUser");
        assert!(
            is_kind(&err, ErrorKind::NoActingUser),
            "{name}: Credential() error = {err}, want gcp.ErrNoActingUser"
        );
        assert!(
            err.to_string().contains(want_msg),
            "{name}: Credential() error = {err}, want it to mention {want_msg:?}"
        );
        for unwanted in leaks {
            assert!(
                !err.to_string().contains(unwanted),
                "{name}: Credential() error = {err}, want the caller-supplied {unwanted:?} kept out of it"
            );
        }
        assert_eq!(
            srv.calls(),
            0,
            "{name}: credentials service must not be called without an ADK identity"
        );
    }
    // Go's exact texts.
    let err = provider_for(&new_echo_server("").await, scheme(TEST_RESOURCE))
        .credential(&AuthContext::anonymous())
        .await
        .expect_err("Credential()");
    assert_eq!(
        err.to_string(),
        "gcp: no acting user: no ADK invocation identity on the context"
    );
    let err = provider_for(&new_echo_server("").await, scheme(TEST_RESOURCE))
        .credential(&userless_adk_context())
        .await
        .expect_err("Credential()");
    assert_eq!(
        err.to_string(),
        "gcp: no acting user: the invocation identity carries no user"
    );
}

// parity: adapted: the "unconstructed client" row cannot be built (a Client only comes from Client::new/with_adc); every other row is kept
#[test]
fn test_new_provider_validates_scheme() {
    // Everything here is a wiring mistake, and each must fail at construction.
    let bad: [(&str, ProviderConfig); 11] = [
        ("empty name", ProviderConfig::default()),
        (
            "extra segments where the provider id belongs",
            cfg_for("projects/p/locations/l/authProviders/../../secret"),
        ),
        (
            "empty path segment",
            cfg_for("projects/p/locations/l/authProviders//ap"),
        ),
        (
            "trailing slash routes differently after normalization",
            cfg_for("projects/p/locations/l/connectors/c/"),
        ),
        (
            "relative segment as the whole provider id",
            cfg_for("projects/p/locations/l/authProviders/.."),
        ),
        ("not a resource name at all", cfg_for("Bearer")),
        (
            "unknown collection",
            cfg_for("projects/p/locations/l/authProvidrs/ap"),
        ),
        ("truncated", cfg_for("projects/p")),
        (
            "query injected into the provider id",
            cfg_for("projects/p/locations/l/authProviders/ap?alt=json"),
        ),
        (
            "percent-escape in the provider id",
            cfg_for("projects/p/locations/l/authProviders/a%2Fb"),
        ),
        (
            "space in the provider id",
            cfg_for("projects/p/locations/l/authProviders/a b"),
        ),
    ];
    for (name, cfg) in bad {
        assert!(
            new_provider(cfg).is_err(),
            "{name}: NewProvider() = a provider, want the config rejected"
        );
    }

    let good = [
        TEST_RESOURCE,
        "projects/p/locations/l/connectors/c",
        // Domain-scoped project ids carry a colon.
        "projects/example.com:my-project/locations/l/authProviders/ap",
    ];
    for name in good {
        if let Err(e) = new_provider(cfg_for(name)) {
            panic!("accepts {name}: NewProvider({name:?}) error = {e}");
        }
    }

    // Go's exact texts for the two messages this function writes itself.
    let text = |cfg| match new_provider(cfg) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("NewProvider() = a provider, want an error"),
    };
    assert_eq!(
        text(ProviderConfig::default()),
        "gcp: new_provider requires a scheme name"
    );
    assert_eq!(
        text(cfg_for("Bearer")),
        r#"gcp: new_provider: scheme name "Bearer" is neither projects/*/locations/*/connectors/* nor projects/*/locations/*/authProviders/*"#
    );
    assert!(
        text(cfg_for("projects/p/locations/l/authProviders/a b"))
            .starts_with("gcp: new_provider: resource "),
        "a validate_resource failure is prefixed with \"gcp: new_provider: \""
    );
}

/// One arm of `test_provider_errors_stay_classifiable_through_credential`.
struct Arm {
    name: &'static str,
    status: u16,
    body: &'static str,
    check: fn(&Error) -> bool,
    want: &'static str,
}

// TestProviderErrorsStayClassifiableThroughCredential pins that the consent error, the
// sentinels and the ApiError reach a caller through Credential still classifiable.
#[tokio::test]
async fn test_provider_errors_stay_classifiable_through_credential() {
    let arms = [
        Arm {
            name: "consent required",
            status: 200,
            body: r#"{"uriConsentRequired":{"authorizationUri":"https://consent.example/auth","consentNonce":"n"}}"#,
            check: |err: &Error| {
                find_consent(err).is_some_and(|c| c.auth_uri == "https://consent.example/auth")
            },
            want: "*auth.ConsentRequiredError with its AuthURI intact",
        },
        Arm {
            name: "undecodable response",
            status: 200,
            body: r#"{"success": NOT JSON"#,
            check: |err: &Error| is_kind(err, ErrorKind::MalformedResponse),
            want: "gcp.ErrMalformedResponse",
        },
        Arm {
            name: "permission denied",
            status: 403,
            body: r#"{"error":{"code":403,"message":"denied"}}"#,
            check: |err: &Error| find_api_error(err).is_some_and(|a| a.status_code == 403),
            want: "*gcp.APIError carrying the status",
        },
    ];
    for arm in arms {
        let (status, body) = (arm.status, arm.body);
        let srv = TestServer::start(move |_| async move { Reply::status(status, body) }).await;

        let p = provider_for(&srv, scheme(TEST_RESOURCE));
        let err = p
            .credential(&adk_context("alice@example.test"))
            .await
            .expect_err("Credential() = a credential, want the service failure surfaced");
        assert!(
            (arm.check)(&err),
            "{}: Credential() error = {err}, want it still classifiable as {}",
            arm.name,
            arm.want
        );
    }
}

/// Set in the child process `test_credential_surfaces_an_unavailable_client` spawns.
const ADC_CHILD_ENV: &str = "GADK_AUTH_GCP_PROVIDER_ADC_CHILD";

/// The Go test body, run in the child, where `GOOGLE_APPLICATION_CREDENTIALS` names a file
/// that does not exist, so ADC discovery fails on the open rather than on the contents.
async fn credential_surfaces_an_unavailable_client_in_child() {
    let p = new_provider(cfg_for(TEST_RESOURCE)).expect("NewProvider()");
    let err = p
        .credential(&adk_context("alice@example.test"))
        .await
        .expect_err("Credential() = a credential, want ErrClientUnavailable");
    assert!(
        is_kind(&err, ErrorKind::ClientUnavailable),
        "Credential() error = {err}, want ErrClientUnavailable"
    );
    assert!(
        !err.to_string().contains(TEST_RESOURCE),
        "Credential() error = {err:?}, want the resource left out of a client-init failure"
    );
}

// TestCredentialSurfacesAnUnavailableClient pins that a failure to build the default client
// reaches the caller as ErrClientUnavailable through the exported Credential.
// parity: adapted: GOOGLE_APPLICATION_CREDENTIALS cannot be set in-process without unsafe, so the test re-runs itself in a child process with the variable naming a missing file; the child takes the real lazy ADC path offline
#[test]
fn test_credential_surfaces_an_unavailable_client() {
    if std::env::var_os(ADC_CHILD_ENV).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(credential_surfaces_an_unavailable_client_in_child());
        return;
    }
    let absent =
        std::env::temp_dir().join(format!("gadk-auth-absent-{}.json", uuid::Uuid::new_v4()));
    let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
        .args([
            "test_credential_surfaces_an_unavailable_client",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ADC_CHILD_ENV, "1")
        .env("GOOGLE_APPLICATION_CREDENTIALS", &absent)
        .output()
        .expect("spawn the child test process");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "child test failed:\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran no test:\n--- stdout\n{stdout}"
    );
}

/// One service of `test_provider_refresh_forces_new_token`.
struct ForceCase {
    name: &'static str,
    resource: &'static str,
    endpoint: fn(&mut Config, String),
    /// The service-specific success envelope carrying a token.
    success: fn(&str) -> String,
    /// Whether a request body asked for a forced refresh.
    forced: fn(&Value) -> bool,
}

// Both services mint a fresh token when asked to force a refresh: Agent Identity is sent the
// rejected token as forceRefreshToken, the connector the boolean forceRefresh.
#[tokio::test]
async fn test_provider_refresh_forces_new_token() {
    let cases = [
        ForceCase {
            name: "agent identity",
            resource: "projects/p/locations/l/authProviders/ap",
            endpoint: |cfg, url| cfg.agent_identity_endpoint = url,
            success: |tok| {
                format!(
                    r#"{{"success":{{"token":"{tok}","header":"Authorization: Bearer","expireTime":"2999-01-01T00:00:00Z"}}}}"#
                )
            },
            forced: |b| {
                b["forceRefreshToken"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            },
        },
        ForceCase {
            name: "iam connector",
            resource: "projects/p/locations/l/connectors/c",
            endpoint: |cfg, url| cfg.connector_endpoint = url,
            success: |tok| {
                format!(
                    r#"{{"done":true,"response":{{"token":"{tok}","header":"Authorization: Bearer","expireTime":"2999-01-01T00:00:00Z"}}}}"#
                )
            },
            forced: |b| b["forceRefresh"].as_bool() == Some(true),
        },
    ];

    for tc in cases {
        let (success, forced) = (tc.success, tc.forced);
        let srv = TestServer::start(move |r: RecordedRequest| {
            // A forced refresh mints a new token; the expiry makes it cacheable.
            let tok = if forced(&body_json(&r)) {
                "tok2"
            } else {
                "tok1"
            };
            let body = success(tok);
            async move { Reply::ok(body) }
        })
        .await;

        let mut cfg = Config {
            http_client: Some(srv.client().into()),
            ..Config::default()
        };
        (tc.endpoint)(&mut cfg, srv.url().to_string());
        let p = provider_with(scheme(tc.resource), client_with(cfg), None);
        let rp = refreshing(&p);
        let cx = adk_context("user-1");

        // Prime the cache with the initial token.
        let first = p.credential(&cx).await.expect("Credential()");
        assert_eq!(
            bearer(&first).as_deref(),
            Some("tok1"),
            "{}: initial credential = {first:?}, want tok1",
            tc.name
        );

        // Refresh sends the rejected token back and returns a new one.
        let cred = rp.refresh(&cx, &first).await.expect("Refresh()");
        let last = srv.requests().last().map(body_json).expect("a request");
        assert!(
            forced(&last),
            "{}: request body {last} did not ask for a forced refresh",
            tc.name
        );
        // Agent Identity is told which token to replace; the connector is not sent the field.
        let want_token = if tc.resource.contains("/authProviders/") {
            "tok1"
        } else {
            ""
        };
        let got_token = last["forceRefreshToken"].as_str().unwrap_or_default();
        assert_eq!(
            got_token, want_token,
            "{}: forceRefreshToken = {got_token:?}, want {want_token:?}",
            tc.name
        );
        assert_eq!(
            bearer(&cred).as_deref(),
            Some("tok2"),
            "{}: refreshed credential = {cred:?}, want tok2",
            tc.name
        );

        // The refreshed credential replaces the cached one.
        let cached = p.credential(&cx).await.expect("Credential() after refresh");
        assert_eq!(
            bearer(&cached).as_deref(),
            Some("tok2"),
            "{}: cached credential after refresh = {cached:?}, want tok2",
            tc.name
        );
    }
}

// TestServiceEchoIsRedactedAcrossCase: a service that lowercases the acting user before
// echoing it is still scrubbed.
#[tokio::test]
async fn test_service_echo_is_redacted_across_case() {
    const ACTING: &str = "Alice@Example.test";
    let srv = TestServer::start(|_| async {
        Reply::status(
            400,
            r#"{"error":{"message":"invalid userId: alice@example.test"}}"#,
        )
    })
    .await;

    let p = provider_for(&srv, scheme(TEST_RESOURCE));
    let err = p
        .credential(&adk_context(ACTING))
        .await
        .expect_err("Credential() = a credential, want the service failure");
    let lowered = err.to_string().to_lowercase();
    assert!(
        !lowered.contains(&ACTING.to_lowercase()),
        "Credential() error = {err}, want the acting user redacted whatever case the service echoed it in"
    );
    let api = find_api_error(&err)
        .unwrap_or_else(|| panic!("Credential() error = {err}, want a wrapped ApiError"));
    assert!(
        !api.body.to_lowercase().contains(&ACTING.to_lowercase()),
        "ApiError.body = {:?}, want the acting user redacted from the field too",
        api.body
    );
    assert!(
        lowered.contains("invalid userid"),
        "Credential() error = {err}, want the service's message kept apart from the id"
    );
}

/// One row of `test_redaction_covers_the_whole_secret_surface`.
struct SecretCase {
    name: &'static str,
    user: &'static str,
    continue_uri: &'static str,
    /// Echoed back by the service on a 400.
    body: String,
    /// Must not survive; compared case-insensitively. Empty to skip.
    unwanted: &'static str,
    /// Service wording that must survive. Empty to skip.
    want_intact: &'static str,
}

// TestRedactionCoversTheWholeSecretSurface covers inputs the other redaction tests miss.
#[tokio::test]
async fn test_redaction_covers_the_whole_secret_surface() {
    let cases = [
        SecretCase {
            name: "a non-ASCII acting user echoed in lower case",
            user: "Émile@example.test",
            continue_uri: "",
            body: r#"{"error":{"message":"invalid userId: émile@example.test"}}"#.to_string(),
            unwanted: "émile@example.test",
            want_intact: "invalid userId: ",
        },
        SecretCase {
            name: "the continue URI echoed back",
            user: "alice@example.test",
            continue_uri: "https://app.example.test/finish?login_hint=alice%40example.test",
            body: r#"{"error":{"message":"bad continueUri: https://app.example.test/finish?login_hint=alice%40example.test"}}"#.to_string(),
            unwanted: "https://app.example.test/finish",
            want_intact: "bad continueUri: ",
        },
        SecretCase {
            name: "an acting user whose lowercase is a different length",
            user: "\u{130}van@example.test",
            continue_uri: "",
            body: r#"{"error":{"message":"invalid userId: ivan@example.test"}}"#.to_string(),
            unwanted: "ivan@example.test",
            want_intact: "invalid userId: ",
        },
        SecretCase {
            name: "a length-changing rune ahead of the echo",
            user: "alice@example.test",
            continue_uri: "",
            body: r#"{"error":{"message":"\u0130 invalid userId: alice@example.test"}}"#.to_string(),
            unwanted: "alice@example.test",
            want_intact: "invalid userId: ",
        },
        SecretCase {
            name: "a body whose shrinking and growing runes balance out",
            user: "alice",
            continue_uri: "",
            body: "\u{130}\u{130}\u{130}\u{130}\u{130}alice\u{23a}\u{23a}\u{23a}\u{23a}\u{23a}"
                .to_string(),
            unwanted: "alice",
            want_intact: "",
        },
        SecretCase {
            name: "an acting user outside the BMP, echoed as a surrogate pair",
            user: "alice\u{1F600}",
            continue_uri: "",
            body: r#"{"error":{"message":"invalid userId: alice\ud83d\ude00"}}"#.to_string(),
            unwanted: r"\ud83d",
            want_intact: "invalid userId: ",
        },
        SecretCase {
            name: "a continue URI echoed with escaped slashes",
            user: "alice@example.test",
            continue_uri: "https://app.example.test/oauth/callback",
            body: r#"{"error":{"message":"bad continueUri: https:\/\/app.example.test\/oauth\/callback"}}"#.to_string(),
            unwanted: "app.example.test",
            want_intact: "bad continueUri: ",
        },
        SecretCase {
            name: "an acting user whose identifier contains a backslash",
            user: r"DOMAIN\alice",
            continue_uri: "",
            body: r#"{"error":{"message":"invalid userId: DOMAIN\\alice"}}"#.to_string(),
            unwanted: r"DOMAIN\\alice",
            want_intact: "invalid userId: ",
        },
        SecretCase {
            name: "one identifier the decode reveals, one it destroys",
            user: r"alice\/bob",
            continue_uri: "https://app.example.test/finish",
            body: r#"{"error":{"message":"bad user alice\/bob, bad continueUri https:\/\/app.example.test\/finish"}}"#.to_string(),
            unwanted: "alice/bob",
            want_intact: "withheld",
        },
        SecretCase {
            name: "a one-character acting user",
            user: "e",
            continue_uri: "",
            body: format!(r#"{{"error":{{"message":"{}"}}}}"#, "e".repeat(800)),
            unwanted: "",
            want_intact: "",
        },
    ];
    for tc in cases {
        let body = tc.body.clone();
        let srv = TestServer::start(move |_| {
            let body = body.clone();
            async move { Reply::status(400, body) }
        })
        .await;

        let p = provider_for(
            &srv,
            ProviderScheme {
                name: TEST_RESOURCE.to_string(),
                continue_uri: tc.continue_uri.to_string(),
                ..ProviderScheme::default()
            },
        );
        let err = p
            .credential(&adk_context(tc.user))
            .await
            .expect_err("Credential() = a credential, want the service failure");
        let api = find_api_error(&err).unwrap_or_else(|| {
            panic!(
                "{}: Credential() error = {err}, want a wrapped ApiError",
                tc.name
            )
        });
        let body = api.body.to_lowercase();
        if !tc.unwanted.is_empty() {
            assert!(
                !body.contains(&tc.unwanted.to_lowercase()),
                "{}: ApiError.body = {:?}, want {:?} redacted out of it",
                tc.name,
                api.body,
                tc.unwanted
            );
        }
        // Go's utf8.ValidString check holds by construction: `body` is a `String`.
        if !tc.want_intact.is_empty() {
            assert!(
                body.contains(&tc.want_intact.to_lowercase()),
                "{}: ApiError.body = {:?} lost {:?}, the wording around the redaction",
                tc.name,
                api.body,
                tc.want_intact
            );
        }
        assert!(
            !api.body.contains("[r[redacted]"),
            "{}: ApiError.body = {:?} has a marker nested inside a marker",
            tc.name,
            api.body
        );
        // maxErrorBody, doubled to leave room for the marker and the ellipsis.
        const CAP: usize = 1024;
        assert!(
            api.body.len() <= 2 * CAP,
            "{}: ApiError.body is {} bytes, want it capped near {CAP}",
            tc.name,
            api.body.len()
        );
    }
}

#[tokio::test]
async fn test_provider_caches_credential() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;

    // Default (in-memory) store; two resolves for the same app+user+resource.
    let p = provider_for(&e, scheme(TEST_RESOURCE));
    let mut got = Vec::new();
    for i in 0..2 {
        let cred = p
            .credential(&adk_context("user-1"))
            .await
            .unwrap_or_else(|err| panic!("call {i}: Credential() error = {err}"));
        got.push(bearer_token(&cred));
    }
    assert_eq!(
        e.calls(),
        1,
        "service calls = {}, want 1 (second resolve should hit the cache)",
        e.calls()
    );
    require_served(&e, &got);
}

/// One resolve of `test_provider_cache_dimensions`: which caller identity mints, for which
/// end user, under which app, for which scheme.
#[derive(Debug, Clone)]
struct Resolve {
    who: String,
    app: String,
    user: String,
    scheme: ProviderScheme,
}

// TestProviderCacheDimensions: two resolves differing in exactly one thing, through one shared
// store, both reach the service and each gets its own token.
#[tokio::test]
async fn test_provider_cache_dimensions() {
    const RES2: &str = "projects/p/locations/l/authProviders/other";
    let base = Resolve {
        who: "sa-alpha".to_string(),
        app: "app".to_string(),
        user: "user-1".to_string(),
        scheme: ProviderScheme {
            name: TEST_RESOURCE.to_string(),
            scopes: vec!["drive".to_string()],
            ..ProviderScheme::default()
        },
    };
    let with = |f: &dyn Fn(&mut Resolve)| {
        let mut r = base.clone();
        f(&mut r);
        r
    };
    let strings = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    let cases: [(&str, Option<Resolve>, Resolve); 8] = [
        ("end user", None, with(&|r| r.user = "user-2".to_string())),
        ("app", None, with(&|r| r.app = "other-app".to_string())),
        (
            "caller identity",
            None,
            with(&|r| r.who = "sa-beta".to_string()),
        ),
        (
            "resource",
            None,
            with(&|r| r.scheme.name = RES2.to_string()),
        ),
        (
            "scopes",
            None,
            with(&|r| r.scheme.scopes = strings(&["drive.readonly"])),
        ),
        (
            "continue URI",
            None,
            with(&|r| r.scheme.continue_uri = "https://example.com/finish".to_string()),
        ),
        // The two pairs below collide under a delimiter-joined encoding.
        (
            "one scope holding the scope separator",
            Some(with(&|r| r.scheme.scopes = strings(&["a,b"]))),
            with(&|r| r.scheme.scopes = strings(&["a", "b"])),
        ),
        (
            "field separator shifted between scope and continue URI",
            Some(with(&|r| {
                r.scheme.scopes = strings(&["x"]);
                r.scheme.continue_uri = "y|z".to_string();
            })),
            with(&|r| {
                r.scheme.scopes = strings(&["x|y"]);
                r.scheme.continue_uri = "z".to_string();
            }),
        ),
    ];
    for (name, first, second) in cases {
        let first = first.unwrap_or_else(|| base.clone());
        // The pair is symmetric, so also run it reversed.
        for order in [
            [first.clone(), second.clone()],
            [second.clone(), first.clone()],
        ] {
            let e = new_echo_server("2999-01-01T00:00:00Z").await;
            let store = Arc::new(InMemoryCredentialStore::new());
            let mut clients = HashMap::new();
            for r in &order {
                let p = new_sharing_provider(&e, &store, &mut clients, &r.who, r.scheme.clone());
                let cred = p
                    .credential(&adk_context_in(&r.app, &r.user))
                    .await
                    .unwrap_or_else(|err| panic!("{name}: Credential() error = {err}"));
                let want = EchoRequest {
                    path: format!("/v1/{}/credentials:retrieve", r.scheme.name),
                    user_id: r.user.clone(),
                    scopes: r.scheme.scopes.clone(),
                    continue_uri: r.scheme.continue_uri.clone(),
                    caller: r.who.clone(),
                }
                .token();
                assert_eq!(
                    bearer(&cred),
                    Some(want.clone()),
                    "{name}: {r:?} was served {cred:?}, want bearer {want:?}"
                );
            }
            assert_eq!(
                e.calls(),
                2,
                "{name}: service calls = {}, want 2 (the second resolve must not reuse the first entry)",
                e.calls()
            );
        }
    }
}

// The same resolve twice through a shared store is one call.
#[tokio::test]
async fn test_provider_cache_hits_across_providers() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let store = Arc::new(InMemoryCredentialStore::new());
    let mut clients = HashMap::new();
    let scheme = ProviderScheme {
        name: TEST_RESOURCE.to_string(),
        scopes: vec!["drive".to_string()],
        ..ProviderScheme::default()
    };
    let mut got = Vec::new();
    for _ in 0..2 {
        let p = new_sharing_provider(&e, &store, &mut clients, "sa-alpha", scheme.clone());
        let cred = p
            .credential(&adk_context("user-1"))
            .await
            .expect("Credential()");
        got.push(bearer_token(&cred));
    }
    assert_eq!(
        e.calls(),
        1,
        "service calls = {}, want 1 (a second provider with the same client and scheme should hit the entry)",
        e.calls()
    );
    require_served(&e, &got);
}

// Scope order is the caller's, not a cache dimension.
#[tokio::test]
async fn test_provider_cache_ignores_scope_order() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let store = Arc::new(InMemoryCredentialStore::new());
    let mut clients = HashMap::new();
    let mut got = Vec::new();
    for scopes in [["a", "b"], ["b", "a"]] {
        let p = new_sharing_provider(
            &e,
            &store,
            &mut clients,
            "sa-alpha",
            ProviderScheme {
                name: TEST_RESOURCE.to_string(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                ..ProviderScheme::default()
            },
        );
        let cred = p
            .credential(&adk_context("user-1"))
            .await
            .expect("Credential()");
        got.push(bearer_token(&cred));
    }
    assert_eq!(
        e.calls(),
        1,
        "service calls = {}, want 1 (reordered scopes are the same credential)",
        e.calls()
    );
    require_served(&e, &got);
}

// TestProviderStoreDegradesRatherThanFails pins that a store which misbehaves costs a round
// trip and nothing else.
// parity: adapted: the "a hit carries no credential" row cannot be expressed (get returns Option<Arc<dyn Credential>>); the read-fails and write-fails rows are ported
#[tokio::test]
async fn test_provider_store_degrades_rather_than_fails() {
    let rows: [(&str, RecordingStore); 2] = [
        (
            "the read fails",
            RecordingStore {
                get_err: Some("backend unreachable"),
                ..RecordingStore::default()
            },
        ),
        (
            "the write fails",
            RecordingStore {
                set_err: Some("disk on fire"),
                ..RecordingStore::default()
            },
        ),
    ];
    for (name, store) in rows {
        let e = new_echo_server("2999-01-01T00:00:00Z").await;
        let store = Arc::new(store);
        let p = new_store_provider(&e, store.clone());

        if let Err(err) = p.credential(&adk_context("user-1")).await {
            panic!("{name}: Credential() error = {err}; a broken store must not fail auth");
        }
        let gets = store.seen.lock().gets;
        assert_eq!(
            gets, 1,
            "{name}: store gets = {gets}, want 1 (the configured store must be consulted)"
        );
    }
}

// parity: skipped TestProviderDiscardsAHitReportedWithAnError: get returns a Result, which cannot carry a hit and an error together

// With Store unset, each provider gets a store of its own.
#[tokio::test]
async fn test_provider_default_store_is_private() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let client = agent_identity_client(&e);
    for _ in 0..2 {
        let p = provider_with(scheme(TEST_RESOURCE), Arc::clone(&client), None);
        p.credential(&adk_context("user-1"))
            .await
            .expect("Credential()");
    }
    assert_eq!(
        e.calls(),
        2,
        "service calls = {}, want 2 (providers without a Store must not share one)",
        e.calls()
    );
}

// The configured store is written to, under a key whose app and user land in their own fields,
// and the write runs on the request path.
// parity: adapted: Go checks that Set got a cancellable context; store methods take no context, so the test checks that Set ran on the requesting task itself (inline, so dropping the request drops the write)
#[tokio::test]
async fn test_provider_store_writes_the_key_it_read() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let store = Arc::new(RecordingStore::default());
    let p = new_store_provider(&e, store.clone());

    let caller = tokio::spawn(async move {
        let id = tokio::task::id();
        p.credential(&adk_context("user-1")).await.map(|_| id)
    })
    .await
    .expect("request task")
    .expect("Credential()");

    let seen = store.seen.lock();
    assert!(
        seen.gets == 1 && seen.sets == 1,
        "store gets/sets = {}/{}, want 1/1 (the configured store must be used)",
        seen.gets,
        seen.sets
    );
    assert!(
        seen.last_key.app_name == "app" && seen.last_key.user_id == "user-1",
        "store key = {:?}, want app_name \"app\" and user_id \"user-1\"",
        seen.last_key
    );
    assert!(!seen.last_key.key.is_empty(), "store key has an empty slot");
    assert_eq!(
        seen.set_task,
        Some(caller),
        "Set() ran off the requesting task, so the request cannot bound it"
    );
}

// The key the provider writes under is the one Client::cache_key names.
#[tokio::test]
async fn test_client_cache_key_matches_what_the_provider_wrote() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let client = agent_identity_client(&e);
    let scheme = ProviderScheme {
        name: TEST_RESOURCE.to_string(),
        scopes: vec!["drive".to_string()],
        ..ProviderScheme::default()
    };
    let store = Arc::new(InMemoryCredentialStore::new());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let p = provider_with(scheme.clone(), Arc::clone(&client), Some(dyn_store));
    p.credential(&adk_context("user-1"))
        .await
        .expect("Credential()");

    // A session other than the one that resolved: the entry is per user, not per session.
    let key = client.cache_key(
        &scheme,
        &Identity {
            app_name: "app".to_string(),
            user_id: "user-1".to_string(),
            session_id: "another-session".to_string(),
        },
    );
    assert!(
        is_cached(store.as_ref(), &key).await,
        "Client::cache_key() names no cached entry, so a caller cannot invalidate one"
    );
    store.delete(&key).await.expect("Delete()");
    p.credential(&adk_context("user-1"))
        .await
        .expect("Credential() after Delete");
    assert_eq!(
        e.calls(),
        2,
        "service calls = {}, want 2 (Delete must actually invalidate)",
        e.calls()
    );
}

// Two Clients are two cache dimensions even when built from one config.
#[tokio::test]
async fn test_provider_cache_separates_client_instances() {
    let e = new_echo_server("2999-01-01T00:00:00Z").await;
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
    for _ in 0..2 {
        let p = provider_with(
            scheme(TEST_RESOURCE),
            agent_identity_client(&e),
            Some(Arc::clone(&store)),
        );
        p.credential(&adk_context("user-1"))
            .await
            .expect("Credential()");
    }
    assert_eq!(
        e.calls(),
        2,
        "service calls = {}, want 2 (a second Client must not read the first one's entries)",
        e.calls()
    );
}

/// How much life the service reports in `test_provider_cached_expiry`.
enum Left {
    /// From now.
    For(TimeDelta),
    /// Sent verbatim ("" for no expiry, or unparseable text).
    Raw(&'static str),
}

// TestProviderCachedExpiry pins what lifetime the provider is willing to cache for, by asking
// the store what it was handed. Expiries are relative to the wall clock.
#[tokio::test]
async fn test_provider_cached_expiry() {
    // Go `maxCachedLifetimeForTest`: duplicated rather than imported, because the cap is a
    // policy the test should notice changing.
    let max_cached_lifetime_for_test = TimeDelta::hours(1);
    // (name, left, want lifetime or None for no cache write, clamped)
    let rows: [(&str, Left, Option<TimeDelta>, bool); 9] = [
        (
            "honored as reported",
            Left::For(TimeDelta::minutes(5)),
            Some(TimeDelta::minutes(5)),
            false,
        ),
        (
            "a minute of life left",
            Left::For(TimeDelta::minutes(1)),
            Some(TimeDelta::minutes(1)),
            false,
        ),
        (
            "twice the store's margin",
            Left::For(TimeDelta::seconds(20)),
            Some(TimeDelta::seconds(20)),
            false,
        ),
        (
            "clamped to the cap",
            Left::For(TimeDelta::hours(8760)),
            Some(max_cached_lifetime_for_test),
            true,
        ),
        (
            "less left than the store's margin",
            Left::For(TimeDelta::seconds(5)),
            None,
            false,
        ),
        (
            "exactly the store's margin",
            Left::For(TimeDelta::seconds(10)),
            None,
            false,
        ),
        ("already past", Left::For(TimeDelta::hours(-1)), None, false),
        ("absent", Left::Raw(""), None, false),
        ("unparseable", Left::Raw("garbage"), None, false),
    ];
    for (name, left, want, clamped) in rows {
        let issued = Utc::now();
        let expire_time = match left {
            Left::For(d) => (issued + d).to_rfc3339_opts(SecondsFormat::Nanos, true),
            Left::Raw(s) => s.to_string(),
        };
        let e = new_echo_server(&expire_time).await;
        let store = Arc::new(RecordingStore::default());
        let p = new_store_provider(&e, store.clone());

        let before = Utc::now();
        p.credential(&adk_context("user-1"))
            .await
            .unwrap_or_else(|err| panic!("{name}: Credential() error = {err}"));
        let after = Utc::now();

        let seen = store.seen.lock();
        let Some(want) = want else {
            assert_eq!(
                seen.sets, 0,
                "{name}: store Set called with expiry {:?}, want no cache write for expireTime {expire_time:?}",
                seen.last_expires
            );
            continue;
        };
        assert_eq!(
            seen.sets, 1,
            "{name}: store Set calls = {}, want 1",
            seen.sets
        );
        let got = seen.last_expires.expect("an expiry was written");
        if !clamped {
            // Passed through untouched, so this is exact.
            let want = DateTime::parse_from_rfc3339(&expire_time)
                .expect("parse expireTime")
                .with_timezone(&Utc);
            assert_eq!(
                got, want,
                "{name}: cached until {got}, want the service's own {want}"
            );
            continue;
        }
        // Clamped to the cap measured from the provider's own clock read, which happens
        // somewhere between these two.
        let (lo, hi) = (before + want, after + want);
        assert!(
            got >= lo && got <= hi,
            "{name}: cached until {got}, want {want} from now (between {lo} and {hi})"
        );
    }
}

// A non-bearer credential is wrapped for the extra X-Goog-Api-Key header, so the refresh must
// read the token through the wrapper.
#[tokio::test]
async fn test_provider_refresh_reads_through_a_wrapped_credential() {
    let f = refresh_fixture("X-Goog-Api-Key").await;
    let cx = adk_context("user-1");

    let first = f.p.credential(&cx).await.expect("Credential()");
    assert!(
        bearer(&first).is_none(),
        "the fixture produced a bearer credential; this case is about the wrapped one"
    );
    f.rp().refresh(&cx, &first).await.expect("Refresh()");
    let (calls, hint) = f.seen();
    assert_eq!(calls, 2, "service calls = {calls}, want 2");
    assert_eq!(
        hint, "tok1",
        "forceRefreshToken = {hint:?}, want \"tok1\", the token inside the wrapper"
    );
}

// A downstream that rejects everything must not set the rate of forced refreshes.
#[tokio::test]
async fn test_provider_refresh_is_rate_limited() {
    let f = refresh_fixture("Authorization: Bearer").await;
    let cx = adk_context("user-1");

    let first = f.p.credential(&cx).await.expect("Credential()");
    let fresh = f.rp().refresh(&cx, &first).await.expect("Refresh()");
    let (calls, _) = f.seen();
    assert_eq!(
        calls, 2,
        "service calls = {calls}, want 2 (the cold resolve and one refresh)"
    );

    // A second forced refresh inside the cooldown is refused rather than served.
    let err = f
        .rp()
        .refresh(&cx, &fresh)
        .await
        .expect_err("Refresh() twice inside the cooldown = a credential, want it refused");
    assert_eq!(
        err.to_string(),
        "gcp: refusing to force-refresh projects/p/locations/l/authProviders/ap twice inside 30s"
    );
    let (calls, _) = f.seen();
    assert_eq!(
        calls, 2,
        "service calls = {calls}, want 2 (the refused refresh must not reach the service)"
    );
    // Refused or not, the rejected credential is gone.
    let key = key_for(&f.client, &scheme(TEST_RESOURCE), "user-1");
    assert!(
        !is_cached(f.store.as_ref(), &key).await,
        "the rejected credential is still cached after a refused refresh"
    );
}

// Another request may have refreshed already: a cached credential that is not the rejected
// one is served instead of minting again.
#[tokio::test]
async fn test_provider_refresh_serves_a_credential_somebody_else_already_fetched() {
    let f = refresh_fixture("Authorization: Bearer").await;
    let cx = adk_context("user-1");

    let first = f.p.credential(&cx).await.expect("Credential()");
    let fresh = f.rp().refresh(&cx, &first).await.expect("Refresh()");
    let (calls, _) = f.seen();
    assert_eq!(calls, 2, "service calls = {calls}, want 2");

    // A straggler still holding the first credential asks again.
    let got = f
        .rp()
        .refresh(&cx, &first)
        .await
        .expect("Refresh() for a straggler");
    assert_eq!(
        bearer(&got),
        bearer(&fresh),
        "straggler was served {got:?}, want the credential already fetched, {fresh:?}"
    );
    let (calls, _) = f.seen();
    assert_eq!(
        calls, 2,
        "service calls = {calls}, want 2 (the straggler must not mint again)"
    );
}

// A refresh whose replacement cannot be cached, and a refresh that fails, both leave the
// rejected credential gone.
#[tokio::test]
async fn test_provider_refresh_evicts_what_it_cannot_replace() {
    #[allow(clippy::type_complexity)]
    let rows: [(&str, fn() -> Reply, bool); 2] = [
        (
            "the replacement has no usable lifetime",
            || Reply::ok(r#"{"success":{"token":"tok2","header":"Authorization: Bearer"}}"#),
            false,
        ),
        ("the refresh fails", || Reply::status(503, ""), true),
    ];
    for (name, refresh, want_err) in rows {
        let calls = Arc::new(AtomicUsize::new(0));
        let srv = TestServer::start(move |_| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let reply = if n == 1 {
                Reply::ok(
                    json!({"success": {
                        "token": "tok1",
                        "header": "Authorization: Bearer",
                        "expireTime": in_an_hour(),
                    }})
                    .to_string(),
                )
            } else {
                refresh()
            };
            async move { reply }
        })
        .await;

        let client = agent_identity_client(&srv);
        let store = Arc::new(InMemoryCredentialStore::new());
        let dyn_store: Arc<dyn CredentialStore> = store.clone();
        let p = provider_with(scheme(TEST_RESOURCE), Arc::clone(&client), Some(dyn_store));
        let cx = adk_context("user-1");
        let first = p.credential(&cx).await.expect("Credential()");
        let key = key_for(&client, &scheme(TEST_RESOURCE), "user-1");
        assert!(
            is_cached(store.as_ref(), &key).await,
            "{name}: the first credential was not cached, so this case proves nothing"
        );

        let res = refreshing(&p).refresh(&cx, &first).await;
        assert_eq!(
            res.is_err(),
            want_err,
            "{name}: Refresh() = {res:?}, want_err {want_err}"
        );
        assert!(
            !is_cached(store.as_ref(), &key).await,
            "{name}: the rejected credential is still cached, so the next request is served it again"
        );
    }
}

// A service that hands back the credential it already had has not refreshed anything.
#[tokio::test]
async fn test_provider_refresh_rejects_the_same_credential_coming_back() {
    // The same token every time, whatever the request asked for.
    let srv = TestServer::start(|_| {
        let body = json!({"success": {
            "token": "tok1",
            "header": "Authorization: Bearer",
            "expireTime": in_an_hour(),
        }})
        .to_string();
        async move { Reply::ok(body) }
    })
    .await;

    let client = agent_identity_client(&srv);
    let store = Arc::new(InMemoryCredentialStore::new());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let p = provider_with(scheme(TEST_RESOURCE), Arc::clone(&client), Some(dyn_store));
    let cx = adk_context("user-1");
    let first = p.credential(&cx).await.expect("Credential()");

    let res = refreshing(&p).refresh(&cx, &first).await;
    let Err(err) = res else {
        panic!(
            "Refresh() = {res:?}; want it refused when the service returns the rejected credential"
        );
    };
    assert_eq!(
        err.to_string(),
        r#"gcp: resource "projects/p/locations/l/authProviders/ap": the credential service returned the credential that was rejected"#
    );
    let key = key_for(&client, &scheme(TEST_RESOURCE), "user-1");
    assert!(
        !is_cached(store.as_ref(), &key).await,
        "the rejected credential is still cached, so the next request is served it again"
    );
}

// The cooldown protects a credential at the service, identified by the end user and the
// scheme, not by the app name.
#[tokio::test]
async fn test_provider_refresh_cooldown_ignores_the_app_name() {
    let f = refresh_fixture("Authorization: Bearer").await;

    let first =
        f.p.credential(&adk_context_in("app-one", "user-1"))
            .await
            .expect("Credential()");
    f.rp()
        .refresh(&adk_context_in("app-one", "user-1"), &first)
        .await
        .expect("Refresh()");
    let (calls_after_first, _) = f.seen();

    // A second app, same end user and scheme: one credential at the service.
    let second =
        f.p.credential(&adk_context_in("app-two", "user-1"))
            .await
            .expect("Credential() for the second app");
    assert!(
        f.rp()
            .refresh(&adk_context_in("app-two", "user-1"), &second)
            .await
            .is_err(),
        "Refresh() under a second app name = a credential, want the cooldown to hold"
    );
    let (calls, _) = f.seen();
    assert_eq!(
        calls,
        calls_after_first + 1,
        "service calls = {calls}, want {} (only the second app's cold resolve, not another forced refresh)",
        calls_after_first + 1
    );
}

// Eviction must not delete an entry another request replaced in the meantime.
#[tokio::test]
async fn test_provider_refresh_does_not_evict_a_credential_somebody_else_replaced() {
    // A new token each time, so the same-token guard never trips here.
    let calls = Arc::new(AtomicUsize::new(0));
    let srv = TestServer::start(move |_| {
        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"success": {
            "token": format!("tok{n}"),
            "header": "Authorization: Bearer",
            "expireTime": in_an_hour(),
        }})
        .to_string();
        async move { Reply::ok(body) }
    })
    .await;

    let store = Arc::new(SwappingStore::default());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let p = provider_with(
        scheme(TEST_RESOURCE),
        agent_identity_client(&srv),
        Some(dyn_store),
    );
    let rp = refreshing(&p);
    let cx = adk_context("user-1");

    let rejected = p.credential(&cx).await.expect("Credential()");
    // Burn the cooldown, so the Refresh below takes the throttled path.
    rp.refresh(&cx, &rejected).await.expect("Refresh()");

    // One read answers with the rejected credential; every read after that answers with a
    // replacement, as if another request had landed in between.
    let before = {
        let mut s = store.swap.lock();
        s.pinned = Some(Arc::clone(&rejected));
        s.swapped = Some(Arc::new(Bearer {
            token: "replaced-by-somebody-else".to_string(),
        }));
        s.armed = 1;
        s.deletes
    };

    assert!(
        rp.refresh(&cx, &rejected).await.is_err(),
        "Refresh() inside the cooldown = a credential, want it refused"
    );
    assert_eq!(
        store.swap.lock().deletes,
        before,
        "Refresh deleted an entry another request had already replaced"
    );
}

async fn cold_resolve_then_refresh(f: &RefreshFixture, user: &str) -> Result<(), Error> {
    let cx = adk_context(user);
    let cred =
        f.p.credential(&cx)
            .await
            .unwrap_or_else(|err| panic!("Credential({user:?}) error = {err}"));
    f.rp().refresh(&cx, &cred).await.map(|_| ())
}

// The cooldown must separate end users.
#[tokio::test]
async fn test_provider_refresh_cooldown_separates_users() {
    let f = refresh_fixture("Authorization: Bearer").await;

    if let Err(err) = cold_resolve_then_refresh(&f, "user-1").await {
        panic!("Refresh() for the first user error = {err}");
    }
    if let Err(err) = cold_resolve_then_refresh(&f, "user-2").await {
        panic!(
            "Refresh() for a second user error = {err}; the first user's cooldown must not bind them"
        );
    }
    // Two cold resolves and two forced refreshes.
    let (calls, _) = f.seen();
    assert_eq!(calls, 4, "service calls = {calls}, want 4");
}

// A rejected credential whose token cannot be read: Refresh refuses and drops the entry.
// parity: adapted: Go's pointerStore reads back *auth.BearerCredential; the Rust store reads back PointerBearer, a test-local credential type credential_token cannot read
#[tokio::test]
async fn test_provider_refresh_fails_closed_on_an_unreadable_token() {
    let (srv, state) = replacing_server(None).await;
    let store = Arc::new(PointerStore::default());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let (p, client) = replacing_provider(&srv, dyn_store);
    let rp = refreshing(&p);
    let cx = adk_context("user-1");

    p.credential(&cx).await.expect("Credential()");
    let rejected = p.credential(&cx).await.expect("Credential()");
    assert!(
        rejected.downcast_ref::<PointerBearer>().is_some(),
        "Credential() = {rejected:?}, want the store's PointerBearer"
    );

    let res = rp.refresh(&cx, &rejected).await;
    let Err(err) = res else {
        panic!("Refresh() = {res:?}; want a refusal");
    };
    assert_eq!(
        err.to_string(),
        r#"gcp: resource "projects/p/locations/l/authProviders/ap": cannot read the rejected credential's token, so it cannot be force-refreshed"#
    );
    let (calls, _) = replacing_seen(&srv, &state);
    assert_eq!(
        calls, 1,
        "service calls = {calls}, want 1: an unforced retrieval cannot replace the rejected credential"
    );
    let key = key_for(&client, &scheme(TEST_RESOURCE), "user-1");
    assert!(
        !is_cached(store.as_ref(), &key).await,
        "the rejected credential is still cached"
    );
}

// An unreadable cached entry cannot be shown to differ from the rejected one, so it is not
// served as a replacement: Refresh forces instead.
// parity: adapted: the store reads back PointerBearer, a test-local credential type credential_token cannot read, where Go reads back *auth.BearerCredential
#[tokio::test]
async fn test_provider_refresh_does_not_serve_an_unreadable_cached_entry() {
    let (srv, state) = replacing_server(None).await;
    let (p, _client) = replacing_provider(&srv, Arc::new(PointerStore::default()));
    let rp = refreshing(&p);
    let cx = adk_context("user-1");

    let rejected = p.credential(&cx).await.expect("Credential()");
    assert!(
        bearer(&rejected).is_some(),
        "Credential() on a cold miss = {rejected:?}, want the fetched Bearer"
    );

    let got = rp.refresh(&cx, &rejected).await.expect("Refresh()");
    assert_eq!(
        bearer(&got).as_deref(),
        Some("tok2"),
        "Refresh() = {got:?}, want the replacement tok2"
    );
    let (_, forced) = replacing_seen(&srv, &state);
    assert_eq!(forced, 1, "forced refreshes = {forced}, want 1");
}

// The same reading at eviction: an unreadable cached entry counts as possibly the rejected
// one, so the throttled path drops it.
// parity: adapted: the store reads back PointerBearer, a test-local credential type credential_token cannot read, where Go reads back *auth.BearerCredential
#[tokio::test]
async fn test_provider_refresh_evicts_an_unreadable_cached_entry() {
    let (srv, _state) = replacing_server(None).await;
    let store = Arc::new(PointerStore::default());
    let dyn_store: Arc<dyn CredentialStore> = store.clone();
    let (p, client) = replacing_provider(&srv, dyn_store);
    let rp = refreshing(&p);
    let cx = adk_context("user-1");

    let first = p.credential(&cx).await.expect("Credential()");
    // Burns the cooldown and caches tok2, which the store reads back as a PointerBearer.
    let fresh = rp.refresh(&cx, &first).await.expect("Refresh()");
    assert!(
        rp.refresh(&cx, &fresh).await.is_err(),
        "Refresh() inside the cooldown = a credential, want it refused"
    );
    let key = key_for(&client, &scheme(TEST_RESOURCE), "user-1");
    assert!(
        !is_cached(store.as_ref(), &key).await,
        "the rejected credential is still cached after a refused refresh"
    );
}

// parity: skipped TestProviderRefreshOnADoneContextKeepsTheCooldown: Rust has no done-context state; a dropped refresh future returns nothing

// A burst of requests rejected together reaches the service with one forced refresh. The
// winner is held at the service until every other Refresh has returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_provider_refresh_concurrent_forces_once() {
    const N: usize = 8;
    let (release, hold) = watch::channel(false);
    let (srv, state) = replacing_server(Some(hold)).await;
    let (p, _client) = replacing_provider(&srv, Arc::new(InMemoryCredentialStore::new()));
    let cx = adk_context("user-1");

    let rejected = p.credential(&cx).await.expect("Credential()");

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..N {
        let (p, cx, rejected) = (Arc::clone(&p), cx.clone(), Arc::clone(&rejected));
        set.spawn(async move { refreshing(&p).refresh(&cx, &rejected).await });
    }
    let mut got = Vec::with_capacity(N);
    let all_but_one = tokio::time::timeout(Duration::from_secs(5), async {
        while got.len() < N - 1 {
            got.push(
                set.join_next()
                    .await
                    .expect("a refresh task")
                    .expect("refresh task"),
            );
        }
    })
    .await;
    if all_but_one.is_err() {
        release.send_replace(true);
        panic!(
            "{} of {} refreshes returned while one was held at the service, so more than one reached it",
            got.len(),
            N - 1
        );
    }
    release.send_replace(true);
    got.push(
        set.join_next()
            .await
            .expect("the held refresh")
            .expect("refresh task"),
    );

    let (_, forced) = replacing_seen(&srv, &state);
    assert_eq!(forced, 1, "forced retrievals = {forced}, want 1");
    let mut replaced = 0;
    for r in &got {
        let Ok(cred) = r else { continue };
        assert_eq!(
            bearer(cred).as_deref(),
            Some("tok2"),
            "Refresh() = {cred:?}, want the replacement tok2 or an error"
        );
        replaced += 1;
    }
    assert_eq!(
        replaced, 1,
        "{replaced} refreshes returned the replacement, want exactly the one that fetched it"
    );
    // The losers' evictions ran before the replacement was written, so it stands.
    let (before, _) = replacing_seen(&srv, &state);
    let cred = p.credential(&cx).await.expect("Credential()");
    assert_eq!(
        bearer(&cred).as_deref(),
        Some("tok2"),
        "Credential() = {cred:?}; want the cached replacement tok2"
    );
    let (after, _) = replacing_seen(&srv, &state);
    assert_eq!(
        after, before,
        "service calls went {before} -> {after}; the replacement should have been served from the cache"
    );
}

// The end-to-end path: a downstream 401 reaching a real provider through a real Transport, the
// rejected credential surviving the trip readable, and the retry going out with the
// replacement.
#[tokio::test]
async fn test_transport_refreshes_through_the_real_provider() {
    let mints = Arc::new(AtomicUsize::new(0));
    let cred_srv = TestServer::start(move |_| {
        let n = mints.fetch_add(1, Ordering::SeqCst) + 1;
        let body = json!({"success": {
            "token": format!("tok{n}"),
            "header": "Authorization: Bearer",
            "expireTime": in_an_hour(),
        }})
        .to_string();
        async move { Reply::ok(body) }
    })
    .await;

    let p = provider_with(
        scheme(TEST_RESOURCE),
        agent_identity_client(&cred_srv),
        Some(Arc::new(InMemoryCredentialStore::new())),
    );

    // The downstream refuses the first credential and accepts the second.
    let hits = Arc::new(AtomicUsize::new(0));
    let downstream = TestServer::start(move |_| {
        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
        let reply = if n == 1 {
            Reply::status(401, "no")
        } else {
            Reply::ok("ok")
        };
        async move { reply }
    })
    .await;

    let hc = gadk_auth::client(downstream.client(), p, adk_context("user-1"));
    let resp = hc.get(downstream.url()).send().await.expect("Do()");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "status = {}, want 200 after the refresh and retry",
        resp.status()
    );

    let got_auth: Vec<String> = downstream
        .requests()
        .iter()
        .map(|r| r.header("authorization").unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        got_auth,
        ["Bearer tok1", "Bearer tok2"],
        "downstream saw {got_auth:?}"
    );
    // The second retrieval carries the rejected token: the point of passing it through the
    // Transport rather than re-reading the cache.
    let got_hints: Vec<String> = cred_srv
        .requests()
        .iter()
        .map(force_refresh_token)
        .collect();
    assert_eq!(
        got_hints,
        ["", "tok1"],
        "credential service saw forceRefreshToken {got_hints:?}"
    );
}

// parity: skipped TestProviderRefreshEvictsOnADetachedContext: no caller context exists to inherit cancellation from; drop_entry runs on a spawned task regardless
// parity: skipped TestProviderRefreshDropsAnUnreadableTokenOnADetachedContext: no caller context exists to inherit cancellation from; drop_entry runs on a spawned task regardless
