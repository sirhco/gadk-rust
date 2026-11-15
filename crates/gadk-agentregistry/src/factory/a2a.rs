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

//! Go `agentregistry/factory.go`, A2A part: `RemoteAgent`, `agentCard`, `toA2ASkills` and the
//! compat transports. a2a-go registers a compat transport per advertised `(binding, version)`;
//! a2a-client-lf keys transports by `(binding, major version)` with no compat registration, so
//! [`CompatClientProvider`] rewrites each `JSONRPC` / `HTTP+JSON` interface of another major
//! version to the crate's current version before handing the card to its factory (ledgered).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_a2a::a2a;
use gadk_a2a::a2a_client;
use gadk_a2a::a2a_client::jsonrpc::JsonRpcTransportFactory;
use gadk_a2a::a2a_client::rest::RestTransportFactory;
use gadk_a2a::remoteagent::{A2AClient, A2AClientProvider, A2AConfig, ClientContext};
use gadk_agent::Agent as GadkAgent;
use serde_json::{Map, Value};

use super::{Egress, EgressClient, EgressConfig, add_headers, client_with_headers};
use crate::config::Client;
use crate::error::Error;
use crate::resolve::{PROTOCOL_TYPE_A2A_AGENT, TRANSPORT_HTTP_JSON, clean_name, connection_uri};
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
    let mut ec = EgressConfig::default();
    for opt in opts {
        match opt {
            RemoteAgentOption::HttpClient(c) => ec.http_client = Some(c.clone()),
            RemoteAgentOption::Headers(h) => add_headers(&mut ec, h),
        }
    }
    ec
}

impl Client {
    /// Go `RemoteAgent`: resolves the registered A2A agent `name` (full resource name) into a
    /// gadk remote agent usable as a sub-agent: [`Client::get_agent`], `agent_card`, then
    /// `gadk_a2a::remoteagent::new_a2a` with the cleaned name, the card's description, the card,
    /// and a `CompatClientProvider` over the option's client (or a default one, never the
    /// registry's) and the option's headers.
    pub async fn remote_agent(
        &self,
        name: &str,
        opts: &[RemoteAgentOption],
    ) -> Result<Arc<dyn GadkAgent>, Error> {
        let info = self.get_agent(name).await?;
        let (card, agent_name) = agent_card(&info, name)?;

        let ec = apply_remote_agent_options(opts);
        // A2A egress is not auto-authenticated (parity with adk-python): use the caller's
        // client or the default, never the registry's ADC client.
        let base = ec
            .http_client
            .map(EgressClient::Option)
            .unwrap_or(EgressClient::Default);
        let egress = client_with_headers(base, &ec.headers);

        gadk_a2a::remoteagent::new_a2a(A2AConfig {
            name: agent_name,
            description: card.description.clone(),
            agent_card: Some(card),
            client_provider: Some(Arc::new(CompatClientProvider::new(egress)?)),
            ..A2AConfig::default()
        })
        .map_err(Error::A2a)
    }
}

