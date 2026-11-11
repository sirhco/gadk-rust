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

//! Ports of ADK-Go `agentregistry/factory_test.go`. Go's client-identity tests
//! (`TestEgressClient*`, `TestClientWithHeaders*`) compare `*http.Client` pointers; gadk records
//! the egress choice as an [`Egress`], so they assert the chosen [`EgressClient`] variant and the
//! header list (ledgered), and the header test sends through an a2a transport built from it.

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_a2a::a2a;
use gadk_a2a::remoteagent::{A2AClientProvider, ClientContext};
use pretty_assertions::assert_eq;
use serde_json::value::RawValue;

use super::agentregistry_requester::{new_test_client, serve, test_http_client};
use crate::config::Client;
use crate::factory::a2a::{
    CompatClientProvider, DEFAULT_A2A_PROTOCOL_VERSION, agent_card, apply_remote_agent_options,
};
use crate::factory::mcp::{apply_mcp_toolset_options, mcp_endpoint_uri};
use crate::factory::{
    Egress, EgressClient, EgressConfig, client_with_headers, is_google_api, with_a2a_headers,
    with_a2a_http_client, with_mcp_headers, with_mcp_http_client,
};
use crate::requester::RestRequester;
use crate::types::{Agent, Card, Interface, McpServer, Protocol, Skill};

/// A registry [`Client`] that is never asked to send anything (Go `&Client{httpClient: …}`);
/// `auth` stands for the requester's ADC provider (`None`: a caller-supplied client).
fn offline_client(auth: Option<Arc<dyn gadk_auth::CredentialProvider>>) -> Client {
    Client {
        requester: RestRequester {
            http: test_http_client(),
            auth,
            base_url: "http://127.0.0.1:1".to_owned(),
            base_path: "projects/p/locations/l".to_owned(),
            user_project: "p".to_owned(),
        },
    }
}

/// A header map from literal pairs (Go `map[string]string{…}`).
fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The variant name of an [`EgressClient`] (Go compares the client pointers).
fn variant(c: &EgressClient) -> &'static str {
    match c {
        EgressClient::Option(_) => "Option",
        EgressClient::Registry => "Registry",
        EgressClient::Default => "Default",
    }
}

