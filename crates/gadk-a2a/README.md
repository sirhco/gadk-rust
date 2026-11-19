# gadk-a2a

Agent2Agent (A2A) protocol support for gadk, built on the official A2A Rust SDK (`a2a-lf`,
`a2a-server-lf`, `a2a-client-lf`). It works in both directions: serve a gadk agent to A2A
clients, and use a remote A2A agent as a gadk agent inside your own agent tree. It is
wire-compatible with ADK A2A servers and clients: the metadata keys, extension marker and error
texts are the ones ADK uses.

- `convert` (always built): ADK and A2A part, event, metadata and extension conversions.
- `server` (feature `server`): `Executor`, an `a2a_server::AgentExecutor` that runs a gadk agent
  and streams its events as A2A task, status and artifact updates (`OutputMode` picks one
  artifact per run or per event); `build_agent_skills` for the agent card; and the identity
  header bridge (`IDENTITY_HEADER`, `a2a_identity_layer`).
- `remoteagent` (feature `client`): `new_a2a` builds a gadk agent backed by a remote A2A agent
  from an `A2AConfig`, with agent-card resolution (`new_agent_card_provider`), credential scoping
  and cleanup of abandoned remote tasks.

The a2a-rs crates are re-exported as `gadk_a2a::a2a`, `a2a_client` and `a2a_server`. Through the
`gadk` facade this crate is `gadk::a2a` (feature `a2a`, not on by default).

## Features

| Feature | Default | Enables |
|---|---|---|
| `server` | yes | The `server` module (adds `a2a-server-lf` and `axum`) |
| `client` | yes | The `remoteagent` module (adds `a2a-client-lf` and `reqwest`) |
| `gadk-server` | no | `server::caller_user`, reading the user of gadk-server's authn layer |
| `testutil` | no | Test fixtures: a scripted runner, a fake A2A client and an in-process server |

## Example

Use a remote A2A agent as a sub-agent; its card is fetched from
`<url>/.well-known/agent-card.json`:

```rust
use gadk_a2a::remoteagent::{A2AConfig, CardResolveOptions, new_a2a, new_agent_card_provider};

let weather = new_a2a(A2AConfig {
    name: "weather".into(),
    description: "Answers questions about the weather.".into(),
    agent_card_provider: Some(new_agent_card_provider(
        "https://weather.example.com",
        CardResolveOptions::default(),
    )),
    ..Default::default()
})?;
// `weather` is an `Arc<dyn Agent>`: add it to a parent agent's sub-agents.
```

To serve an agent, wrap a `gadk_engine::runner::RunnerConfig` in `server::Executor` and put it
behind a2a-rs's `DefaultRequestHandler` and JSON-RPC router; the `Executor` rustdoc has a
complete example.