/// Go `agentCard`: the embedded card (when `card.card_type` is `A2A_AGENT_CARD` and content is
/// present) or one synthesized from the agent's fields, plus the cleaned agent name.
pub(crate) fn agent_card(
    info: &Agent,
    resource_name: &str,
) -> Result<(a2a::AgentCard, String), Error> {
    if let Some(content) = info
        .card
        .as_ref()
        .filter(|c| c.card_type == CARD_TYPE_A2A_AGENT_CARD)
        .and_then(|c| c.content.as_ref())
    {
        let embedded = decode_card(content.get()).map_err(|e| Error::EmbeddedCardDecode {
            resource: resource_name.to_owned(),
            message: e.to_string(),
        })?;
        // Fail fast: an interface-less card would otherwise only error at the first
        // invocation, deep in the a2a client, without the resource name.
        if embedded.supported_interfaces.is_empty() {
            return Err(Error::EmbeddedCardNoInterfaces(resource_name.to_owned()));
        }
        let agent_name = clean_name(&embedded.name);
        if agent_name.is_empty() {
            return Err(Error::EmbeddedCardEmptyName(resource_name.to_owned()));
        }
        return Ok((embedded, agent_name));
    }

    let Some(conn) = connection_uri(&info.protocols, &[], PROTOCOL_TYPE_A2A_AGENT, "") else {
        return Err(Error::NoConnectionUri(resource_name.to_owned()));
    };

    let display_name = if info.display_name.is_empty() {
        resource_name
    } else {
        &info.display_name
    };
    let agent_name = clean_name(display_name);
    if agent_name.is_empty() {
        return Err(Error::EmptyName(resource_name.to_owned()));
    }

    // Default to HTTP+JSON for an unrecognized binding (as adk-python does).
    let binding = if conn.binding.is_empty() {
        TRANSPORT_HTTP_JSON
    } else {
        conn.binding
    };
    let protocol_version = if conn.version.is_empty() {
        DEFAULT_A2A_PROTOCOL_VERSION.to_owned()
    } else {
        conn.version
    };

    let card = a2a::AgentCard {
        name: agent_name.clone(),
        description: info.description.clone(),
        version: info.version.clone(),
        supported_interfaces: vec![a2a::AgentInterface {
            url: conn.uri,
            protocol_binding: binding.to_owned(),
            protocol_version,
            tenant: None,
        }],
        capabilities: a2a::AgentCapabilities::default(),
        default_input_modes: vec!["text".to_owned()],
        default_output_modes: vec!["text".to_owned()],
        skills: to_a2a_skills(&info.skills),
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    };
    Ok((card, agent_name))
}

/// Go `json.Unmarshal(content, &a2a.AgentCard{})`: Go leaves absent or `null` fields at their
/// zero value (a `null` document decodes to the zero card), while a2a-lf requires some of them;
/// those are filled with Go's zero value before decoding, so only a malformed document fails.
/// Divergence (ledger 5c): keys are matched case-sensitively (Go's `json.Unmarshal` folds case).
fn decode_card(content: &str) -> Result<a2a::AgentCard, serde_json::Error> {
    let mut doc: Value = serde_json::from_str(content)?;
    if doc.is_null() {
        doc = Value::Object(Map::new());
    }
    if let Value::Object(card) = &mut doc {
        zero_fill(
            card,
            &[
                ("name", Value::from("")),
                ("description", Value::from("")),
                ("version", Value::from("")),
                ("supportedInterfaces", Value::Array(Vec::new())),
                ("capabilities", Value::Object(Map::new())),
                ("defaultInputModes", Value::Array(Vec::new())),
                ("defaultOutputModes", Value::Array(Vec::new())),
            ],
        );
        if let Some(Value::Array(ifaces)) = card.get_mut("supportedInterfaces") {
            // Go `[]*AgentInterface`: a `null` element is a nil pointer, which a2a-go's
            // factory skips; it cannot reach a client, so it is dropped here. Divergence
            // (ledger 5c): `[null]` thus leaves no interface and `remote_agent` fails fast with
            // `has no supported interfaces`; Go builds the agent and fails at invocation.
            ifaces.retain(|i| !i.is_null());
            for iface in ifaces.iter_mut().filter_map(Value::as_object_mut) {
                zero_fill(
                    iface,
                    &[
                        ("url", Value::from("")),
                        ("protocolBinding", Value::from("")),
                        ("protocolVersion", Value::from("")),
                    ],
                );
            }
        }
        if let Some(Value::Array(skills)) = card.get_mut("skills") {
            for skill in skills.iter_mut() {
                if skill.is_null() {
                    *skill = Value::Object(Map::new());
                }
                if let Some(skill) = skill.as_object_mut() {
                    zero_fill(
                        skill,
                        &[
                            ("id", Value::from("")),
                            ("name", Value::from("")),
                            ("description", Value::from("")),
                            ("tags", Value::Array(Vec::new())),
                        ],
                    );
                }
            }
        }
    }
    serde_json::from_value(doc)
}

/// Sets each absent or `null` key of `obj` to its Go zero value.
fn zero_fill(obj: &mut Map<String, Value>, zeros: &[(&str, Value)]) {
    for (key, zero) in zeros {
        match obj.get(*key) {
            None | Some(Value::Null) => {
                obj.insert((*key).to_owned(), zero.clone());
            }
            Some(_) => {}
        }
    }
}

