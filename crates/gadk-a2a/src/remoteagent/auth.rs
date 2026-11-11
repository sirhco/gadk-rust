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

//! Credential scoping and application for A2A calls (port of ADK-Go
//! `agent/remoteagent/v2/auth.go` and `internal/agent/remoteagent/auth_scope.go`).
//!
//! Go carries the scope, the per-invocation credential cell and the card-fetch client as
//! `context.Context` values and applies the credential in an `http.RoundTripper`. Here the
//! [`AUTH_SCOPE`] task-local carries them (plan D3), and the credential is applied by
//! [`AuthInterceptor`] (every A2A call, through the a2a-rs client's `ServiceParams`) and by
//! [`apply_credential`] (the agent-card fetch). The single-flight groups Go keeps on its
//! `authTransport` (one per remote agent) are kept per credential provider value.

// The card provider (plan 5b Task 6), the remote agent (Task 8) and the server cleanup (Task 7)
// consume these items; Task 9 removes this allowance once they do.
#![allow(dead_code)]

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::FutureExt as _;
use gadk_auth::{AuthContext, Credential, CredentialProvider, SharedError, TokenSource};
use gadk_platform::{go_query_escape, go_quote};
use http::HeaderMap;
use http::header::{AUTHORIZATION, HeaderValue};
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::time::Instant;
use url::Url;

use crate::Error;

/// Go `a2aclient.SessionID` as built by `CredentialScope`: the credential cache key of an
/// (app, user, session, agent) call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialScope(pub String);

/// Go `CredentialScope(appName, userID, sessionID, agentName)`: the four parts, each
/// percent-encoded with Go's `url.QueryEscape`, joined with `/`. No two identities produce the
/// same key; a provider that wants the parts back splits on `/` and query-unescapes each.
pub fn credential_scope(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    agent_name: &str,
) -> CredentialScope {
    CredentialScope(format!(
        "{}/{}/{}/{}",
        go_query_escape(app_name),
        go_query_escape(user_id),
        go_query_escape(session_id),
        go_query_escape(agent_name)
    ))
}

/// The credential scope of the current call (Go `a2aclient.SessionIDFrom(ctx)`), for custom
/// credential providers; `None` outside a scoped call.
pub fn current_credential_scope() -> Option<CredentialScope> {
    AUTH_SCOPE.try_with(|id| id.scope.clone()).ok()
}

/// What the auth scope carries for one remote call.
#[derive(Clone)]
pub(crate) struct CallIdentity {
    /// The credential scope.
    pub scope: CredentialScope,
    /// The invocation identity handed to the credential provider; `None` on the server's
    /// cleanup-cancel path.
    pub identity: Option<gadk_agent::Identity>,
    /// The credential provider.
    pub auth: Arc<dyn gadk_auth::CredentialProvider>,
    /// Go `WithCardFetchClient`.
    pub card_fetch_client: Option<reqwest::Client>,
    /// The per-call credential cell (Go `credentialCell`).
    pub(crate) cell: Arc<CredentialCell>,
}

tokio::task_local! {
    /// The auth scope of the current remote call (Go `AttachAuthScope`).
    pub(crate) static AUTH_SCOPE: CallIdentity;
}

/// Runs `fut` inside the auth scope `id`; `None` runs it unscoped (Go `OwnsAuthScope == false`),
/// which leaves any scope the caller is already inside in effect.
pub(crate) async fn with_auth_scope<F: Future>(id: Option<CallIdentity>, fut: F) -> F::Output {
    match id {
        Some(id) => AUTH_SCOPE.scope(id, fut).await,
        None => fut.await,
    }
}

/// The card-fetch client of the current auth scope (Go `CardFetchClientFrom(ctx)`); `None`
/// outside a scope or when the scope carries none.
pub(crate) fn current_card_fetch_client() -> Option<reqwest::Client> {
    AUTH_SCOPE
        .try_with(|id| id.card_fetch_client.clone())
        .ok()
        .flatten()
}

