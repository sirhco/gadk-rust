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

//! Serves an in-memory agent over the ADK REST API (`gadk::server`), wire-compatible with
//! ADK REST clients such as the ADK web UI. The agent (`shouter`, a
//! one-node workflow that upper-cases the message) needs no model and no credentials:
//!
//! `cargo run -p gadk --example rest_server --features server [-- 127.0.0.1:8080]`
//!
//! Then, from another shell:
//!
//! ```text
//! curl -X POST http://127.0.0.1:8080/apps/shouter/users/u/sessions/s1
//! curl -X POST http://127.0.0.1:8080/run -H 'Content-Type: application/json' \
//!   -d '{"appName":"shouter","userId":"u","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"hello"}]}}'
//! ```

use std::sync::Arc;

use gadk::agent::{AgentError, Context, SingleLoader};
use gadk::engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk::engine::workflow::{Edge, NodeConfig, START, function_node};
use gadk::server::rest::{LocalAddr, Server, ServerConfig};
use gadk::session::InMemorySessionService;

async fn shout(_ctx: Context, input: String) -> Result<String, AgentError> {
    Ok(input.to_uppercase())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());

    let shout = function_node("shout", shout, NodeConfig::default());
    let agent = workflow_agent::new(WorkflowAgentConfig {
        name: "shouter".to_owned(),
        description: "Upper-cases the message.".to_owned(),
        edges: vec![Edge {
            from: Arc::clone(&START),
            to: shout,
            route: None,
        }],
        ..Default::default()
    })?;

    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(agent)));
    // Naming the address the server binds arms the origin guard's DNS-rebinding check for a
    // loopback address; list the web UI's origin to let it call the server from a browser.
    cfg.bind_host = addr.clone();
    cfg.allowed_origins = vec!["http://localhost:4200".to_owned()];
    let server = Server::new(cfg)?;

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("ADK REST API on http://{}", listener.local_addr()?);
    // The origin guard reads the address each connection was accepted on.
    axum::serve(
        listener,
        server
            .router()
            .into_make_service_with_connect_info::<LocalAddr>(),
    )
    .await?;
    Ok(())
}
