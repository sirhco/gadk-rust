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

//! Ports of ADK-Go `agentregistry/mcptoolset_e2e_test.go`: a registry record resolved by
//! [`Client::mcp_toolset`](crate::Client::mcp_toolset) reaches a local streamable-HTTP MCP
//! server (gadk-mcp's `HttpTestServer` + `ToolServer`, Go's `mcp.NewStreamableHTTPHandler`).

use gadk_agent::Context;
use gadk_agent::tool::Toolset as _;
use gadk_mcp::rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool};
use gadk_mcp::testutil::{HttpTestServer, ToolServer};

use super::agentregistry_requester::{new_test_client, serve};
use crate::factory::with_mcp_headers;
use crate::types::{Interface, McpServer};

/// Go `toolName`.
const TOOL_NAME: &str = "get_data";

/// Go `mcp.NewServer(&mcp.Implementation{Name: "data_mcp", Version: "v1.0.0"}, nil)` with
/// `get_data` ("returns data", input `struct{}`) answering an empty result.
fn data_server() -> ToolServer {
    let mut schema = JsonObject::new();
    schema.insert("additionalProperties".into(), false.into());
    schema.insert("type".into(), "object".into());
    ToolServer::new("data_mcp", "v1.0.0").tool(
        Tool::new(TOOL_NAME, "returns data", schema),
        |_: CallToolRequestParams, _| async move { Ok(CallToolResult::success(Vec::new())) },
    )
}

/// The registry record of Go's test: `Data MCP` with one JSONRPC interface at `url`.
fn record(url: &str) -> String {
    serde_json::to_string(&McpServer {
        display_name: "Data MCP".to_owned(),
        mcp_server_id: "data-mcp".to_owned(),
        interfaces: vec![Interface {
            url: url.to_owned(),
            protocol_binding: "JSONRPC".to_owned(),
        }],
        ..McpServer::default()
    })
    .unwrap()
}

/// Go `TestMCPToolset_E2E`.
#[tokio::test]
async fn test_mcp_toolset_e2_e() {
    let mcp = HttpTestServer::start(data_server).await;
    let body = record(&mcp.url());
    let reg = serve(move |_| (200, body.clone())).await;

    let ts = new_test_client(&reg)
        .mcp_toolset("projects/p/locations/l/mcpServers/data", &[])
        .await
        .unwrap_or_else(|err| panic!("MCPToolset() error = {err}"));

    let ctx = Context::builder().invocation_id("inv-1").build();
    let tools = ts
        .tools(&ctx)
        .await
        .unwrap_or_else(|err| panic!("Tools() error = {err}"));
    let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    assert!(
        names.iter().any(|n| n == TOOL_NAME),
        "Tools() = {names:?}, want it to contain {TOOL_NAME:?}"
    );

    if let Err(err) = ts.close().await {
        panic!("Close() error = {err}");
    }
}

/// R17 (not a Go test): `with_mcp_headers` reaches the MCP server on every request
/// (`McpTransport::StreamableHttp.headers`).
#[tokio::test]
async fn test_mcp_toolset_headers_reach_server() {
    let mcp = HttpTestServer::start(data_server).await;
    let body = record(&mcp.url());
    let reg = serve(move |_| (200, body.clone())).await;

    let ts = new_test_client(&reg)
        .mcp_toolset(
            "projects/p/locations/l/mcpServers/data",
            &[with_mcp_headers([("X-Test", "v")])],
        )
        .await
        .unwrap();
    let ctx = Context::builder().invocation_id("inv-1").build();
    ts.tools(&ctx).await.unwrap();
    ts.close().await.unwrap();

    let got = mcp.requests();
    assert!(!got.is_empty(), "the MCP server received no requests");
    for (i, r) in got.iter().enumerate() {
        let values: Vec<_> = r.headers.get_all("x-test").iter().collect();
        assert_eq!(values, ["v"], "request {i} ({}): X-Test", r.method);
    }
}