/// Applies the scope's credential to every A2A call (Go `authTransport`): `before` resolves
/// the credential of the current [`AUTH_SCOPE`] and writes its headers into the call's
/// `ServiceParams`, which the a2a-rs transports send as HTTP headers. Outside a scope it does
/// nothing. A credential that cannot be resolved or applied fails the call.
pub(crate) struct AuthInterceptor;

#[async_trait::async_trait]
impl a2a_client::middleware::CallInterceptor for AuthInterceptor {
    async fn before(
        &self,
        _method: &str,
        params: &mut a2a_client::ServiceParams,
    ) -> Result<(), a2a::A2AError> {
        let mut headers = HeaderMap::new();
        apply_credential(&mut headers).await?;
        write_service_params(params, &headers);
        Ok(())
    }
}

/// Copies `headers` into `params`, each name replacing whatever values `params` held under it
/// in any letter case (Go `h[k] = v` on a canonical key).
fn write_service_params(params: &mut a2a_client::ServiceParams, headers: &HeaderMap) {
    for name in headers.keys() {
        params.retain(|key, _| !key.eq_ignore_ascii_case(name.as_str()));
        let values = headers
            .get_all(name)
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect();
        params.insert(name.as_str().to_owned(), values);
    }
}

/// Applies the credential of the current auth scope to `headers`; a no-op outside a scope.
///
/// The credential is resolved once per invocation (the scope's [`CredentialCell`]) with the
/// scope's identity as the provider's [`AuthContext`], and applied through the scope's
/// single-flight groups ([`AuthSteps::apply`]).
pub(crate) async fn apply_credential(headers: &mut HeaderMap) -> Result<(), Error> {
    let Ok(id) = AUTH_SCOPE.try_with(CallIdentity::clone) else {
        return Ok(());
    };
    let cred = resolve_credential(&id).await?;
    steps_for(&id.auth)
        .apply(Some(&id.scope), &cred, headers)
        .await
}

/// Go `authTransport.credential`: the cell's credential, else one resolved from the provider
/// and stored in the cell. A failure is not cached, so the next call tries again.
async fn resolve_credential(id: &CallIdentity) -> Result<Arc<dyn Credential>, Error> {
    if let Some(cred) = id.cell.get() {
        return Ok(cred);
    }
    let cx = AuthContext {
        identity: id.identity.clone(),
    };
    let cred = id
        .auth
        .credential(&cx)
        .await
        .map_err(|e| Error::wrap("remoteagent: resolve auth credential", e))?;
    id.cell.set(Arc::clone(&cred));
    Ok(cred)
}

/// The single-flight groups of every credential provider in use, keyed by the provider value.
/// The `Weak` pins the provider's address while the entry exists, so a later provider cannot
/// inherit another's groups.
type StepsRegistry = Vec<(Weak<dyn CredentialProvider>, Arc<AuthSteps>)>;
static STEPS: Mutex<StepsRegistry> = Mutex::new(Vec::new());

/// The groups of `provider` (Go: the `authTransport` built for one remote agent).
pub(crate) fn steps_for(provider: &Arc<dyn CredentialProvider>) -> Arc<AuthSteps> {
    let mut all = STEPS.lock();
    all.retain(|(p, _)| p.strong_count() > 0);
    if let Some((_, steps)) = all
        .iter()
        .find(|(p, _)| std::ptr::addr_eq(p.as_ptr(), Arc::as_ptr(provider)))
    {
        return Arc::clone(steps);
    }
    let steps = Arc::new(AuthSteps::new());
    all.push((Arc::downgrade(provider), Arc::clone(&steps)));
    steps
}

/// Go `authTransport`'s `mints` and `applies` groups and its `apply`.
pub(crate) struct AuthSteps {
    /// OAuth2 token mints.
    pub(crate) mints: MintGroup<String>,
    /// A credential's own `apply`.
    pub(crate) applies: MintGroup<HeaderMap>,
}

