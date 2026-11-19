# gadk-launcher

Run a gadk agent from the command line. A `Launcher` parses ADK-style command lines (flags,
help, error and log text match ADK's launchers) and runs one sub-launcher:

- `console`: an interactive chat on stdin and stdout;
- `web`: an HTTP server whose own sub-launchers add routes: `api` (the ADK REST API from
  `gadk-server`, under `/api` by default), `a2a` (the agent over the Agent2Agent protocol),
  `webui` (the embedded ADK web UI under `/ui/`), and `pubsub` / `eventarc` (push triggers).

`full::new()` builds `console` and `web` with `webui`, `a2a`, `pubsub`, `eventarc` and `api`;
`prod::new()` builds only `web` with `api` and `a2a`.
`agentengine::new_launcher` (feature `agentengine`) builds the launcher a Vertex AI Agent
Engine deployment runs. `Config` carries the agent loader, the session, artifact and memory
services (in-memory when `None`), plugins, telemetry options, inbound authentication and
authorization, allowed origins and context compaction.

With the facade, enable the `launcher` feature of `gadk` and use `gadk::launcher` (it also
enables `gadk::telemetry`, which the launcher uses for OpenTelemetry export).

## Example

```rust
// Inside `#[tokio::main] async fn main() -> anyhow::Result<()>`; `agent` is an
// `Arc<dyn gadk_agent::Agent>`.
use std::sync::Arc;
use gadk_agent::SingleLoader;
use gadk_launcher::golog::Logger;
use gadk_launcher::{CancellationToken, Config, Launcher, full};

let config = Config::new(Arc::new(SingleLoader::new(agent)));
let mut launcher = full::new();
let args: Vec<String> = std::env::args().skip(1).collect();
if let Err(e) = launcher.execute(CancellationToken::new(), config, args).await {
    Logger::stderr().print(&format!("Run failed: {e}\n\n{}", launcher.command_line_syntax()));
    std::process::exit(1);
}
```

Then:

```sh
my-agent                    # console (the first sub-launcher is the default)
my-agent web api webui      # REST API and web UI on http://localhost:8080/ui/
my-agent web -port 9000 api a2a
my-agent -h                 # every command line and flag
```

The `web` launcher binds `127.0.0.1` by default (`-host 0.0.0.0` to listen on all interfaces,
for example in a container) and port 8080 (`-port`); `-allow_origins` adds browser origins and
`-otel_to_cloud` turns on export to Google Cloud.

## Features

| Feature | Default | Adds |
|---|---|---|
| `web` | via `full` | The web server and the `api` sub-launcher |
| `a2a` | via `full` | The `a2a` sub-launcher (through `gadk-a2a`) |
| `webui` | via `full` | The embedded web UI sub-launcher |
| `triggers` | via `full` | The `pubsub` and `eventarc` sub-launchers |
| `prod` | no | `web` + `a2a`, and the `prod` preset |
| `full` | yes | All of the above except `prod`; the `full` preset |
| `agentengine` | no | The Agent Engine sub-launcher and `agentengine::new_launcher` |

The console is always built.
