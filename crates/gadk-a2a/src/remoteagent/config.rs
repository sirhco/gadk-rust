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

//! The remote agent's server config and state (port of ADK-Go `internal/agent/remoteagent`
//! `a2a_config.go`). Bodies: plan 5b Task 6.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::Agent;

use super::card::AgentCardProvider;
use super::client::{A2AClient, A2AClientProvider, ClientContext};
use crate::Error;

/// Go `A2AServerConfig`: how to reach the remote agent.
#[derive(Clone)]
pub struct A2AServerConfig {
    /// A fixed card (takes precedence over `agent_card_provider`).
    pub agent_card: Option<a2a::AgentCard>,
    /// Resolves the card on every invocation.
    pub agent_card_provider: Option<AgentCardProvider>,
    /// Creates the client for a card.
    pub client_provider: Arc<dyn A2AClientProvider>,
    /// Go `OwnsAuthScope`: the remote agent enters the credential scope around its calls.
    pub owns_auth_scope: bool,
    /// Go `WithCardFetchClient`: overrides the card-fetch HTTP client.
    pub card_fetch_client: Option<reqwest::Client>,
    /// The credential provider (Go `A2AConfig.Auth`).
    pub auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
}

/// Go `RemoteAgentState`: stored as the remote agent's `AgentState.config`.
#[derive(Clone)]
pub struct RemoteAgentState {
    /// The server config.
    pub a2a: Arc<A2AServerConfig>,
}

/// Go `ResolveAgentCard`.
pub(crate) async fn resolve_agent_card(cfg: &A2AServerConfig) -> Result<a2a::AgentCard, Error> {
    todo!("plan 5b Task 6")
}

/// Go `CreateA2AClient`: the resolved card and a client for it.
pub(crate) async fn create_a2a_client(
    cx: ClientContext<'_>,
    cfg: &A2AServerConfig,
) -> Result<(a2a::AgentCard, Arc<dyn A2AClient>), Error> {
    todo!("plan 5b Task 6")
}

/// The server config of a remote agent (downcasts `AgentState.config` to
/// [`RemoteAgentState`]); `None` for any other agent.
pub(crate) fn find_remote_agent_state(agent: &dyn Agent) -> Option<Arc<A2AServerConfig>> {
    todo!("plan 5b Task 6")
}