impl Default for AuthSteps {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthSteps {
    /// Fresh groups.
    pub(crate) fn new() -> Self {
        Self {
            mints: MintGroup::mints(),
            applies: MintGroup::applies(),
        }
    }

    /// Go `authTransport.apply`: writes `cred` onto `h`. A bare OAuth2 credential is minted
    /// through the mint group and sent as a bearer token; anything else (an OAuth2 credential
    /// wrapped by `with_headers` included) writes itself through its own `apply` on the apply
    /// group, into a private header map copied onto `h` only once the step has landed. Both
    /// steps are bounded by [`MINT_TIMEOUT`]. A `None` scope (no path sends one today) gets
    /// fresh groups, so it never shares a step with another request.
    pub(crate) async fn apply(
        &self,
        scope: Option<&CredentialScope>,
        cred: &Arc<dyn Credential>,
        h: &mut HeaderMap,
    ) -> Result<(), Error> {
        let fresh;
        let (steps, scope) = match scope {
            Some(scope) => (self, scope.clone()),
            None => {
                fresh = AuthSteps::new();
                (&fresh, CredentialScope(String::new()))
            }
        };

        if let Some(oauth2) = (**cred).downcast_ref::<gadk_auth::OAuth2>() {
            let token = steps
                .mints
                .token(&scope, Arc::clone(&oauth2.token_source))
                .await?;
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| Error::wrap("remoteagent: mint oauth2 token", e))?;
            value.set_sensitive(true);
            h.insert(AUTHORIZATION, value);
            return Ok(());
        }

        let cred = Arc::clone(cred);
        let written = steps
            .applies
            .run(&scope, async move {
                let mut out = HeaderMap::new();
                cred.apply(&mut out).await.map_err(|e| {
                    StepError::wrap("remoteagent: apply auth credential", redact_token_error(e))
                })?;
                Ok(out)
            })
            .await?;
        for name in written.keys() {
            h.remove(name);
        }
        for (name, value) in &written {
            h.append(name.clone(), value.clone());
        }
        Ok(())
    }
}

/// Go `a2aRequestTimeout`: the a2a client's own default, restated because the auth client is
/// built explicitly.
const A2A_REQUEST_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Go `cardFetchTimeout`: the bound a2a-go's default card resolver applies.
const CARD_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `maxRedirects`: net/http's default cap, which a custom policy replaces.
const MAX_REDIRECTS: usize = 10;

/// Go `authHTTPClient`: 3 min timeout, Go `checkRedirect` policy, at most 10 redirects.
pub(crate) fn auth_http_client() -> reqwest::Client {
    http_client(A2A_REQUEST_TIMEOUT)
}

/// Go `cardFetchHTTPClient`: the RPC client's redirect policy with a 30 s timeout.
pub(crate) fn card_fetch_http_client() -> reqwest::Client {
    http_client(CARD_FETCH_TIMEOUT)
}

fn http_client(timeout: Duration) -> reqwest::Client {
    let built = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(
            |attempt| match check_redirect(attempt.url(), attempt.previous()) {
                Ok(()) => attempt.follow(),
                Err(err) => attempt.error(err),
            },
        ))
        .build();
    match built {
        Ok(client) => client,
        Err(err) => {
            // `build` fails only when the TLS backend cannot be initialized, and
            // `Client::new` then fails on the same error (reqwest documents that it panics);
            // no client exists to return without one.
            tracing::error!(error = %err, "remoteagent: build the A2A HTTP client");
            reqwest::Client::new()
        }
    }
}

