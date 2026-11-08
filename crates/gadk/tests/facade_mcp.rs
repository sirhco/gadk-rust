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

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! R16: `gadk::mcp` (feature `mcp`, on by default) is `gadk_mcp`, and a toolset built through
//! the façade lists and calls an in-memory server's tools.

use std::sync::Arc;

use gadk::agent::tool::Toolset;
use gadk::agent::{Context, ContextDelta};
use gadk::mcp::rmcp::model::ContentBlock;
use gadk::mcp::{
    CallContext, CommandBuilder, Config, Error, McpClient, McpTool, McpToolset, McpTransport,
    MetadataProvider, TransportFactory, format_mcp_content, new, serve_client,
};
use gadk_mcp::testutil::{in_memory, weather_server};
use pretty_assertions::assert_eq;
use serde_json::{Map, json};

/// Compiles only if each façade path names the `gadk_mcp` item.
#[test]
fn facade_mcp_paths_are_gadk_mcp() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<McpToolset>(None, None::<&gadk_mcp::McpToolset>);
    same::<McpTool>(None, None::<&gadk_mcp::McpTool>);
    same::<Config>(None, None::<&gadk_mcp::Config>);
    same::<McpTransport>(None, None::<&gadk_mcp::McpTransport>);
    same::<Error>(None, None::<&gadk_mcp::Error>);
    same::<CallContext>(None, None::<&gadk_mcp::CallContext>);
    same::<dyn McpClient>(None, None::<&dyn gadk_mcp::McpClient>);
    same::<dyn TransportFactory>(None, None::<&dyn gadk_mcp::TransportFactory>);
    same::<CommandBuilder>(None, None::<&gadk_mcp::CommandBuilder>);
    same::<MetadataProvider>(None, None::<&gadk_mcp::MetadataProvider>);
    let _: fn(Config) -> Result<McpToolset, Error> = new;
    let _ = serve_client::<tokio::io::DuplexStream, std::io::Error, _>;

    // Go's formatter, through the façade: plain text blocks are concatenated as Go does.
    assert_eq!(
        format_mcp_content(&[ContentBlock::text("a"), ContentBlock::text("b")]),
        "ab"
    );
    // `new` validates without connecting (R1).
    let ts = new(Config::new(McpTransport::Endpoint(String::new()))).err();
    assert_eq!(
        ts.map(|e| e.to_string()).as_deref(),
        Some("mcptoolset: set Config.Transport or Config.Endpoint")
    );
}

#[tokio::test]
async fn facade_mcp_toolset_lists_and_calls_in_memory_tools() {
    let ts = new(Config::new(McpTransport::Custom(in_memory(weather_server)))).unwrap();
    assert_eq!(Toolset::name(&ts), "mcp_tool_set");

    let ctx = Context::builder().invocation_id("inv-1").build();
    let tools = ts.tools(&ctx).await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(names, ["get_weather"]);
    assert!(tools[0].as_any().downcast_ref::<McpTool>().is_some());

    let tool_ctx = ctx.with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        actions: Some(Default::default()),
        ..Default::default()
    });
    let got = tools[0]
        .as_runnable()
        .unwrap()
        .run(tool_ctx, json!({"city": "Paris"}))
        .await
        .unwrap();
    let mut want = Map::new();
    want.insert(
        "output".into(),
        json!({"weather_summary": "Today in \"Paris\" is sunny"}),
    );
    assert_eq!(got, want);

    // The façade toolset is shareable as a gadk toolset.
    let _: Arc<dyn Toolset> = Arc::new(ts);
}
