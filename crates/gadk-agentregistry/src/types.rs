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

//! The registry's wire types, with the registry's JSON field names. Decoding is lenient: absent
//! fields and JSON `null` leave the field at its default, and unknown fields are ignored.
//! Serialization omits empty strings, empty lists, absent options and `false` hints.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

/// A single connection interface (endpoint URL + binding) for a protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    /// Endpoint URL of the interface.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub url: String,
    /// Protocol binding: `JSONRPC`, `HTTP_JSON` or `GRPC`.
    #[serde(
        rename = "protocolBinding",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_binding: String,
}

/// A protocol a resource speaks together with its interfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protocol {
    /// Protocol type, e.g. `A2A_AGENT`.
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_type: String,
    /// Version of the protocol.
    #[serde(
        rename = "protocolVersion",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_version: String,
    /// Interfaces the resource exposes.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
}

/// An A2A agent skill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Skill identifier.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub id: String,
    /// Skill name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Human-readable description.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Tags describing the skill.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    /// Example prompts for the skill.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub examples: Vec<String>,
}

/// An embedded agent card. `content` holds the raw card JSON (e.g. an A2A
/// `AgentCard`) when `card_type` is `A2A_AGENT_CARD`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Card {
    /// Card type, e.g. `A2A_AGENT_CARD`.
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub card_type: String,
    /// Raw card JSON. `None` means the field was absent; a JSON `null` is kept as the raw text
    /// `null`.
    #[serde(
        default,
        deserialize_with = "raw_keep_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub content: Option<Box<RawValue>>,
}

/// A registered A2A agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Agent {
    /// The full resource name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Human-readable display name.
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Human-readable description.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Version of the resource.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub version: String,
    /// Protocols the resource speaks.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protocols: Vec<Protocol>,
    /// Skills the agent advertises.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub skills: Vec<Skill>,
    /// Embedded agent card, if the registry returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
}

/// A registered MCP server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    /// Resource name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Short identifier of the MCP server within the registry.
    #[serde(
        rename = "mcpServerId",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub mcp_server_id: String,
    /// Human-readable display name.
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Human-readable description.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Protocols the resource speaks.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protocols: Vec<Protocol>,
    /// Top-level interfaces, treated as a trailing type-less protocol by the endpoint
    /// resolution.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
    /// The registry's declared tool metadata.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tools: Vec<Tool>,
    /// Creation time, as the registry sends it.
    #[serde(
        rename = "createTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub create_time: String,
    /// Last update time, as the registry sends it.
    #[serde(
        rename = "updateTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub update_time: String,
    /// Free-form attributes attached to the resource.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Map::is_empty"
    )]
    pub attributes: Map<String, Value>,
}

/// A tool the registry reports for an MCP server (declared metadata; the live tool
/// set is discovered over MCP when a toolset connects).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// Resource name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Human-readable description.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Behavioral hints for the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Behavioral hints for a [`Tool`]. Absent hints carry API-side defaults
/// (`destructiveHint` and `openWorldHint` default to true), so `false` here can mean "unset".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotations {
    /// Display title of the tool.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub title: String,
    /// Hint that the tool may perform destructive updates.
    #[serde(
        rename = "destructiveHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub destructive_hint: bool,
    /// Hint that repeated calls with the same arguments have no further effect.
    #[serde(
        rename = "idempotentHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub idempotent_hint: bool,
    /// Hint that the tool may interact with an open-ended set of external entities.
    #[serde(
        rename = "openWorldHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub open_world_hint: bool,
    /// Hint that the tool does not modify its environment.
    #[serde(
        rename = "readOnlyHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub read_only_hint: bool,
}

/// A registered model endpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Resource name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Short identifier of the endpoint within the registry.
    #[serde(
        rename = "endpointId",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub endpoint_id: String,
    /// Human-readable display name.
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Human-readable description.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Interfaces the resource exposes.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
    /// Creation time, as the registry sends it.
    #[serde(
        rename = "createTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub create_time: String,
    /// Last update time, as the registry sends it.
    #[serde(
        rename = "updateTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub update_time: String,
    /// Free-form attributes attached to the resource.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Map::is_empty"
    )]
    pub attributes: Map<String, Value>,
}

/// One page of [`Client::list_agents`](crate::Client::list_agents).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListAgentsResponse {
    /// Agents on this page.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub agents: Vec<Agent>,
    /// Token for the next page; empty on the last page.
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// One page of
/// [`Client::list_mcp_servers`](crate::Client::list_mcp_servers).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListMcpServersResponse {
    /// MCP servers on this page.
    #[serde(
        rename = "mcpServers",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mcp_servers: Vec<McpServer>,
    /// Token for the next page; empty on the last page.
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// One page of
/// [`Client::list_endpoints`](crate::Client::list_endpoints).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListEndpointsResponse {
    /// Endpoints on this page.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub endpoints: Vec<Endpoint>,
    /// Token for the next page; empty on the last page.
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// Deserializes a field, treating JSON `null` as the type's default value.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Deserializes any present value, `null` included, as raw JSON text.
fn raw_keep_null<'de, D>(d: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Box::<RawValue>::deserialize(d).map(Some)
}

fn is_false(b: &bool) -> bool {
    !*b
}
