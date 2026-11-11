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

//! An in-process a2a-rs server (`DefaultRequestHandler` + `InMemoryTaskStore` + the JSON-RPC and
//! REST routers + a static card) on an ephemeral loopback listener. Bodies: plan 5b Task 7.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use crate::server::Executor;

/// A running server; dropping it shuts the server down.
pub struct SpawnedServer {
    /// `http://127.0.0.1:<port>`.
    pub base_url: String,
    /// The card the server serves, its interfaces pointing at `base_url`.
    pub card: a2a::AgentCard,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Serves `executor` with `card` on an ephemeral loopback port.
pub async fn spawn_a2a_server(executor: Executor, card: a2a::AgentCard) -> SpawnedServer {
    todo!("plan 5b Task 7")
}
