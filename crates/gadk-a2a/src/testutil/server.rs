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
//! REST routers + a static card) on an ephemeral loopback listener, laid out like the Go probe
//! that recorded the ADK-Go fixtures: the card at `/.well-known/agent-card.json`, JSON-RPC at `/jsonrpc`,
//! REST under `/rest`.

use std::net::Ipv4Addr;
use std::sync::Arc;

use a2a_server::{
    DefaultRequestHandler, InMemoryTaskStore, StaticAgentCard, agent_card::agent_card_router,
    jsonrpc::jsonrpc_router, rest::rest_router,
};

use crate::server::Executor;

/// The JSON-RPC endpoint's path.
const JSONRPC_PATH: &str = "/jsonrpc";
/// The prefix of the REST endpoints.
const REST_PREFIX: &str = "/rest";
/// The placeholder for the server's base URL in a card's interface URLs.
const BASE_PLACEHOLDER: &str = "<base>";

/// A running server; dropping it shuts the server down.
pub struct SpawnedServer {
    /// `http://127.0.0.1:<port>`.
    pub base_url: String,
    /// The card the server serves, its interfaces pointing at `base_url`.
    pub card: a2a::AgentCard,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for SpawnedServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// Serves `executor` with `card` on an ephemeral loopback port.
///
/// The served card is `card` with `BASE_PLACEHOLDER` in its interface URLs replaced by the
/// server's base URL (`"<base>"`); a card without interfaces gets Go's `newCard` pair, JSON-RPC at
/// `<base>/jsonrpc` and HTTP+JSON at `<base>/rest`. The request handler declares the card's
/// capabilities (a2a-rs refuses streaming unless they declare it).
///
/// # Panics
///
/// When no loopback port can be bound (a test fixture has no caller to report it to).
pub async fn spawn_a2a_server(executor: Executor, card: a2a::AgentCard) -> SpawnedServer {
    let listener = match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
        Ok(listener) => listener,
        Err(err) => panic!("spawn_a2a_server: binding a loopback port failed: {err}"),
    };
    let base_url = match listener.local_addr() {
        Ok(addr) => format!("http://{addr}"),
        Err(err) => panic!("spawn_a2a_server: reading the bound address failed: {err}"),
    };
    let card = with_base_url(card, &base_url);

    let handler = Arc::new(
        DefaultRequestHandler::new(executor, InMemoryTaskStore::new())
            .with_capabilities(card.capabilities.clone()),
    );
    let app = axum::Router::new()
        .nest(JSONRPC_PATH, jsonrpc_router(Arc::clone(&handler)))
        .nest(REST_PREFIX, rest_router(handler))
        .merge(agent_card_router(Arc::new(StaticAgentCard::new(
            card.clone(),
        ))));

    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = stopped.await;
        });
        if let Err(err) = serve.await {
            tracing::warn!(error = %err, "spawn_a2a_server: the server failed");
        }
    });

    SpawnedServer {
        base_url,
        card,
        shutdown: Some(shutdown),
    }
}

/// `card` with its interfaces pointing at `base_url`.
fn with_base_url(mut card: a2a::AgentCard, base_url: &str) -> a2a::AgentCard {
    if card.supported_interfaces.is_empty() {
        card.supported_interfaces = vec![
            a2a::AgentInterface::new(
                format!("{base_url}{JSONRPC_PATH}"),
                a2a::TRANSPORT_PROTOCOL_JSONRPC,
            ),
            a2a::AgentInterface::new(
                format!("{base_url}{REST_PREFIX}"),
                a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
            ),
        ];
        return card;
    }
    for iface in &mut card.supported_interfaces {
        iface.url = iface.url.replace(BASE_PLACEHOLDER, base_url);
    }
    card
}
