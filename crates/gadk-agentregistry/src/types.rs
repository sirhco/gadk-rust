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

//! Go `agentregistry/types.go`: the registry's wire types. Every field Go declares, with Go's
//! JSON names; decoding follows Go's `encoding/json` leniencies that the registry can produce
//! (absent fields and JSON `null` leave the zero value, unknown fields are ignored).
//! Serialization mirrors Go's `omitempty` tags.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

/// Go `Interface`: a single connection interface (endpoint URL + binding) for a protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interface {
    /// Go `URL` (`json:"url,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub url: String,
    /// Go `ProtocolBinding` (`json:"protocolBinding,omitempty"`): `JSONRPC`, `HTTP_JSON`, `GRPC`.
    #[serde(
        rename = "protocolBinding",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_binding: String,
}

/// Go `Protocol`: a protocol a resource speaks together with its interfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Protocol {
    /// Go `Type` (`json:"type,omitempty"`), e.g. `A2A_AGENT`.
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_type: String,
    /// Go `ProtocolVersion` (`json:"protocolVersion,omitempty"`).
    #[serde(
        rename = "protocolVersion",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub protocol_version: String,
    /// Go `Interfaces` (`json:"interfaces,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
}

/// Go `Skill`: an A2A agent skill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Go `ID` (`json:"id,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub id: String,
    /// Go `Name` (`json:"name,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Go `Description` (`json:"description,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Go `Tags` (`json:"tags,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    /// Go `Examples` (`json:"examples,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub examples: Vec<String>,
}

/// Go `Card`: an embedded agent card. `content` holds the raw card JSON (e.g. an A2A
/// `AgentCard`) when `card_type` is `A2A_AGENT_CARD`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Card {
    /// Go `Type` (`json:"type,omitempty"`).
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub card_type: String,
    /// Go `Content json.RawMessage` (`json:"content,omitempty"`): `None` is Go's empty
    /// `RawMessage` (absent); a JSON `null` is kept as the raw text `null`, as Go keeps it.
    #[serde(
        default,
        deserialize_with = "raw_keep_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub content: Option<Box<RawValue>>,
}

/// Go `Agent`: a registered A2A agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Agent {
    /// Go `Name` (`json:"name,omitempty"`): the full resource name.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Go `DisplayName` (`json:"displayName,omitempty"`).
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Go `Description` (`json:"description,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Go `Version` (`json:"version,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub version: String,
    /// Go `Protocols` (`json:"protocols,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protocols: Vec<Protocol>,
    /// Go `Skills` (`json:"skills,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub skills: Vec<Skill>,
    /// Go `Card *Card` (`json:"card,omitempty"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<Card>,
}

/// Go `MCPServer`: a registered MCP server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
    /// Go `Name` (`json:"name,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Go `MCPServerID` (`json:"mcpServerId,omitempty"`).
    #[serde(
        rename = "mcpServerId",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub mcp_server_id: String,
    /// Go `DisplayName` (`json:"displayName,omitempty"`).
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Go `Description` (`json:"description,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Go `Protocols` (`json:"protocols,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protocols: Vec<Protocol>,
    /// Go `Interfaces` (`json:"interfaces,omitempty"`): top-level interfaces, treated as a
    /// trailing type-less protocol by the endpoint resolution.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
    /// Go `Tools` (`json:"tools,omitempty"`): the registry's declared tool metadata.
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tools: Vec<Tool>,
    /// Go `CreateTime` (`json:"createTime,omitempty"`), as the registry sends it.
    #[serde(
        rename = "createTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub create_time: String,
    /// Go `UpdateTime` (`json:"updateTime,omitempty"`).
    #[serde(
        rename = "updateTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub update_time: String,
    /// Go `Attributes map[string]any` (`json:"attributes,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Map::is_empty"
    )]
    pub attributes: Map<String, Value>,
}

/// Go `Tool`: a tool the registry reports for an MCP server (declared metadata; the live tool
/// set is discovered over MCP when a toolset connects).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// Go `Name` (`json:"name,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Go `Description` (`json:"description,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Go `Annotations *Annotations` (`json:"annotations,omitempty"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Go `Annotations`: behavioral hints for a [`Tool`]. Absent hints carry API-side defaults
/// (`destructiveHint` and `openWorldHint` default to true), so `false` here can mean "unset".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotations {
    /// Go `Title` (`json:"title,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub title: String,
    /// Go `DestructiveHint` (`json:"destructiveHint,omitempty"`).
    #[serde(
        rename = "destructiveHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub destructive_hint: bool,
    /// Go `IdempotentHint` (`json:"idempotentHint,omitempty"`).
    #[serde(
        rename = "idempotentHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub idempotent_hint: bool,
    /// Go `OpenWorldHint` (`json:"openWorldHint,omitempty"`).
    #[serde(
        rename = "openWorldHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub open_world_hint: bool,
    /// Go `ReadOnlyHint` (`json:"readOnlyHint,omitempty"`).
    #[serde(
        rename = "readOnlyHint",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "is_false"
    )]
    pub read_only_hint: bool,
}

/// Go `Endpoint`: a registered model endpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Go `Name` (`json:"name,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub name: String,
    /// Go `EndpointID` (`json:"endpointId,omitempty"`).
    #[serde(
        rename = "endpointId",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub endpoint_id: String,
    /// Go `DisplayName` (`json:"displayName,omitempty"`).
    #[serde(
        rename = "displayName",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    /// Go `Description` (`json:"description,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    /// Go `Interfaces` (`json:"interfaces,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub interfaces: Vec<Interface>,
    /// Go `CreateTime` (`json:"createTime,omitempty"`).
    #[serde(
        rename = "createTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub create_time: String,
    /// Go `UpdateTime` (`json:"updateTime,omitempty"`).
    #[serde(
        rename = "updateTime",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub update_time: String,
    /// Go `Attributes map[string]any` (`json:"attributes,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Map::is_empty"
    )]
    pub attributes: Map<String, Value>,
}

/// Go `ListAgentsResponse`: one page of [`Client::list_agents`](crate::Client::list_agents).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListAgentsResponse {
    /// Go `Agents` (`json:"agents,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub agents: Vec<Agent>,
    /// Go `NextPageToken` (`json:"nextPageToken,omitempty"`).
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// Go `ListMCPServersResponse`: one page of
/// [`Client::list_mcp_servers`](crate::Client::list_mcp_servers).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListMcpServersResponse {
    /// Go `MCPServers` (`json:"mcpServers,omitempty"`).
    #[serde(
        rename = "mcpServers",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub mcp_servers: Vec<McpServer>,
    /// Go `NextPageToken` (`json:"nextPageToken,omitempty"`).
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// Go `ListEndpointsResponse`: one page of
/// [`Client::list_endpoints`](crate::Client::list_endpoints).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ListEndpointsResponse {
    /// Go `Endpoints` (`json:"endpoints,omitempty"`).
    #[serde(
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub endpoints: Vec<Endpoint>,
    /// Go `NextPageToken` (`json:"nextPageToken,omitempty"`).
    #[serde(
        rename = "nextPageToken",
        default,
        deserialize_with = "null_default",
        skip_serializing_if = "String::is_empty"
    )]
    pub next_page_token: String,
}

/// Go `encoding/json`: a JSON `null` leaves the field at its zero value.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Go `json.RawMessage`: any present value, `null` included, is kept as raw text.
fn raw_keep_null<'de, D>(d: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Box::<RawValue>::deserialize(d).map(Some)
}

fn is_false(b: &bool) -> bool {
    !*b
}