/// Go `checkRedirect`: the hop cap, then the same-credential-target check against the original
/// request and against the hop just taken (`via` is every URL already requested, oldest
/// first, as reqwest's `Attempt::previous` and Go's `via` both are).
pub(crate) fn check_redirect(next: &Url, via: &[Url]) -> Result<(), Error> {
    if via.len() >= MAX_REDIRECTS {
        return Err(Error::msg(format!(
            "remoteagent: stopped after {MAX_REDIRECTS} redirects"
        )));
    }
    for from in via.first().into_iter().chain(via.last()) {
        if !same_credential_target(from, next) {
            return Err(Error::msg(format!(
                "remoteagent: refusing redirect from {} to {}: A2AConfig.Auth is set and the credential would follow",
                request_origin(from),
                request_origin(next)
            )));
        }
    }
    Ok(())
}

/// Go `sameCredentialTarget`: the host must be identical, and the scheme may only stay the
/// same or be upgraded from http to https.
pub(crate) fn same_credential_target(from: &Url, to: &Url) -> bool {
    if !hostname(from).eq_ignore_ascii_case(hostname(to)) {
        return false;
    }
    let (fs, ts) = (from.scheme(), to.scheme());
    if fs.eq_ignore_ascii_case(ts) {
        defaulted_port(from) == defaulted_port(to)
    } else if fs.eq_ignore_ascii_case("http") && ts.eq_ignore_ascii_case("https") {
        is_default_port(from) && is_default_port(to) || from.port() == to.port()
    } else {
        false
    }
}

/// Go `url.URL.Hostname`: the host without IPv6 brackets.
fn hostname(u: &Url) -> &str {
    let h = u.host_str().unwrap_or("");
    h.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h)
}

/// Go `defaultedPort`: the explicit port, else the scheme's default (`""` for neither http
/// nor https).
fn defaulted_port(u: &Url) -> String {
    if let Some(port) = u.port() {
        return port.to_string();
    }
    scheme_default_port(u.scheme()).to_owned()
}

fn scheme_default_port(scheme: &str) -> &'static str {
    if scheme.eq_ignore_ascii_case("https") {
        "443"
    } else if scheme.eq_ignore_ascii_case("http") {
        "80"
    } else {
        ""
    }
}

/// Go `isDefaultPort`: the port is implicit or names the scheme's default.
fn is_default_port(u: &Url) -> bool {
    match u.port() {
        None => true,
        Some(port) => port.to_string() == scheme_default_port(u.scheme()),
    }
}

/// Go `requestOrigin`: `scheme://host[:port]`.
fn request_origin(u: &Url) -> String {
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    format!("{}://{}{}", u.scheme(), u.host_str().unwrap_or(""), port)
}

/// Go `cardSendsInClear`: every interface URL of the card that is neither https nor loopback,
/// in card order. An interface whose URL does not parse is skipped.
pub(crate) fn card_sends_in_clear(card: &a2a::AgentCard) -> Vec<String> {
    card.supported_interfaces
        .iter()
        .filter_map(|iface| {
            let u = Url::parse(&iface.url).ok()?;
            let clear =
                !u.scheme().eq_ignore_ascii_case("https") && !is_loopback_host(hostname(&u));
            clear.then(|| iface.url.clone())
        })
        .collect()
}

/// Go `isLoopbackHost`: `localhost`, a `.localhost` name, or a loopback IP (an IPv4-mapped
/// IPv6 loopback included, as Go's `net.IP.IsLoopback` treats it).
pub(crate) fn is_loopback_host(hostname: &str) -> bool {
    let h = hostname.to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    match hostname.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

/// Go `oauth2.RetrieveError`: a token endpoint's non-2xx response. A token source that reports
/// one lets [`redact_token_error`] keep the response body out of the error text.
#[derive(Debug, Clone, Default)]
pub(crate) struct RetrieveError {
    /// Go `Response.Status` (`"400 Bad Request"`); `None` = no response.
    pub status: Option<String>,
    /// The response body.
    pub body: Vec<u8>,
    /// RFC 6749 `error`.
    pub error_code: String,
    /// RFC 6749 `error_description`.
    pub error_description: String,
    /// RFC 6749 `error_uri`.
    pub error_uri: String,
}

impl fmt::Display for RetrieveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.error_code.is_empty() {
            write!(f, "oauth2: {}", go_quote(&self.error_code))?;
            if !self.error_description.is_empty() {
                write!(f, " {}", go_quote(&self.error_description))?;
            }
            if !self.error_uri.is_empty() {
                write!(f, " {}", go_quote(&self.error_uri))?;
            }
            return Ok(());
        }
        write!(
            f,
            "oauth2: cannot fetch token: {}\nResponse: {}",
            self.status.as_deref().unwrap_or(""),
            String::from_utf8_lossy(&self.body)
        )
    }
}

