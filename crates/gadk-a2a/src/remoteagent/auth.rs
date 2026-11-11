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
//! `agent/remoteagent/v2/auth.go` and `internal/agent/remoteagent/auth_scope.go`). Bodies: plan
//! 5b Task 4.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::Error;

/// Go `a2aclient.SessionID` as built by `CredentialScope`: the credential cache key of an
/// (app, user, session, agent) call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialScope(pub String);

/// Go `CredentialScope(appName, userID, sessionID, agentName)`.
pub fn credential_scope(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    agent_name: &str,
) -> CredentialScope {
    todo!("plan 5b Task 4")
}

/// The credential scope of the current call (Go `a2aclient.SessionIDFrom(ctx)`), for custom
/// credential providers; `None` outside a scoped call.
pub fn current_credential_scope() -> Option<CredentialScope> {
    todo!("plan 5b Task 4")
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

/// Runs `fut` inside the auth scope `id`; `None` runs it unscoped (Go `OwnsAuthScope == false`).
pub(crate) async fn with_auth_scope<F: Future>(id: Option<CallIdentity>, fut: F) -> F::Output {
    todo!("plan 5b Task 4")
}

/// Applies the scope's credential to every A2A call (Go `authTransport`).
pub(crate) struct AuthInterceptor;

#[async_trait::async_trait]
impl a2a_client::middleware::CallInterceptor for AuthInterceptor {
    async fn before(
        &self,
        method: &str,
        params: &mut a2a_client::ServiceParams,
    ) -> Result<(), a2a::A2AError> {
        todo!("plan 5b Task 4")
    }
}

/// Applies the credential of the current auth scope to `headers`; a no-op outside a scope.
pub(crate) async fn apply_credential(headers: &mut http::HeaderMap) -> Result<(), Error> {
    todo!("plan 5b Task 4")
}

/// Go `authHTTPClient`: 3 min timeout, Go `checkRedirect` policy, at most 10 redirects.
pub(crate) fn auth_http_client() -> reqwest::Client {
    todo!("plan 5b Task 4")
}

/// Go `cardFetchHTTPClient`: the RPC client's redirect policy with a 30 s timeout.
pub(crate) fn card_fetch_http_client() -> reqwest::Client {
    todo!("plan 5b Task 4")
}

/// Go `cardSendsInClear`: the card's non-loopback `http` interface URLs.
pub(crate) fn card_sends_in_clear(card: &a2a::AgentCard) -> Vec<String> {
    todo!("plan 5b Task 4")
}

/// Go `redactTokenError`.
pub(crate) fn redact_token_error(err: gadk_auth::Error) -> Error {
    todo!("plan 5b Task 4")
}

/// Go `mintTimeout`.
pub(crate) const MINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Go `mintGroup[T]`: deduplicates concurrent mints and header applications per scope.
pub(crate) struct MintGroup<T> {
    _value: PhantomData<T>,
}

/// Go `credentialCell`: the credential of one call.
pub(crate) struct CredentialCell {}

impl CredentialCell {
    /// Go `&credentialCell{}` (Task 4 keeps this signature).
    pub(crate) fn new() -> Self {
        Self {}
    }
}
