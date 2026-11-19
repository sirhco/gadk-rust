# gadk-server

The ADK REST API server for gadk agents, wire-compatible with ADK REST clients such as the ADK
web UI. `rest::Server` serves sessions, runs (`/run`, `/run_sse`, `/run_live`), apps, artifacts,
version and, optionally, the debug trace and agent-graph routes as an `axum::Router` that you
mount on any axum or hyper server. Pub/Sub and Eventarc push-trigger handlers live in
`rest::triggers`.

Inbound requests can be authenticated and authorized:

- `authn`: an `Authenticator` resolves a request to a `Caller` (401 without valid credentials,
  403 for a refused principal). Built-ins: `noop`, `header` (user ID from a header),
  `identity_aware_proxy` (Cloud IAP headers), `google_oidc` (Google-signed ID tokens from
  allowed service accounts) and `custom` (a closure).
- `authz`: an `Authorizer` decides whether the caller may act as the user a request names.
  Built-ins: `noop` (allow all) and `strict` (only as itself).

The `agentengine` module (feature `agentengine`, off by default) is the Vertex AI Agent Engine
server: the `/reasoning_engine` and `/stream_reasoning_engine` handler an Agent Engine
deployment calls. `mux` holds routing plumbing shared with `gadk-launcher` and is not a stable
API.

Most applications get the server through the launcher (`gadk-launcher`, `web api`). To embed it
directly, enable the `server` feature of `gadk` and use `gadk::server`.

## Example

```rust
// Inside an async fn returning Result; `agent` is an `Arc<dyn gadk_agent::Agent>`.
use std::sync::Arc;
use gadk_agent::SingleLoader;
use gadk_server::rest::{LocalAddr, Server, ServerConfig};
use gadk_server::{authn, authz};
use gadk_session::InMemorySessionService;

let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
cfg.agent_loader = Some(Arc::new(SingleLoader::new(agent)));
cfg.bind_host = "127.0.0.1:8080".into();
cfg.allowed_origins = vec!["http://localhost:4200".into()];
cfg.authenticator = Some(authn::header("X-User-Id"));
cfg.authorizer = Some(authz::strict());

let server = Server::new(cfg)?;
let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
// The origin guard reads the address each connection was accepted on.
axum::serve(
    listener,
    server.router().into_make_service_with_connect_info::<LocalAddr>(),
)
.await?;
```

## Configuration notes

- `ServerConfig::new` sets the session service and leaves every other field at its default.
  Without an artifact service, the artifact routes answer 503.
- `bind_host` set to a loopback address turns on the DNS-rebinding Host check;
  `allowed_origins` lists the browser origins allowed to call the server (`*` turns the origin
  checks off).
- `max_payload_size` limits request bodies (zero or negative: 10 MiB).
- `debug_api_config.include_debug_api` serves the debug and agent-graph routes, which expose
  tool arguments and responses; do not enable it in production. Install `Server::debug_layer`
  in your `tracing` subscriber to feed them.

## Features

| Feature | Default | Adds |
|---|---|---|
| `agentengine` | no | `agentengine`: the Agent Engine server |
| `testutil` | no | Test fixtures (internal) |
