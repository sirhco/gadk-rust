# gadk-agentregistry

A client for the Google Cloud Agent Registry (`agentregistry.googleapis.com`), a governed
catalog of A2A agents, MCP servers and model endpoints. `Client` lists and fetches registered
agents, MCP servers and endpoints, and turns a registered entry into something an agent can use:
`Client::remote_agent` builds a gadk remote A2A agent (feature `a2a`) and `Client::mcp_toolset`
builds an MCP toolset over streamable HTTP (feature `mcp`).

Registry calls are authenticated with Application Default Credentials (cloud-platform scope)
unless `Config::http_client` supplies a client that manages its own credentials; with ADC the
endpoint switches to the mTLS one according to `GOOGLE_API_USE_MTLS_ENDPOINT`. A2A egress
(`remote_agent`) never reuses the registry's ADC client: it uses the client and headers given
in its options, or a default client.

With the facade, enable the `agentregistry` feature of `gadk` and use `gadk::agentregistry`.

## Example

```rust
// Inside an async fn returning Result.
use futures::StreamExt;
use gadk_agentregistry::{Client, Config, with_page_size};

let registry = Client::new(Config {
    project_id: "my-project".into(),
    location: "us-central1".into(),
    ..Default::default()
})
.await?;

// Every registered agent, across pages.
let mut agents = registry.all_agents(&[with_page_size(50)]);
while let Some(agent) = agents.next().await {
    let agent = agent?;
    println!("{} ({})", agent.display_name, agent.name);
}

// A registered A2A agent as a sub-agent, and a registered MCP server as a toolset.
let helper = registry
    .remote_agent("projects/my-project/locations/us-central1/agents/my-agent", &[])
    .await?;
let tools = registry
    .mcp_toolset("projects/my-project/locations/us-central1/mcpServers/my-server", &[])
    .await?;
```

## API overview

- Listing: `list_agents`, `list_mcp_servers`, `list_endpoints` fetch one page;
  `all_agents`, `all_mcp_servers`, `all_endpoints` stream every page. Narrow them with
  `with_filter`, `with_page_size` and `with_page_token`.
- Fetching by full resource name: `get_agent`, `get_mcp_server`, `get_endpoint`.
- Egress options: `with_a2a_http_client` / `with_a2a_headers` for `remote_agent`,
  `with_mcp_http_client` / `with_mcp_headers` for `mcp_toolset`.
- Errors: `Error`, with `Error::Api` carrying the service's `ApiError` for a non-2xx answer.

## Features

| Feature | Default | Adds |
|---|---|---|
| `a2a` | yes | `Client::remote_agent` (through `gadk-a2a`) |
| `mcp` | yes | `Client::mcp_toolset` (through `gadk-mcp`) |