impl std::error::Error for RetrieveError {}

/// Go `redactedError`: reports `msg` while keeping `cause` reachable through the source chain.
#[derive(Debug)]
struct RedactedError {
    msg: String,
    cause: gadk_auth::Error,
}

impl fmt::Display for RedactedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for RedactedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Go `redactTokenError`: an error whose chain holds a token endpoint response with a body gets
/// a message built here, from the status and the RFC 6749 error code only; nothing is copied
/// from what any wrapper wrote. The original error stays reachable as the source. Any other
/// error is returned unchanged.
///
/// Two shapes count as such a response: a [`RetrieveError`] with a response and a non-empty
/// body (the first one in the chain decides), and a `reqwest` status error, which is what
/// `google-cloud-auth` (behind `gadk_auth::{adc, service_account}`) puts under a message that
/// carries the body verbatim.
pub(crate) fn redact_token_error(err: gadk_auth::Error) -> gadk_auth::Error {
    let Some((status, code)) = token_response(&err) else {
        return err;
    };
    let mut msg = format!("oauth2: cannot fetch token: {status} (response body redacted)");
    if !code.is_empty() {
        msg.push_str(": ");
        msg.push_str(&go_quote(&code));
    }
    gadk_auth::Error::Other(SharedError::new(RedactedError { msg, cause: err }))
}

/// The status and error code of the first token-endpoint response in `err`'s chain that
/// carried a body.
fn token_response(err: &(dyn std::error::Error + 'static)) -> Option<(String, String)> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(re) = e.downcast_ref::<RetrieveError>() {
            return match &re.status {
                Some(status) if !re.body.is_empty() => {
                    Some((status.clone(), re.error_code.clone()))
                }
                _ => None,
            };
        }
        if let Some(status) = e
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status)
        {
            return Some((status.to_string(), String::new()));
        }
        cur = e.source();
    }
    None
}

/// Go `mintTimeout`.
pub(crate) const MINT_TIMEOUT: Duration = Duration::from_secs(30);

/// The outcome of one step, shared by every caller waiting on it.
#[derive(Debug, Clone)]
pub(crate) enum StepError {
    /// Go `errors.New(text)`.
    Message(String),
    /// Go `fmt.Errorf("<prefix>: %w", source)`.
    Wrapped {
        /// The text before `: `.
        prefix: String,
        /// The wrapped error.
        source: SharedError,
    },
}

impl StepError {
    /// [`StepError::Wrapped`].
    pub(crate) fn wrap(
        prefix: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Wrapped {
            prefix: prefix.into(),
            source: SharedError::new(source),
        }
    }

    fn into_error(self) -> Error {
        match self {
            Self::Message(text) => Error::msg(text),
            Self::Wrapped { prefix, source } => Error::wrap(prefix, source),
        }
    }
}

type InFlight<T> = Arc<Mutex<HashMap<CredentialScope, Arc<MintCall<T>>>>>;

