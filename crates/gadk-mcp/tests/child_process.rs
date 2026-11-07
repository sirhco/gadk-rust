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
//! calls a tool, kills the child, and checks that the next connect works (Task 7; Unix only).
//!
//! An integration test reaches only the public API, and the toolset's own `Command` factory is
//! private, so the test does what that factory does on every connect: run the transport's
//! `CommandBuilder` and serve the client over a fresh `TokioChildProcess`. The factory itself,
//! and its fresh child per connect, are unit-tested in `src/transport.rs`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gadk_mcp::rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation,
};
use gadk_mcp::rmcp::service::RunningService;
use gadk_mcp::rmcp::transport::TokioChildProcess;
use gadk_mcp::rmcp::{RoleClient, ServiceError};
use gadk_mcp::{McpTransport, serve_client};
use serde_json::json;

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