/// Go `toA2ASkills`: registry skills as A2A skills (none → empty).
pub(crate) fn to_a2a_skills(skills: &[Skill]) -> Vec<a2a::AgentSkill> {
    skills
        .iter()
        .map(|s| a2a::AgentSkill {
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            tags: s.tags.clone(),
            examples: (!s.examples.is_empty()).then(|| s.examples.clone()),
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        })
        .collect()
}

/// The major version of an A2A protocol version (the text before the first `.`).
fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// The major of A2A 0.3 (Go `a2av0.Version`), the only older protocol Go's factory speaks.
const A2A_V0_MAJOR: &str = "0";

/// The card [`CompatClientProvider`] hands to its factory: every interface whose binding is
/// `JSONRPC` or `HTTP+JSON` and whose protocol version is a 0.x version gets
/// `protocol_version = a2a::VERSION`; everything else is unchanged, so an interface of an
/// unknown major (such as 2.0) finds no transport and is refused, as in Go (869020a1).
///
/// Divergence (ledger 5c): Go 869020a1 registers a2a-go's `a2av0` transports and speaks the A2A
/// 0.3 wire format (`A2A-Version: 0.3`, JSON-RPC 0.3 method names, HTTP+JSON under `/v1`) to a
/// 0.3.x interface (and to a registry record without a version, assumed 0.3.0); a2a-rs has no
/// A2A 0.3 client transport, so gadk speaks A2A 1.0 to such an interface (`A2A-Version: 1.0`,
/// 1.0 routes and payloads), which reaches a server that serves 1.0 at that interface.
pub(crate) fn compat_card(card: &a2a::AgentCard) -> a2a::AgentCard {
    let mut out = card.clone();
    for iface in &mut out.supported_interfaces {
        let compat_binding = iface.protocol_binding == a2a::TRANSPORT_PROTOCOL_JSONRPC
            || iface.protocol_binding == a2a::TRANSPORT_PROTOCOL_HTTP_JSON;
        if compat_binding
            && !iface.protocol_version.is_empty()
            && major(&iface.protocol_version) == A2A_V0_MAJOR
        {
            iface.protocol_version = a2a::VERSION.to_owned();
        }
    }
    out
}

/// The remote agent's client provider: Go `a2aClientFactory` (A2A 1.0 JSON-RPC and REST
/// transports over the egress client; 0.3 interfaces through [`compat_card`]) as a gadk-a2a
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
        let http = match egress.client {
            EgressClient::Option(c) => c,
            EgressClient::Registry | EgressClient::Default => super::default_http_client()?,
        };
        let mut builder = a2a_client::A2AClientFactory::builder()
            .register(Arc::new(JsonRpcTransportFactory::new(Some(http.clone()))))
            .register(Arc::new(RestTransportFactory::new(Some(http))));
        if !egress.headers.is_empty() {
            builder = builder.with_interceptor(Arc::new(HeaderInterceptor {
                headers: egress.headers,
            }));
        }
        Ok(CompatClientProvider {
            inner: gadk_a2a::remoteagent::new_a2a_client_provider(Some(Arc::new(builder.build()))),
        })
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
        self.inner.create_client(cx, &compat_card(card)).await
    }
}

/// Go `headerRoundTripper` for A2A: sets each header on every call (as a2a-client service
/// params, which its transports send as HTTP headers). Divergence (ledger 5c): a2a-client-lf
/// appends the params with reqwest's `builder.header`, so a header the transport also sets
/// (e.g. `Accept`) is sent with two values (Go `Header.Set` replaces it), and a value reqwest
/// rejects fails the A2A request (MCP skips such a header; Go sends it).
pub(crate) struct HeaderInterceptor {
    /// The headers, in key order.
    pub(crate) headers: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl a2a_client::middleware::CallInterceptor for HeaderInterceptor {
    async fn before(
        &self,
        _method: &str,
        params: &mut a2a_client::ServiceParams,
    ) -> Result<(), a2a::A2AError> {
        for (name, value) in &self.headers {
            // Go `Header.Set`: one value, replacing any spelling of the same header name.
            params.retain(|k, _| !k.eq_ignore_ascii_case(name));
            params.insert(name.clone(), vec![value.clone()]);
        }
        Ok(())
    }
}