/// Go `mintGroup[T]`: collapses concurrent requests for one credential scope onto a single
/// blocking credential step (an OAuth2 token mint, or a credential's own `apply`) and hands the
/// result to everyone waiting on it. The step runs on its own task, so only the callers' wait is
/// bounded: by the attempt's deadline ([`MINT_TIMEOUT`] from its start), or by the caller
/// dropping the future (Go's context cancellation). A second step for the same scope starts only
/// once the current attempt's deadline has passed; the retired attempt still publishes to
/// whoever is on it, and never evicts its successor. A failed step is never replayed to a later
/// request, and a panicking step becomes an error.
pub(crate) struct MintGroup<T> {
    /// Names the step in errors, e.g. "mint oauth2 token".
    what: &'static str,
    /// Go `mintTimeout` (a field here so tests need not wait it out).
    timeout: Duration,
    in_flight: InFlight<T>,
}

/// Go `mintCall[T]`: one in-flight step. The outcome is written once, under the group's lock,
/// together with the removal of the group's entry.
pub(crate) struct MintCall<T> {
    deadline: Instant,
    outcome: watch::Sender<Option<Result<T, StepError>>>,
}

impl<T: Clone> MintCall<T> {
    /// Go `mintCall.result`: the outcome if it has landed, without blocking.
    fn result(&self) -> Option<Result<T, StepError>> {
        self.outcome.borrow().clone()
    }
}

impl MintGroup<String> {
    /// Go `newMintGroup`: the group for OAuth2 token mints.
    pub(crate) fn mints() -> Self {
        Self::new("mint oauth2 token", MINT_TIMEOUT)
    }

    /// Go `mintGroup.token`: a fresh access token from `ts` for `scope`.
    pub(crate) async fn token(
        &self,
        scope: &CredentialScope,
        ts: Arc<dyn TokenSource>,
    ) -> Result<String, Error> {
        self.run(scope, mint_access_token(ts)).await
    }
}

impl MintGroup<HeaderMap> {
    /// Go `newApplyGroup`: the group for a credential's own `apply`.
    pub(crate) fn applies() -> Self {
        Self::new("apply auth credential", MINT_TIMEOUT)
    }
}

impl<T> MintGroup<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// A group whose attempts each have `timeout`.
    pub(crate) fn new(what: &'static str, timeout: Duration) -> Self {
        Self {
            what,
            timeout,
            in_flight: Arc::default(),
        }
    }

    /// Go `mintGroup.do`: runs `step` for `scope`, joining a step already in flight (in which
    /// case `step` is dropped unpolled), and waits for it bounded by the attempt's deadline.
    pub(crate) async fn run<F>(&self, scope: &CredentialScope, step: F) -> Result<T, Error>
    where
        F: Future<Output = Result<T, StepError>> + Send + 'static,
    {
        let now = Instant::now();
        let call = {
            let mut in_flight = self.in_flight.lock();
            match in_flight.get(scope) {
                Some(call) if now < call.deadline => Arc::clone(call),
                // None, or an attempt that has spent its budget: retire it (its task still
                // publishes to whoever is on it) and start afresh.
                _ => {
                    let call = Arc::new(MintCall {
                        deadline: now + self.timeout,
                        outcome: watch::Sender::new(None),
                    });
                    in_flight.insert(scope.clone(), Arc::clone(&call));
                    tokio::spawn(publish(
                        self.what,
                        Arc::clone(&self.in_flight),
                        scope.clone(),
                        Arc::clone(&call),
                        step,
                    ));
                    call
                }
            }
        };

        // A result that has already landed beats an expired bound.
        if let Some(result) = call.result() {
            return result.map_err(StepError::into_error);
        }
        let mut rx = call.outcome.subscribe();
        let waited = tokio::time::timeout_at(call.deadline, async {
            rx.wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|v| v.clone())
        })
        .await;
        match waited.ok().flatten().or_else(|| call.result()) {
            Some(result) => result.map_err(StepError::into_error),
            None => Err(Error::wrap(
                format!("remoteagent: {}", self.what),
                gadk_agent::AgentError::DeadlineExceeded,
            )),
        }
    }

    /// The in-flight attempt for `scope`.
    #[cfg(test)]
    pub(crate) fn in_flight(&self, scope: &CredentialScope) -> Option<Arc<MintCall<T>>> {
        self.in_flight.lock().get(scope).cloned()
    }

    /// The number of in-flight attempts.
    #[cfg(test)]
    pub(crate) fn in_flight_len(&self) -> usize {
        self.in_flight.lock().len()
    }
}