/// Go `a2a.AgentCard{…}` with every other field at its zero value.
fn a2a_card(
    name: &str,
    description: &str,
    version: &str,
    ifaces: Vec<a2a::AgentInterface>,
) -> a2a::AgentCard {
    a2a::AgentCard {
        name: name.to_owned(),
        description: description.to_owned(),
        version: version.to_owned(),
        supported_interfaces: ifaces,
        capabilities: a2a::AgentCapabilities::default(),
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

/// Go `&Agent{Card: &Card{Type: "A2A_AGENT_CARD", Content: json.Marshal(card)}}`.
fn embedded(card: &a2a::AgentCard) -> Agent {
    let content = serde_json::to_string(card).unwrap();
    Agent {
        card: Some(Card {
            card_type: "A2A_AGENT_CARD".to_owned(),
            content: Some(RawValue::from_string(content).unwrap()),
        }),
        ..Agent::default()
    }
}

/// Go `Interface{URL, ProtocolBinding}`.
fn iface(url: &str, binding: &str) -> Interface {
    Interface {
        url: url.to_owned(),
        protocol_binding: binding.to_owned(),
    }
}

/// Go `TestIsGoogleAPI`.
#[test]
fn test_is_google_api() {
    let tests = [
        ("https://googleapis.com/v1", true),
        ("https://agentregistry.googleapis.com/v1", true),
        ("https://agentregistry.mtls.googleapis.com/v1", true),
        ("https://evil.com/googleapis.com", false),
        ("https://notgoogleapis.com", false),
        ("https://example.com", false),
        ("://bad", false),
    ];
    for (url, want) in tests {
        assert_eq!(is_google_api(url), want, "isGoogleAPI({url:?})");
    }
}

/// Go `TestEgressClient` (adapted: the chosen variant instead of the client pointer).
// parity: adapted: asserts the chosen EgressClient variant instead of comparing *http.Client pointers
#[test]
fn test_egress_client() {
    let c = offline_client(None);
    let tests = [
        (
            "explicit override wins",
            "https://x.googleapis.com",
            EgressConfig {
                http_client: Some(reqwest::Client::new()),
                ..EgressConfig::default()
            },
            "Option",
        ),
        (
            "google api reuses registry client",
            "https://x.googleapis.com/mcp",
            EgressConfig::default(),
            "Registry",
        ),
        (
            "non-google uses default",
            "https://third-party.example/mcp",
            EgressConfig::default(),
            "Default",
        ),
    ];
    for (name, url, ec, want) in tests {
        let got = c.egress(url, &ec);
        assert_eq!(variant(&got.client), want, "{name}: egressClient()");
        assert!(
            got.headers.is_empty(),
            "{name}: headers = {:?}",
            got.headers
        );
    }
}

/// Go `TestEgressClient_HeadersProduceDistinctClient` (adapted: the registry client is kept as
/// the base and the headers are layered on it, never written into the shared client).
// parity: adapted: asserts the Egress header list layered on the registry client instead of a distinct *http.Client
#[test]
fn test_egress_client_headers_produce_distinct_client() {
    let c = offline_client(None);
    let got = c.egress(
        "https://x.googleapis.com",
        &EgressConfig {
            headers: headers(&[("X", "y")]),
            ..EgressConfig::default()
        },
    );
    assert_eq!(variant(&got.client), "Registry");
    assert_eq!(got.headers, vec![("X".to_owned(), "y".to_owned())]);
}

/// Go `TestClientWithHeaders_NoHeadersReturnsBase` (adapted).
// parity: adapted: asserts the Egress (base variant, no headers) instead of client identity
#[test]
fn test_client_with_headers_no_headers_returns_base() {
    let got = client_with_headers(EgressClient::Default, &BTreeMap::new());
    assert_eq!(variant(&got.client), "Default");
    assert!(got.headers.is_empty(), "headers = {:?}", got.headers);
}

/// Go `TestClientWithHeaders_AppliesAndDoesNotMutateBase` (adapted): an A2A transport built from
/// the [`Egress`] sends `X-Test: v`; the base client itself still sends no `X-Test`.
// parity: adapted: an A2A transport built from the Egress sends the header to an axum server; the base client sends none
#[tokio::test]
async fn test_client_with_headers_applies_and_does_not_mutate_base() {
    let srv = serve(|_| (200, "{}".to_owned())).await;

    let base = test_http_client();
    let wrapped = client_with_headers(
        EgressClient::Option(base.clone()),
        &headers(&[("X-Test", "v")]),
    );
    assert_eq!(variant(&wrapped.client), "Option");
    assert_eq!(wrapped.headers, vec![("X-Test".to_owned(), "v".to_owned())]);

    let card = a2a_card(
        "x",
        "",
        "",
        vec![a2a::AgentInterface::new(
            srv.url.clone(),
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
        )],
    );
    let client = CompatClientProvider::new(wrapped)
        .unwrap()
        .create_client(ClientContext::Detached, &card)
        .await
        .unwrap();
    let req = a2a::SendMessageRequest {
        message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text("hi")]),
        configuration: None,
        metadata: None,
        tenant: None,
    };
    // The server's `{}` is not a JSON-RPC answer; only the request matters.
    let _ = client.send_message(&req).await;
    let _ = client.destroy().await;

    base.get(&srv.url).send().await.unwrap();

    let seen = srv.requests();
    assert_eq!(seen.len(), 2, "requests = {seen:?}");
    let x_test = |i: usize| {
        seen[i]
            .headers
            .get("x-test")
            .map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(x_test(0), Some("v".to_owned()), "server saw X-Test");
    assert_eq!(
        x_test(1),
        None,
        "the base client sent X-Test: it was mutated"
    );
}

/// Go `TestRemoteAgentOptions`.
#[test]
fn test_remote_agent_options() {
    let ec = apply_remote_agent_options(&[
        with_a2a_http_client(reqwest::Client::new()),
        with_a2a_headers([("X", "y")]),
        with_a2a_headers([("Z", "w")]),
    ]);
    assert!(
        ec.http_client.is_some(),
        "httpClient = None, want the option's client"
    );
    // Repeated WithA2AHeaders calls accumulate rather than replacing the set.
    assert_eq!(ec.headers, headers(&[("X", "y"), ("Z", "w")]));
}

/// Go `TestMCPToolsetOptions`.
#[test]
fn test_mcp_toolset_options() {
    let ec = apply_mcp_toolset_options(&[
        with_mcp_http_client(reqwest::Client::new()),
        with_mcp_headers([("X", "y")]),
        with_mcp_headers([("Z", "w")]),
    ]);
    assert!(
        ec.http_client.is_some(),
        "httpClient = None, want the option's client"
    );
    // Repeated WithMCPHeaders calls accumulate rather than replacing the set.
    assert_eq!(ec.headers, headers(&[("X", "y"), ("Z", "w")]));
}

/// Go `TestAgentCard_EmbeddedFastPath`.
#[test]
fn test_agent_card_embedded_fast_path() {
    let info = embedded(&a2a_card(
        "My Agent",
        "embedded desc",
        "9",
        vec![a2a::AgentInterface::new(
            "https://a.example",
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
        )],
    ));

    let (card, name) = agent_card(&info, "projects/p/locations/l/agents/a").unwrap();
    assert_eq!(name, "My_Agent", "name (cleaned)");
    assert_eq!(card.description, "embedded desc");
    // The embedded card itself is used verbatim (its name is not rewritten).
    assert_eq!(
        (card.name.as_str(), card.version.as_str()),
        ("My Agent", "9")
    );
}

/// Go `TestAgentCard_EmbeddedNoInterfaces`.
#[test]
fn test_agent_card_embedded_no_interfaces() {
    // An embedded card without supported interfaces must fail fast (with the resource name)
    // rather than only at the first invocation.
    let info = embedded(&a2a_card("My Agent", "", "", Vec::new()));
    let err = agent_card(&info, "projects/p/locations/l/agents/a").unwrap_err();
    assert_eq!(
        err.to_string(),
        "agentregistry: embedded agent card for \"projects/p/locations/l/agents/a\" has no supported interfaces"
    );
}

/// Go `TestAgentCard_EmbeddedEmptyName`.
#[test]
fn test_agent_card_embedded_empty_name() {
    // An embedded card must carry a usable name; the embedded path does not fall back to the
    // resource name (parity with adk-python), so an empty name fails.
    let info = embedded(&a2a_card(
        "",
        "",
        "",
        vec![a2a::AgentInterface::new(
            "https://a.example",
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
        )],
    ));
    let err = agent_card(&info, "projects/p/locations/l/agents/a").unwrap_err();
    assert_eq!(
        err.to_string(),
        "agentregistry: embedded agent card for \"projects/p/locations/l/agents/a\" has an empty name"
    );
}

/// Go `TestAgentCard_Synthesized`.
#[test]
fn test_agent_card_synthesized() {
    let info = Agent {
        display_name: "Cool Agent".to_owned(),
        description: "does cool things".to_owned(),
        version: "2.0".to_owned(),
        protocols: vec![Protocol {
            protocol_type: "A2A_AGENT".to_owned(),
            protocol_version: "0.3.0".to_owned(),
            interfaces: vec![iface("https://a.example/jsonrpc", "JSONRPC")],
        }],
        skills: vec![Skill {
            id: "s1".to_owned(),
            name: "Skill One".to_owned(),
            tags: vec!["t".to_owned()],
            ..Skill::default()
        }],
        ..Agent::default()
    };

    let (card, name) = agent_card(&info, "projects/p/locations/l/agents/cool").unwrap();
    assert_eq!(name, "Cool_Agent");
    assert_eq!(card.description, "does cool things");
    assert_eq!(
        (card.name.as_str(), card.version.as_str()),
        ("Cool_Agent", "2.0")
    );
    assert_eq!(card.supported_interfaces.len(), 1, "supported interfaces");
    let iface = &card.supported_interfaces[0];
    assert_eq!(iface.url, "https://a.example/jsonrpc", "interface URL");
    assert_eq!(
        iface.protocol_binding,
        a2a::TRANSPORT_PROTOCOL_JSONRPC,
        "interface binding"
    );
    assert_eq!(
        iface.protocol_version, "0.3.0",
        "interface protocol version"
    );
    assert_eq!(card.skills.len(), 1, "skills = {:?}", card.skills);
    assert_eq!(
        (card.skills[0].id.as_str(), card.skills[0].name.as_str()),
        ("s1", "Skill One")
    );
}

/// Go `TestAgentCard_SynthesizedDefaultsAndNameFallback`.
#[test]
fn test_agent_card_synthesized_defaults_and_name_fallback() {
    // No display name -> fall back to the resource name; no binding -> HTTP+JSON; no protocol
    // version -> default.
    let info = Agent {
        protocols: vec![Protocol {
            protocol_type: "A2A_AGENT".to_owned(),
            interfaces: vec![iface("https://a.example", "SOMETHING_ELSE")],
            ..Protocol::default()
        }],
        ..Agent::default()
    };
    let (card, name) = agent_card(&info, "projects/p/locations/l/agents/fallback-id").unwrap();
    assert_eq!(
        name, "projects_p_locations_l_agents_fallback_id",
        "cleaned resource name"
    );
    let iface = &card.supported_interfaces[0];
    assert_eq!(
        iface.protocol_binding,
        a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
        "default HTTP+JSON for unknown binding"
    );
    assert_eq!(iface.protocol_version, DEFAULT_A2A_PROTOCOL_VERSION);
}

/// Go `TestAgentCard_NoConnectionURI`.
#[test]
fn test_agent_card_no_connection_uri() {
    let info = Agent {
        display_name: "no interfaces".to_owned(),
        ..Agent::default()
    };
    let err = agent_card(&info, "projects/p/locations/l/agents/x").unwrap_err();
    assert_eq!(
        err.to_string(),
        "agentregistry: A2A connection URI not found for agent \"projects/p/locations/l/agents/x\""
    );
}

/// Go `TestAgentCard_EmptyNameError`.
#[test]
fn test_agent_card_empty_name_error() {
    // No display name and an empty resource name leave nothing to derive a non-empty agent name
    // from, so agentCard must fail rather than build an agent with an empty (invalid) name.
    let info = Agent {
        protocols: vec![Protocol {
            protocol_type: "A2A_AGENT".to_owned(),
            interfaces: vec![iface("https://a.example", "JSONRPC")],
            ..Protocol::default()
        }],
        ..Agent::default()
    };
    let err = agent_card(&info, "").unwrap_err();
    assert_eq!(
        err.to_string(),
        "agentregistry: cannot derive a non-empty agent name for \"\""
    );
}

/// Go `TestRemoteAgent_Integration`.
#[tokio::test]
async fn test_remote_agent_integration() {
    let srv = serve(|_| {
        (
            200,
            r#"{
			"displayName": "Doc Summarizer",
			"description": "summarizes documents",
			"protocols": [{"type": "A2A_AGENT", "protocolVersion": "0.3.0",
				"interfaces": [{"url": "https://a.example/jsonrpc", "protocolBinding": "JSONRPC"}]}]
		}"#
            .to_owned(),
        )
    })
    .await;

    let got = new_test_client(&srv)
        .remote_agent("projects/p/locations/l/agents/doc", &[])
        .await
        .unwrap();
    assert_eq!(got.name(), "Doc_Summarizer", "agent Name()");
    assert_eq!(
        got.description(),
        "summarizes documents",
        "agent Description()"
    );
}

