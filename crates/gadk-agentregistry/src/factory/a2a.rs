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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `agentregistry/factory.go`, A2A part: `RemoteAgent`, `agentCard`, `toA2ASkills` and the
//! compat transports. a2a-go registers a compat transport per advertised `(binding, version)`;
//! a2a-client-lf keys transports by `(binding, major version)` with no compat registration, so
//! [`CompatClientProvider`] rewrites each `JSONRPC` / `HTTP+JSON` interface of another major
//! version to the crate's current version before handing the card to its factory (ledgered).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_a2a::a2a;
use gadk_a2a::a2a_client;
use gadk_a2a::remoteagent::{A2AClient, A2AClientProvider, ClientContext};
use gadk_agent::Agent as GadkAgent;

use super::{Egress, EgressConfig};
use crate::config::Client;
use crate::error::Error;
use crate::types::{Agent, Skill};

/// Go `cardTypeA2AAgentCard`.
pub(crate) const CARD_TYPE_A2A_AGENT_CARD: &str = "A2A_AGENT_CARD";
/// Go `defaultA2AProtocolVersion`.
pub(crate) const DEFAULT_A2A_PROTOCOL_VERSION: &str = "0.3.0";

/// Go `RemoteAgentOption`: customizes [`Client::remote_agent`]; applied in order.
#[derive(Debug, Clone)]
pub enum RemoteAgentOption {
    /// Go `WithA2AHTTPClient`: the HTTP client used to reach the remote A2A agent (A2A egress
    /// is not auto-authenticated). A later one replaces an earlier one.
    HttpClient(reqwest::Client),
    /// Go `WithA2AHeaders`: headers added to (or overwriting) every A2A request; repeated
    /// options accumulate, a later value wins on a key conflict.
    Headers(BTreeMap<String, String>),
}

/// Go `WithA2AHTTPClient`.
pub fn with_a2a_http_client(client: reqwest::Client) -> RemoteAgentOption {
    RemoteAgentOption::HttpClient(client)
}

/// Go `WithA2AHeaders`.
pub fn with_a2a_headers<K, V>(headers: impl IntoIterator<Item = (K, V)>) -> RemoteAgentOption
where
    K: Into<String>,
    V: Into<String>,
{
    RemoteAgentOption::Headers(
        headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    )
}

/// Go `applyRemoteAgentOptions`.
pub(crate) fn apply_remote_agent_options(opts: &[RemoteAgentOption]) -> EgressConfig {
    todo!("Task 5")
}

impl Client {
    /// Go `RemoteAgent`: resolves the registered A2A agent `name` (full resource name) into a
    /// gadk remote agent usable as a sub-agent: [`Client::get_agent`], [`agent_card`], then
    /// `gadk_a2a::remoteagent::new_a2a` with the cleaned name, the card's description, the card,
    /// and a [`CompatClientProvider`] over the option's client (or a default one, never the
    /// registry's) and the option's headers.
    pub async fn remote_agent(
        &self,
        name: &str,
        opts: &[RemoteAgentOption],
    ) -> Result<Arc<dyn GadkAgent>, Error> {
        todo!("Task 5")
    }
}

/// Go `agentCard`: the embedded card (when `card.card_type` is `A2A_AGENT_CARD` and content is
/// present) or one synthesized from the agent's fields, plus the cleaned agent name.
pub(crate) fn agent_card(
    info: &Agent,
    resource_name: &str,
) -> Result<(a2a::AgentCard, String), Error> {
    todo!("Task 5")
}

/// Go `toA2ASkills`: registry skills as A2A skills (none → empty).
pub(crate) fn to_a2a_skills(skills: &[Skill]) -> Vec<a2a::AgentSkill> {
    todo!("Task 5")
}

/// The card [`CompatClientProvider`] hands to its factory: every interface whose binding is
/// `JSONRPC` or `HTTP+JSON` and whose non-empty protocol version's major differs from
/// `a2a::VERSION`'s gets `protocol_version = a2a::VERSION`; everything else is unchanged.
pub(crate) fn compat_card(card: &a2a::AgentCard) -> a2a::AgentCard {
    todo!("Task 5")
}

/// The remote agent's client provider: Go `a2aClientFactory` (JSON-RPC and REST transports
/// over the egress client, compat transports for the card's older versions) as a gadk-a2a
/// provider.
pub(crate) struct CompatClientProvider {
    /// `gadk_a2a::remoteagent::new_a2a_client_provider` over an `A2AClientFactory` with the
    /// JSON-RPC and REST transports on the egress client and a [`HeaderInterceptor`].
    inner: Arc<dyn A2AClientProvider>,
}

impl CompatClientProvider {
    /// The provider for `egress` (`EgressClient::Option` or `EgressClient::Default` only; the
    /// registry client is never used for A2A).
    pub(crate) fn new(egress: Egress) -> Result<CompatClientProvider, Error> {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
impl A2AClientProvider for CompatClientProvider {
    /// [`compat_card`], then the inner provider.
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, gadk_a2a::Error> {
        todo!("Task 5")
    }
}

/// Go `headerRoundTripper` for A2A: sets each header on every call (as a2a-client service
/// params, which its transports send as HTTP headers).
pub(crate) struct HeaderInterceptor {
    /// The headers, in key order.
    pub(crate) headers: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl a2a_client::middleware::CallInterceptor for HeaderInterceptor {
    async fn before(
        &self,
        method: &str,
        params: &mut a2a_client::ServiceParams,
    ) -> Result<(), a2a::A2AError> {
        todo!("Task 5")
    }
}
