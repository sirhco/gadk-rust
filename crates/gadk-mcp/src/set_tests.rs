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

//! Rust-only tests of the toolset over the in-memory server: listing, calling, closing and
//! `tools()` cancellation.

use std::sync::Arc;

use gadk_agent::tool::{Tool, ToolPredicate, Toolset};
use gadk_agent::{Context, ContextDelta, ToolError};
use pretty_assertions::assert_eq;
use rmcp::model::{CallToolResult, ContentBlock, Tool as McpToolDef};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::McpToolset;
use crate::config::{Config, McpTransport, TransportFactory};
use crate::error::Error;
use crate::testutil::{ScriptedFactory, ToolServer, in_memory, weather_server};
use crate::tool::McpTool;

/// `echo`'s input schema.
fn echo_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"]
    })
    .as_object()
    .unwrap()
    .clone()
}

/// `weather_server()` plus a second tool, `echo`, which answers its `text` argument.
fn two_tool_server() -> ToolServer {
    weather_server().tool(
        McpToolDef::new("echo", "echoes the text", Arc::new(echo_schema())),
        |params, _| async move {
            let text = params
                .arguments
                .and_then(|a| a.get("text").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        },
    )
}

fn toolset_over(factory: Arc<dyn TransportFactory>, filter: Option<ToolPredicate>) -> McpToolset {
    let mut cfg = Config::new(McpTransport::Custom(factory));
    cfg.tool_filter = filter;
    crate::new(cfg).unwrap()
}

fn toolset(filter: Option<ToolPredicate>) -> McpToolset {
    toolset_over(in_memory(two_tool_server), filter)
}

/// An invocation context whose cancellation token is `token`.
fn ctx_with_token(token: CancellationToken) -> Context {
    Context::builder()
        .invocation_id("inv-1")
        .cancellation_token(token)
        .build()
}

fn ctx() -> Context {
    ctx_with_token(CancellationToken::new())
}

/// The crate error behind a `ToolError::Execution`.
fn execution_source(err: &ToolError) -> &Error {
    match err {
        ToolError::Execution(source) => source
            .downcast_ref::<Error>()
            .unwrap_or_else(|| panic!("not a gadk_mcp::Error: {source}")),
        other => panic!("want ToolError::Execution, got {other:?}"),
    }
}

fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
    tools.iter().map(|t| t.name().to_string()).collect()
}

/// `ts.tools(ctx)`'s error; panics with the listed names when it succeeds.
async fn tools_err(ts: &McpToolset, ctx: &Context) -> ToolError {
    match ts.tools(ctx).await {
        Ok(tools) => panic!("want an error, got tools {:?}", names(&tools)),
        Err(err) => err,
    }
}

#[test]
fn test_name_and_description() {
    let ts = toolset(None);
    assert_eq!(Toolset::name(&ts), "mcp_tool_set");
    assert_eq!(
        ts.description(),
        "Connects to a MCP Server, retrieves MCP Tools into ADK Tools."
    );
    assert!(Toolset::as_any(&ts).downcast_ref::<McpToolset>().is_some());
}