/// Go `TestA2AClientFactory_CompatProtocolVersion`: a card advertising an older A2A protocol
/// version than the SDK's current one still yields a client.
#[tokio::test]
async fn test_a2_a_client_factory_compat_protocol_version() {
    let tests = [
        (
            "http+json old version",
            a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
            "0.3.0",
        ),
        (
            "jsonrpc old version",
            a2a::TRANSPORT_PROTOCOL_JSONRPC,
            "0.3.0",
        ),
        (
            "http+json current version",
            a2a::TRANSPORT_PROTOCOL_HTTP_JSON,
            a2a::VERSION,
        ),
    ];
    for (name, binding, version) in tests {
        let card = a2a_card(
            "x",
            "",
            "",
            vec![a2a::AgentInterface {
                url: "https://x.example/a2a".to_owned(),
                protocol_binding: binding.to_owned(),
                protocol_version: version.to_owned(),
                tenant: None,
            }],
        );
        let provider = CompatClientProvider::new(Egress {
            client: EgressClient::Default,
            headers: Vec::new(),
        })
        .unwrap();
        let client = match provider.create_client(ClientContext::Detached, &card).await {
            Ok(client) => client,
            Err(err) => {
                panic!("{name}: CreateFromCard({binding}@{version}) error = {err}, want a client")
            }
        };
        let _ = client.destroy().await;
    }
}