#[cfg(test)]
impl<T: Clone> MintCall<T> {
    /// The attempt's deadline.
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Waits until the attempt's outcome has been published (Go `<-call.done`).
    pub(crate) async fn done(&self) {
        let mut rx = self.outcome.subscribe();
        let _ = rx.wait_for(Option::is_some).await;
    }
}

/// Go `mintGroup.run`: performs one step and publishes its outcome. The entry is removed (only
/// if it is still this attempt's) and the outcome published under the lock.
async fn publish<T, F>(
    what: &'static str,
    in_flight: InFlight<T>,
    scope: CredentialScope,
    call: Arc<MintCall<T>>,
    step: F,
) where
    F: Future<Output = Result<T, StepError>>,
{
    let result = match AssertUnwindSafe(step).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => Err(StepError::Message(format!(
            "remoteagent: {what}: credential panicked: {}",
            panic_text(&*panic)
        ))),
    };
    let mut in_flight = in_flight.lock();
    if in_flight
        .get(&scope)
        .is_some_and(|current| Arc::ptr_eq(current, &call))
    {
        in_flight.remove(&scope);
    }
    call.outcome.send_replace(Some(result));
}

/// Go `%v` of a recovered panic value, for the payloads Rust can name.
fn panic_text(panic: &(dyn Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_owned()
    }
}

/// Go `mintAccessToken`: one access token from `ts`, checked to be sendable as a bearer token.
async fn mint_access_token(ts: Arc<dyn TokenSource>) -> Result<String, StepError> {
    let tok = ts
        .token()
        .await
        .map_err(|e| StepError::wrap("remoteagent: mint oauth2 token", redact_token_error(e)))?;
    if tok.access_token.is_empty() {
        return Err(StepError::Message(
            "remoteagent: oauth2 token source returned an empty access token".to_owned(),
        ));
    }
    // `Token::token_type` is Go `Token.Type()`, which yields "Bearer" exactly when the type
    // equal-folds "bearer" or is empty.
    let ty = tok.token_type();
    if ty != "Bearer" {
        return Err(StepError::Message(format!(
            "remoteagent: oauth2 token type {} cannot be sent as a bearer token",
            go_quote(ty)
        )));
    }
    Ok(tok.access_token)
}

/// Go `credentialCell`: the credential one invocation resolved. An uncached cell (Go's nil
/// cell, what a request made outside an invocation gets) holds nothing and ignores writes.
pub(crate) struct CredentialCell {
    cred: Option<Mutex<Option<Arc<dyn Credential>>>>,
}

impl Default for CredentialCell {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialCell {
    /// An empty cell that keeps the first credential set on it (Go `&credentialCell{}`).
    pub(crate) fn new() -> Self {
        Self {
            cred: Some(Mutex::new(None)),
        }
    }

    /// A cell that never holds a credential (Go's nil `*credentialCell`).
    pub(crate) fn uncached() -> Self {
        Self { cred: None }
    }

    /// The credential, if one was set.
    pub(crate) fn get(&self) -> Option<Arc<dyn Credential>> {
        self.cred.as_ref().and_then(|c| c.lock().clone())
    }

    /// Stores `cred`; ignored by an uncached cell.
    pub(crate) fn set(&self, cred: Arc<dyn Credential>) {
        if let Some(c) = &self.cred {
            *c.lock() = Some(cred);
        }
    }
}
