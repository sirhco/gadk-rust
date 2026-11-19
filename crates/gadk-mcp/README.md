# gadk-mcp

The tools of a Model Context Protocol (MCP) server as a gadk toolset, built on
[`rmcp`](https://crates.io/crates/rmcp) (re-exported as `gadk_mcp::rmcp`). `new` turns a
`Config` into an `McpToolset`: it lists the server's tools (`tools/list`, every page) and exposes
each one as an `McpTool` whose declaration carries the server's input and output schemas
unchanged. Running a tool calls `tools/call` and shapes the result the way ADK does
(`{"output": …}`, `Tool execution failed.` when the server reports `isError`, and
`format_mcp_content` for text, resources, images and audio).

Transports (`McpTransport`):

- `Endpoint(url)`: streamable HTTP with a default client;
- `StreamableHttp { url, client, headers }`: streamable HTTP with your `reqwest::Client` and
  extra headers;
- `Command(builder)`: a child process speaking MCP over stdio;
- `Custom(factory)`: any rmcp client transport through a `TransportFactory` (with
  `serve_client`).

The legacy SSE client transport is not supported. Most applications use this crate through the
`gadk` façade (`gadk::mcp`, feature `mcp`, on by default).

## Example

```rust
use std::sync::Arc;
use gadk_mcp::{Config, McpToolset, McpTransport};

// Inside an async fn returning Result.
let mut cfg = Config::new(McpTransport::Command(Arc::new(|| {
    let mut cmd = tokio::process::Command::new("npx");
    cmd.args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]);
    cmd
})));
cfg.prefix = "fs".into(); // tools appear as fs_<name>
let toolset: Arc<McpToolset> = Arc::new(gadk_mcp::new(cfg)?);

// LlmAgentConfig { toolsets: vec![toolset.clone()], .. }
// … run the agent …
toolset.close().await?;
```

## Behaviour

- The session connects lazily on first use. An operation that fails because the connection was
  lost pings the session, reconnects once and retries.
- `Config::auth` (streamable HTTP only) applies a `gadk-auth` credential provider to every
  request. The toolset keeps one MCP session per distinct credential, closes sessions idle for
  10 minutes and keeps at most 64 open.
- `Config::prefix` exposes each tool as `<prefix>_<name>`, so two servers' same-named tools can
  serve one agent; the server is still called with its own name. `tool_filter` sees the server's
  names.
- `require_confirmation` / `require_confirmation_provider` gate calls behind a
  Human-in-the-Loop confirmation; `metadata_provider` supplies `_meta` for each call.
- Every HTTP request carries the ADK tracking headers (`user-agent`, `x-goog-api-client`).
- `to_mcp_tool` goes the other way: it turns a gadk tool into an MCP tool definition.
- Close a toolset with `McpToolset::close` when done; dropping it cancels its sessions without
  waiting.

## Features

- `testutil`: in-process MCP test servers (`gadk_mcp::testutil`) and the
  `gadk-mcp-stdio-test-server` binary.
