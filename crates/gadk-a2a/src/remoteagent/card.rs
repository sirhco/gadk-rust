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

//! Agent-card resolution and source checks (port of ADK-Go `agent/remoteagent/v2/a2a_agent.go`
//! `NewAgentCardProvider`, `classifyCardSource`, `validateCardInterfaceOrigins`,
//! `requireSecureCardSource`). Bodies: plan 5b Task 6.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::Error;

/// Go `AgentCardProvider`: resolves the remote agent's card.
pub type AgentCardProvider =
    Arc<dyn Fn() -> BoxFuture<'static, Result<a2a::AgentCard, Error>> + Send + Sync>;

/// Go `agentcard.ResolveOption`s that [`new_agent_card_provider`] accepts.
#[derive(Debug, Clone, Default)]
pub struct CardResolveOptions {
    /// Go `agentcard.WithPath`: the card path; `None` → `/.well-known/agent-card.json`.
    pub path: Option<String>,
    /// Go `agentcard.WithRequestHeader`: extra request headers, in order.
    pub request_headers: Vec<(String, String)>,
}

/// Go `NewAgentCardProvider`: a URL or file source; reads the card-fetch client and credential
/// scope from the auth scope when present.
pub fn new_agent_card_provider(
    source: impl Into<String>,
    opts: CardResolveOptions,
) -> AgentCardProvider {
    todo!("plan 5b Task 6")
}

/// Go `classifyCardSource`: `Ok(true)` for a file source, `Ok(false)` for http(s).
pub(crate) fn classify_card_source(source: &str) -> Result<bool, Error> {
    todo!("plan 5b Task 6")
}

/// Go `validateCardInterfaceOrigins`.
pub(crate) fn validate_card_interface_origins(
    card: &a2a::AgentCard,
    source: &str,
) -> Result<(), Error> {
    todo!("plan 5b Task 6")
}

/// Go `requireSecureCardSource`.
pub(crate) fn require_secure_card_source(source: &str) -> Result<(), Error> {
    todo!("plan 5b Task 6")
}