/// Go `TestMCPEndpointURI`.
#[test]
fn test_mcp_endpoint_uri() {
    let tests = [
        (
            "prefers jsonrpc over http_json",
            McpServer {
                interfaces: vec![
                    iface("https://s/http", "HTTP_JSON"),
                    iface("https://s/jsonrpc", "JSONRPC"),
                ],
                ..McpServer::default()
            },
            Some("https://s/jsonrpc"),
        ),
        (
            "falls back to http_json",
            McpServer {
                interfaces: vec![iface("https://s/http", "HTTP_JSON")],
                ..McpServer::default()
            },
            Some("https://s/http"),
        ),
        (
            "reads from protocols too",
            McpServer {
                protocols: vec![Protocol {
                    interfaces: vec![iface("https://s/jsonrpc", "JSONRPC")],
                    ..Protocol::default()
                }],
                ..McpServer::default()
            },
            Some("https://s/jsonrpc"),
        ),
        (
            "no supported binding",
            McpServer {
                interfaces: vec![iface("https://s/grpc", "GRPC")],
                ..McpServer::default()
            },
            None,
        ),
        ("empty", McpServer::default(), None),
    ];
    for (name, server, want) in tests {
        assert_eq!(
            mcp_endpoint_uri(&server).as_deref(),
            want,
            "{name}: mcpEndpointURI()"
        );
    }
}