#[tokio::test]
async fn test_tools_lists_server_tools_in_order() {
    let ts = toolset(None);
    let tools = ts.tools(&ctx()).await.unwrap();
    assert_eq!(names(&tools), vec!["get_weather", "echo"]);

    let weather = &tools[0];
    assert!(weather.as_any().downcast_ref::<McpTool>().is_some());
    assert_eq!(weather.description(), "returns weather in the given city");
    let decl = weather.declaration().unwrap();
    assert_eq!(decl.name, "get_weather");
    assert_eq!(decl.description, "returns weather in the given city");
    assert_eq!(
        decl.parameters_json_schema,
        Some(json!({
            "additionalProperties": false,
            "properties": {"city": {"description": "city name", "type": "string"}},
            "required": ["city"],
            "type": "object",
        }))
    );
    assert_eq!(
        decl.response_json_schema,
        Some(json!({
            "additionalProperties": false,
            "properties": {
                "weather_summary": {
                    "description": "weather summary in the given city",
                    "type": "string",
                },
            },
            "required": ["weather_summary"],
            "type": "object",
        }))
    );

    let echo = tools[1].declaration().unwrap();
    assert_eq!(echo.description, "echoes the text");
    assert_eq!(
        echo.parameters_json_schema,
        Some(Value::Object(echo_schema()))
    );
    assert_eq!(echo.response_json_schema, None);
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_tools_applies_the_filter_to_converted_tools() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_by_filter = seen.clone();
    let filter: ToolPredicate = Arc::new(move |ctx, tool| {
        // Go passes the converted ADK tool and the listing's context.
        assert!(tool.as_any().downcast_ref::<McpTool>().is_some());
        seen_by_filter
            .lock()
            .unwrap()
            .push((ctx.invocation_id().to_string(), tool.name().to_string()));
        tool.name() == "echo"
    });
    let ts = toolset(Some(filter));
    let tools = ts.tools(&ctx()).await.unwrap();
    assert_eq!(names(&tools), vec!["echo"]);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("inv-1".to_string(), "get_weather".to_string()),
            ("inv-1".to_string(), "echo".to_string()),
        ]
    );
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_listed_tools_call_the_server() {
    let ts = toolset(None);
    let tools = ts.tools(&ctx()).await.unwrap();
    let tool_ctx = ctx().with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        actions: Some(Default::default()),
        ..Default::default()
    });

    let weather = tools[0].as_runnable().unwrap();
    let got = weather
        .run(tool_ctx.clone(), json!({"city": "Paris"}))
        .await
        .unwrap();
    let mut want = Map::new();
    want.insert(
        "output".into(),
        json!({"weather_summary": "Today in \"Paris\" is sunny"}),
    );
    assert_eq!(got, want);

    let echo = tools[1].as_runnable().unwrap();
    let got = echo.run(tool_ctx, json!({"text": "hi"})).await.unwrap();
    let mut want = Map::new();
    want.insert("output".into(), json!("hi"));
    assert_eq!(got, want);
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_close_then_tools_fails_with_connection_closed() {
    let factory = ScriptedFactory::new(two_tool_server);
    let ts = toolset_over(factory.clone(), None);
    ts.tools(&ctx()).await.unwrap();
    assert_eq!(factory.connects(), 1);

    ts.close().await.unwrap();
    let err = tools_err(&ts, &ctx()).await;
    let source = execution_source(&err);
    assert!(source.is_connection_closed(), "{source:?}");
    assert_eq!(
        err.to_string(),
        "failed to list MCP tools: connection closed"
    );
    // Closing is final: no reconnect was attempted.
    assert_eq!(factory.connects(), 1);
}

#[tokio::test]
async fn test_close_is_idempotent() {
    let ts = toolset(None);
    // Never connected: closing is still fine, and so is closing again.
    ts.close().await.unwrap();
    ts.close().await.unwrap();

    let ts = toolset(None);
    ts.tools(&ctx()).await.unwrap();
    let (a, b) = tokio::join!(ts.close(), ts.close());
    a.unwrap();
    b.unwrap();
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_tools_honours_a_cancelled_context() {
    let factory = ScriptedFactory::new(two_tool_server);
    let ts = toolset_over(factory.clone(), None);
    let token = CancellationToken::new();
    token.cancel();
    let err = tools_err(&ts, &ctx_with_token(token)).await;
    let source = execution_source(&err);
    assert!(source.is_cancelled(), "{source:?}");
    assert!(!source.is_connection_closed(), "{source:?}");
    // The cancelled listing never connected.
    assert_eq!(factory.connects(), 0);
    ts.close().await.unwrap();
}

#[tokio::test]
async fn test_tools_stops_when_cancelled_while_connecting() {
    let factory = ScriptedFactory::new(two_tool_server);
    factory.close_gate();
    let ts = Arc::new(toolset_over(factory.clone(), None));
    let token = CancellationToken::new();
    let listing = tokio::spawn({
        let ts = ts.clone();
        let token = token.clone();
        async move { ts.tools(&ctx_with_token(token)).await.map(|t| t.len()) }
    });
    factory.entered().await;
    token.cancel();
    let err = listing.await.unwrap().unwrap_err();
    assert!(execution_source(&err).is_cancelled(), "{err:?}");

    // The toolset is still usable once the gate opens.
    factory.open_gate();
    assert_eq!(ts.tools(&ctx()).await.unwrap().len(), 2);
    ts.close().await.unwrap();
}
