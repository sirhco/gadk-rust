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

//! The A2A client seam (port of ADK-Go `agent/remoteagent/v2/client.go` and the client types of
//! `internal/agent/remoteagent`). Bodies: plan 5b Task 6.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_agent::InvocationContext;

use crate::Error;

/// Go `A2AClient`: the calls the remote agent makes.
#[async_trait::async_trait]
pub trait A2AClient: Send + Sync {
    /// Go `SendMessage`.
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError>;
    /// Go `SendStreamingMessage`.
    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError>;
    /// Go `CancelTask`.
    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError>;
    /// Go `Destroy`.
    async fn destroy(&self) -> Result<(), a2a::A2AError>;
}

/// What a client is created for: an agent invocation, or work detached from one (the server's
/// child-task cleanup; Go passes the request context).
#[derive(Clone, Copy)]
pub enum ClientContext<'a> {
    /// A remote-agent invocation.
    Invocation(&'a InvocationContext),
    /// No invocation (server-side cleanup).
    Detached,
}

/// Go `A2AClientProvider`.
#[async_trait::async_trait]
pub trait A2AClientProvider: Send + Sync {
    /// Go `CreateClient`: a client for the agent `card` describes.
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error>;
}

/// Go `NewA2AClientProvider(factory)`; `None` → a default `A2AClientFactory`.
pub fn new_a2a_client_provider(
    factory: Option<Arc<a2a_client::A2AClientFactory>>,
) -> Arc<dyn A2AClientProvider> {
    todo!("plan 5b Task 6")
}
