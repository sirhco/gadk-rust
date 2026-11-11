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

//! The remote A2A agent (port of ADK-Go `agent/remoteagent/v2/a2a_agent.go`: `A2AConfig`,
//! `NewA2A`, the run loop, `newMessage`, `toErrorEvent`, `cleanupRemoteTask`). Bodies: plan 5b
//! Task 8.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{AfterAgentCallback, Agent, BeforeAgentCallback, Context, InvocationContext};
use gadk_session::Event;

use super::card::AgentCardProvider;
use super::client::{A2AClient, A2AClientProvider};
use crate::Error;
use crate::convert::{A2APartConverter, GenAIPartConverter};

/// Go `BeforeA2ARequestCallback`: an event short-circuits the request.
pub type BeforeA2ARequestCallback = Arc<
    dyn Fn(&Context, &a2a::SendMessageRequest) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `A2AEventConverter`: converts a remote response (or transport error) to a session event.
pub type A2AEventConverter = Arc<
    dyn Fn(
            &InvocationContext,
            &a2a::SendMessageRequest,
            Result<&a2a::StreamResponse, &a2a::A2AError>,
        ) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `AfterA2ARequestCallback`: an event replaces the converted one.
pub type AfterA2ARequestCallback = Arc<
    dyn Fn(
            &Context,
            &a2a::SendMessageRequest,
            Option<&Event>,
            Option<&a2a::A2AError>,
        ) -> BoxFuture<'static, Result<Option<Event>, Error>>
        + Send
        + Sync,
>;

/// Go `A2ARemoteTaskCleanupCallback`: runs after an abandoned or failed remote task was
/// cancelled; `task_info` is `(task_id, context_id)`.
pub type A2ARemoteTaskCleanupCallback = Arc<
    dyn Fn(
            &a2a::AgentCard,
            Arc<dyn A2AClient>,
            (String, String),
            Option<&Error>,
        ) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// Go `A2AConfig`.
#[derive(Clone, Default)]
pub struct A2AConfig {
    /// The agent name.
    pub name: String,
    /// The agent description.
    pub description: String,
    /// A fixed card (takes precedence over `agent_card_provider`).
    pub agent_card: Option<a2a::AgentCard>,
    /// Resolves the card on every invocation.
    pub agent_card_provider: Option<AgentCardProvider>,
    /// Go `BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<BeforeAgentCallback>,
    /// Go `BeforeRequestCallbacks`.
    pub before_request_callbacks: Vec<BeforeA2ARequestCallback>,
    /// Go `Converter`; `None` → `to_session_event_with_parts`.
    pub converter: Option<A2AEventConverter>,
    /// Go `AfterRequestCallbacks`.
    pub after_request_callbacks: Vec<AfterA2ARequestCallback>,
    /// Go `AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<AfterAgentCallback>,
    /// Go `AllowTransferToAgent`.
    pub allow_transfer_to_agent: bool,
    /// Go `A2APartConverter`.
    pub a2a_part_converter: Option<A2APartConverter>,
    /// Go `GenAIPartConverter`.
    pub genai_part_converter: Option<GenAIPartConverter>,
    /// Go `ClientProvider`; `None` → `new_a2a_client_provider(None)`.
    pub client_provider: Option<Arc<dyn A2AClientProvider>>,
    /// Go `Auth`: the credential every A2A call and the card fetch carry.
    pub auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
    /// Go `MessageSendConfig`.
    pub message_send_config: Option<a2a::SendMessageConfiguration>,
    /// Go `RemoteTaskCleanupCallback`.
    pub remote_task_cleanup_callback: Option<A2ARemoteTaskCleanupCallback>,
}

/// Go `NewA2A`: an agent of type `RemoteAgent` backed by the remote A2A agent.
pub fn new_a2a(cfg: A2AConfig) -> Result<Arc<dyn Agent>, Error> {
    todo!("plan 5b Task 8")
}
