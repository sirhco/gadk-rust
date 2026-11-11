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

//! Serves a weather agent over the Agent2Agent protocol and calls it through a remote A2A agent
//! (port of ADK-Go `examples/a2a`):
//!
//! `cargo run -p gadk --example a2a --features a2a`
//!
//! The server half mounts the agent card at `/.well-known/agent-card.json` and the A2A JSON-RPC
//! endpoint at `/invoke`, behind an authentication middleware (outermost) and gadk-a2a's identity
//! layer (inside it), so the executor runs as the authenticated caller. The client half is a
//! remote agent ([`gadk::a2a::remoteagent::new_a2a`]) that fetches the card and sends a bearer
//! token on every call. The weather agent answers from a scripted model, so the example runs
//! offline; swap in a real model (e.g. `gadk::gemini`) to talk to an LLM.

use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk::a2a::a2a;
use gadk::a2a::remoteagent::{A2AConfig, CardResolveOptions, new_a2a, new_agent_card_provider};
use gadk::a2a::server::{
    Executor, ExecutorConfig, a2a_identity_layer, build_agent_skills, identity_header_resolver,
};
use gadk::agent::{Agent, RunConfig};
use gadk::engine::llm::config::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk::model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk::session::{InMemorySessionService, SessionService};

/// The bearer token the client sends and the server accepts, and the user it stands for.
const TOKEN: &str = "demo-token";
const USER: &str = "alice";

/// A model that answers every request with a fixed forecast (stands in for Gemini).
struct ScriptedWeatherModel;

impl Llm for ScriptedWeatherModel {
    fn name(&self) -> &str {
        "scripted-weather"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let reply = LlmResponse {
            content: Some(Content::from_text(
                "It is sunny and 22°C in Paris right now.",
                ROLE_MODEL,
            )),
            ..Default::default()
        };
        futures::stream::iter([Ok(reply)]).boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `newWeatherAgent`.
fn new_weather_agent() -> anyhow::Result<Arc<dyn Agent>> {
    Ok(gadk::engine::llm_agent::new(LlmAgentConfig {
        name: "weather_time_agent".into(),
        model: Some(Arc::new(ScriptedWeatherModel)),
        description: "Agent to answer questions about the time and weather in a city.".into(),
        instruction: "I can answer your questions about the time and weather in a city.".into(),
        ..Default::default()
    })?)
}

/// The authenticated caller, stored by [`authenticate`] for the identity layer.
#[derive(Clone)]
struct CallerId(String);

/// A stand-in authentication middleware: `Authorization: Bearer demo-token` is user `alice`,
/// anything else is refused. In a gadk-server deployment this is `gadk::server::authn` with
/// `gadk::a2a::server::caller_user` as the identity extractor (façade features `a2a` and `server`).
async fn authenticate(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let authorized = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .is_some_and(|v| v.as_bytes() == format!("Bearer {TOKEN}").as_bytes());
    if !authorized {
        return axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED);
    }
    req.extensions_mut().insert(CallerId(USER.into()));
    next.run(req).await
}

/// Go `startWeatherAgentServer`: serves the weather agent over A2A on a loopback port; returns
/// the base URL.
async fn start_weather_agent_server() -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    println!("Starting A2A server on {base_url}");

    let agent = new_weather_agent()?;
    let agent_card: a2a::AgentCard = serde_json::from_value(serde_json::json!({
        "name": agent.name(),
        "description": agent.description(),
        "supportedInterfaces": [{
            "url": format!("{base_url}/invoke"),
            "protocolBinding": a2a::TRANSPORT_PROTOCOL_JSONRPC,
            "protocolVersion": "1.0",
        }],
        "version": "1.0.0",
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": build_agent_skills(agent.as_ref()),
        "capabilities": {"streaming": true},
    }))?;

    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(RunnerConfig {
            app_name: agent.name().to_owned(),
            agent,
            session_service: Arc::new(InMemorySessionService::new()),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: gadk::platform::Platform::default(),
            compaction: None,
        }),
        // The user is the identity layer's header, i.e. the authenticated caller.
        user_resolver: Some(identity_header_resolver()),
        ..Default::default()
    });
    let handler = Arc::new(
        gadk::a2a::a2a_server::DefaultRequestHandler::new(
            executor,
            gadk::a2a::a2a_server::InMemoryTaskStore::new(),
        )
        .with_capabilities(agent_card.capabilities.clone()),
    );

    // Mounting order: authentication outermost, the identity layer inside it (it replaces any
    // identity header the client sent with the authenticated caller), then the A2A router.
    let invoke = axum::Router::new()
        .nest(
            "/invoke",
            gadk::a2a::a2a_server::jsonrpc::jsonrpc_router(handler),
        )
        .layer(a2a_identity_layer(|ext: &axum::http::Extensions| {
            ext.get::<CallerId>().map(|c| c.0.clone())
        }))
        .layer(axum::middleware::from_fn(authenticate));
    let card = gadk::a2a::a2a_server::agent_card::agent_card_router(Arc::new(
        gadk::a2a::a2a_server::StaticAgentCard::new(agent_card),
    ));
    let app = card.merge(invoke);

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("A2A server stopped: {err}");
        }
    });
    Ok(base_url)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a2a_server_address = start_weather_agent_server().await?;

    let remote_agent = new_a2a(A2AConfig {
        name: "A2A Weather agent".into(),
        agent_card_provider: Some(new_agent_card_provider(
            a2a_server_address,
            CardResolveOptions::default(),
        )),
        // Sent as `Authorization: Bearer demo-token` on the card fetch and every A2A call.
        auth: Some(gadk::auth::static_token(TOKEN)),
        ..Default::default()
    })?;

    // Go hands the remote agent to the launcher (`full.NewLauncher()`); here one turn runs through
    // a runner, so the example stays self-contained. `examples/launcher.rs` shows an agent run
    // through `gadk::launcher::full::new()` (feature `launcher`).
    let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "a2a_example".into(),
        agent: remote_agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: gadk::platform::Platform::default(),
        compaction: None,
    })?;
    let question = Content::from_text("What is the weather in Paris?", ROLE_USER);
    println!("user: What is the weather in Paris?");
    let mut events = runner.run(
        "user",
        "session",
        Some(question),
        RunConfig::default(),
        RunOptions::default(),
    );
    let mut replied = false;
    while let Some(event) = events.next().await {
        let event = event?;
        if !event.response.error_message.is_empty() {
            anyhow::bail!("remote agent failed: {}", event.response.error_message);
        }
        for part in event.response.content.iter().flat_map(|c| &c.parts) {
            if !part.text.is_empty() {
                println!("{}: {}", event.author, part.text);
                replied = true;
            }
        }
    }
    anyhow::ensure!(replied, "the remote agent produced no reply");
    Ok(())
}