/// Go `TestMCPToolset_Integration`.
#[tokio::test]
async fn test_mcp_toolset_integration() {
    let srv = serve(|_| {
        (
            200,
            r#"{
			"displayName": "Data MCP",
			"mcpServerId": "data-mcp",
			"interfaces": [{"url": "https://data.example/mcp", "protocolBinding": "JSONRPC"}]
		}"#
            .to_owned(),
        )
    })
    .await;

    let ts = new_test_client(&srv)
        .mcp_toolset("projects/p/locations/l/mcpServers/data", &[])
        .await
        .unwrap();
    let _ = ts.close().await;
}

/// Go `TestMCPToolset_NoEndpoint`.
#[tokio::test]
async fn test_mcp_toolset_no_endpoint() {
    let srv = serve(|_| (200, r#"{"displayName": "Data MCP"}"#.to_owned())).await; // no interfaces

    let err = match new_test_client(&srv)
        .mcp_toolset("projects/p/locations/l/mcpServers/data", &[])
        .await
    {
        Ok(_) => panic!("MCPToolset() error = nil, want an error when no endpoint URI is present"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        "agentregistry: MCP server endpoint URI not found for \"projects/p/locations/l/mcpServers/data\""
    );
}

/// R17 (not a Go test): the toolset authenticates with the registry's ADC provider exactly when
/// the egress is the registry's on the ADC path.
#[test]
fn test_mcp_config_auth_follows_registry_egress() {
    let adc = gadk_auth::static_token("tok");
    let adc_client = offline_client(Some(Arc::clone(&adc)));
    let caller_client = offline_client(None);
    let google = "https://x.googleapis.com/mcp";
    let other = "https://third-party.example/mcp";
    let override_ec = EgressConfig {
        http_client: Some(reqwest::Client::new()),
        ..EgressConfig::default()
    };

    let auth_of = |c: &Client, url: &str, ec: &EgressConfig| {
        c.mcp_config(url.to_owned(), c.egress(url, ec))
            .unwrap()
            .auth
    };
    let got = auth_of(&adc_client, google, &EgressConfig::default());
    assert!(
        got.is_some_and(|a| Arc::ptr_eq(&a, &adc)),
        "ADC path, googleapis URL: want the registry's provider"
    );
    assert!(
        auth_of(&adc_client, other, &EgressConfig::default()).is_none(),
        "ADC path, other URL"
    );
    assert!(
        auth_of(&adc_client, google, &override_ec).is_none(),
        "ADC path, option client"
    );
    assert!(
        auth_of(&caller_client, google, &EgressConfig::default()).is_none(),
        "caller-supplied registry client"
    );
}
