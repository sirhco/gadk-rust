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

//! Rust-only: spawns `gadk-mcp-stdio-test-server` through `McpTransport::Command`, lists and
//! calls a tool, kills the child, and checks that the next use reconnects (Unix only).
//!
//! The first test drives the session by hand, as the toolset's private `Command` factory does on
//! every connect: run the transport's `CommandBuilder` and serve the client over a fresh
//! `TokioChildProcess` (Task 7). The second goes through the public API end to end:
//! `gadk_mcp::new` over `McpTransport::Command`, `Toolset::tools` and `RunnableTool::run`, with
//! the connection refresher reconnecting to a new child after the first one is killed (Task 12).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gadk_agent::tool::Toolset;
use gadk_agent::{Context, ContextDelta};
use gadk_mcp::rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation,
};
use gadk_mcp::rmcp::service::RunningService;
use gadk_mcp::rmcp::transport::TokioChildProcess;
use gadk_mcp::rmcp::{RoleClient, ServiceError};
use gadk_mcp::{Config, McpTool, McpTransport, serve_client};
use serde_json::{Map, json};

/// The transport a user configures for the stdio test server.
fn stdio_server() -> McpTransport {
    McpTransport::Command(Arc::new(|| {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_gadk-mcp-stdio-test-server"))
    }))
}

/// One connect of a `Command` transport: a fresh child from the builder, its pid, and the
/// initialized session over its stdio.
async fn connect(transport: &McpTransport) -> (RunningService<RoleClient, ClientConfig>, u32) {
    let McpTransport::Command(build) = transport else {
        panic!("want a Command transport, got {}", transport.variant_name());
    };
    let child = TokioChildProcess::new(build()).unwrap();
    let pid = child.id().unwrap();
    let info = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("child-process-test", "v1"),
    );
    (serve_client(info, child).await.unwrap(), pid)
}

async fn weather_in_paris(session: &RunningService<RoleClient, ClientConfig>) -> serde_json::Value {
    let mut params = CallToolRequestParams::new("get_weather");
    params.arguments = json!({"city": "Paris"}).as_object().cloned();
    let result = session.peer().call_tool(params).await.unwrap();
    result.structured_content.unwrap()
}

#[tokio::test]
async fn command_transport_lists_and_calls_and_reconnects_after_the_child_dies() {
    let transport = stdio_server();
    let (session, pid) = connect(&transport).await;

    let tools = session.peer().list_tools(None).await.unwrap().tools;
    let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["get_weather"]);
    assert_eq!(
        weather_in_paris(&session).await,
        json!({"weather_summary": "Today in \"Paris\" is sunny"})
    );

    // Kill the child: the session's next request fails.
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    // The child's stdout closes (TransportClosed) or its stdin breaks (TransportSend), depending
    // on which the client notices first; both are connection losses the refresher recovers from.
    let err = session.peer().list_tools(None).await.unwrap_err();
    assert!(
        matches!(
            err,
            ServiceError::TransportClosed | ServiceError::TransportSend(_)
        ),
        "{err:?}"
    );

    // The next connect runs the builder again and serves from a new child.
    let (fresh, fresh_pid) = connect(&transport).await;
    assert_ne!(fresh_pid, pid);
    assert_eq!(
        weather_in_paris(&fresh).await,
        json!({"weather_summary": "Today in \"Paris\" is sunny"})
    );
    fresh.cancel().await.unwrap();
    let _ = session.cancel().await;
}

/// The pids of the live stdio test servers whose command line carries `tag`.
fn pids_tagged(tag: &str) -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args(["-f", tag])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Waits (up to 10 s) until `pids_tagged(tag)` satisfies `done`, and returns it.
async fn wait_for_pids(tag: &str, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    for _ in 0..200 {
        let pids = pids_tagged(tag);
        if done(&pids) {
            return pids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("stdio test servers tagged {tag}: {:?}", pids_tagged(tag));
}

#[tokio::test]
async fn toolset_over_a_command_transport_reconnects_after_the_child_dies() {
    // The server ignores its arguments; the tag lets the test find (and kill) its own children
    // only, while other tests spawn the same binary concurrently.
    let tag = format!("gadk-mcp-child-process-{}", std::process::id());
    let builder_tag = tag.clone();
    let ts = gadk_mcp::new(Config::new(McpTransport::Command(Arc::new(move || {
        let mut cmd =
            tokio::process::Command::new(env!("CARGO_BIN_EXE_gadk-mcp-stdio-test-server"));
        cmd.arg(&builder_tag);
        cmd
    }))))
    .unwrap();
    // Lazy connect: `new` spawns nothing.
    assert_eq!(pids_tagged(&tag), Vec::<String>::new());

    let ctx = Context::builder().invocation_id("inv-1").build();
    let tool_ctx = ctx.with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        actions: Some(Default::default()),
        ..Default::default()
    });
    let mut want = Map::new();
    want.insert(
        "output".into(),
        json!({"weather_summary": "Today in \"Paris\" is sunny"}),
    );

    let tools = ts.tools(&ctx).await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(names, ["get_weather"]);
    assert!(tools[0].as_any().downcast_ref::<McpTool>().is_some());
    let weather = tools[0].as_runnable().unwrap();
    let got = weather
        .run(tool_ctx.clone(), json!({"city": "Paris"}))
        .await
        .unwrap();
    assert_eq!(got, want);

    let first = pids_tagged(&tag);
    assert_eq!(first.len(), 1, "{first:?}");
    let status = std::process::Command::new("kill")
        .args(["-9", &first[0]])
        .status()
        .unwrap();
    assert!(status.success());
    wait_for_pids(&tag, |pids| !pids.contains(&first[0])).await;

    // The call fails on the dead session; the refresher reconnects through the builder (a new
    // child) and retries once, so the caller sees a normal result.
    let got = weather
        .run(tool_ctx, json!({"city": "Paris"}))
        .await
        .unwrap();
    assert_eq!(got, want);
    let second = pids_tagged(&tag);
    assert_eq!(second.len(), 1, "{second:?}");
    assert_ne!(second, first);

    // Listing works on the new session too, and closing ends the child.
    let tools = ts.tools(&ctx).await.unwrap();
    assert_eq!(tools.len(), 1);
    ts.close().await.unwrap();
    wait_for_pids(&tag, <[String]>::is_empty).await;
}
