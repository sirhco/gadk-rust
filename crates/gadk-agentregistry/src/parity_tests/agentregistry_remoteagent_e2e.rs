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

//! Ports of ADK-Go `agentregistry/remoteagent_e2e_test.go`: a registry record resolved by
//! [`Client::remote_agent`](crate::Client::remote_agent) reaches a real gadk-a2a server (Go's
//! `a2asrv` + `adka2a.NewExecutor` over a canned agent, a gadk-engine runner and an in-memory
//! session service). Plus the spec's compat test: a card advertising only `0.3.0` interfaces.

use std::sync::Arc;

use futures::StreamExt as _;
use gadk_a2a::a2a;
use gadk_a2a::remoteagent::{ClientContext, new_a2a_client_provider};
use gadk_a2a::server::{Executor, ExecutorConfig};
use gadk_a2a::testutil::{SpawnedServer, spawn_a2a_server};
use gadk_agent::{Agent as GadkAgent, AgentConfig, AgentError, RunConfig, new_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_model::LlmResponse;
use gadk_session::{Event, InMemorySessionService};
use serde_json::value::RawValue;

use super::agentregistry_requester::{new_test_client, serve};
use crate::types::{Agent, Card, Interface, Protocol};

/// Go `reply`.
const REPLY: &str = "hello from the remote agent";

/// The server-side layout of `spawn_a2a_server`: JSON-RPC at `/jsonrpc`, REST under `/rest`
/// (Go serves each handler at the root of its own `httptest` server).
fn endpoint(server: &SpawnedServer, binding: &str) -> String {
    match binding {
        a2a::TRANSPORT_PROTOCOL_JSONRPC => format!("{}/jsonrpc", server.base_url),
        a2a::TRANSPORT_PROTOCOL_HTTP_JSON => format!("{}/rest", server.base_url),
        other => panic!("unsupported binding {other:?}"),
    }
}

/// Go `a2a.AgentCard{Name, Description, SupportedInterfaces, Capabilities{Streaming: true}}`.
fn card(name: &str, description: &str, ifaces: Vec<a2a::AgentInterface>) -> a2a::AgentCard {
    a2a::AgentCard {
        name: name.to_owned(),
        description: description.to_owned(),
        version: String::new(),
        supported_interfaces: ifaces,
        capabilities: a2a::AgentCapabilities {
            streaming: Some(true),
            ..a2a::AgentCapabilities::default()
        },
        default_input_modes: Vec::new(),
        default_output_modes: Vec::new(),
        skills: Vec::new(),
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    }
}

/// Go `startCannedA2AServer`: an agent answering every turn with `reply`, served over A2A by
/// gadk-a2a's executor (gadk-engine runner, in-memory sessions). The server card is
/// streaming-capable, so both the streaming and the non-streaming client path are served.
async fn start_canned_a2a_server(reply: &'static str) -> SpawnedServer {
    let canned = new_agent(AgentConfig {
        name: "echo_agent".to_owned(),
        run: Some(Arc::new(move |ic| {
            let ev = Event {
                invocation_id: ic.invocation_id().to_owned(),
                author: "echo_agent".to_owned(),
                response: LlmResponse {
                    content: Some(Content::from_text(reply, ROLE_MODEL)),
                    ..LlmResponse::default()
                },
                ..Event::default()
            };
            futures::stream::iter([Ok::<_, AgentError>(ev)]).boxed()
        })),
        ..AgentConfig::default()
    })
    .unwrap();

    let executor = Executor::new(ExecutorConfig {
        runner_config: Some(RunnerConfig {
            app_name: canned.name().to_owned(),
            agent: canned,
            session_service: Arc::new(InMemorySessionService::new()),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: gadk_platform::Platform::default(),
            compaction: None,
        }),
        ..ExecutorConfig::default()
    });
    spawn_a2a_server(executor, card("echo_agent", "", Vec::new())).await
}

/// Go `runAndCollectText`: runs `agent` for one `prompt` turn (auto-created session) and
/// concatenates the text of every event's content.
async fn run_and_collect_text(agent: Arc<dyn GadkAgent>, prompt: &str) -> String {
    let runner = Runner::new(RunnerConfig {
        app_name: agent.name().to_owned(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: gadk_platform::Platform::default(),
        compaction: None,
    })
    .unwrap();

    let mut text = String::new();
    let mut events = runner.run(
        "user",
        "session",
        Some(Content::from_text(prompt, ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = events.next().await {
        let ev = item.unwrap_or_else(|err| panic!("runner.Run() yielded error = {err}"));
        let Some(content) = &ev.response.content else {
            continue;
        };
        for p in &content.parts {
            text.push_str(&p.text);
        }
    }
    text
}

/// Resolves `record` through a test registry and runs the remote agent once.
async fn remote_reply(record: serde_json::Value) -> String {
    let body = record.to_string();
    let reg = serve(move |_| (200, body.clone())).await;
    let remote = new_test_client(&reg)
        .remote_agent("projects/p/locations/l/agents/echo", &[])
        .await
        .unwrap_or_else(|err| panic!("RemoteAgent() error = {err}"));
    run_and_collect_text(remote, "ping").await
}

/// Go `TestRemoteAgent_E2E`.
#[tokio::test]
async fn test_remote_agent_e2_e() {
    type Record = fn(&str) -> Agent;
    let tests: [(&str, &str, Record); 2] = [
        (
            "embedded A2A_AGENT_CARD over JSONRPC",
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
            |server_url| {
                let card = card(
                    "Echo",
                    "echoes a canned reply",
                    vec![a2a::AgentInterface::new(
                        server_url,
                        a2a::TRANSPORT_PROTOCOL_JSONRPC,
                    )],
                );
                Agent {
                    display_name: "Echo".to_owned(),
                    description: "echoes a canned reply".to_owned(),
                    card: Some(Card {
                        card_type: "A2A_AGENT_CARD".to_owned(),
                        content: Some(
                            RawValue::from_string(serde_json::to_string(&card).unwrap()).unwrap(),
                        ),
                    }),
                    ..Agent::default()
                }
            },
        ),
        (
            // Registry wire binding HTTP_JSON maps to a2a HTTP+JSON; the missing
            // protocolVersion falls back to 0.3.0, which the compat provider serves.
            "discrete protocols over HTTP+JSON",
            a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
            |server_url| Agent {
                display_name: "Echo".to_owned(),
                description: "echoes a canned reply".to_owned(),
                protocols: vec![Protocol {
                    protocol_type: "A2A_AGENT".to_owned(),
                    interfaces: vec![Interface {
                        url: server_url.to_owned(),
                        protocol_binding: "HTTP_JSON".to_owned(),
                    }],
                    ..Protocol::default()
                }],
                ..Agent::default()
            },
        ),
    ];
    for (name, binding, record) in tests {
        let server = start_canned_a2a_server(REPLY).await;
        let a2a_url = endpoint(&server, binding);
        let got = remote_reply(serde_json::to_value(record(&a2a_url)).unwrap()).await;
        assert!(
            got.contains(REPLY),
            "{name}: remote agent response = {got:?}, want it to contain {REPLY:?}"
        );
    }
}

/// Spec (R16, not a Go test): a card whose interfaces all advertise protocol `0.3.0` (JSON-RPC
/// and HTTP+JSON, each alone) reaches a current gadk-a2a server through the registry's
/// compat provider, while a2a-client-lf's plain factory finds no transport for it.
#[tokio::test]
async fn test_compat_provider_reaches_0_3_0_only_card() {
    for binding in [
        a2a::TRANSPORT_PROTOCOL_JSONRPC,
        a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
    ] {
        let server = start_canned_a2a_server(REPLY).await;
        let mut iface = a2a::AgentInterface::new(endpoint(&server, binding), binding);
        iface.protocol_version = "0.3.0".to_owned();
        let old_card = card("Echo", "echoes a canned reply", vec![iface]);

        let plain = new_a2a_client_provider(None)
            .create_client(ClientContext::Detached, &old_card)
            .await;
        assert!(
            plain.is_err(),
            "{binding}: the plain provider served a 0.3.0-only card; the compat test is moot"
        );

        let record = serde_json::json!({
            "displayName": "Echo",
            "card": {"type": "A2A_AGENT_CARD", "content": old_card},
        });
        let got = remote_reply(record).await;
        assert!(
            got.contains(REPLY),
            "{binding}: remote agent response = {got:?}, want it to contain {REPLY:?}"
        );
    }
}
