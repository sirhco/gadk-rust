# gadk — Agent Development Kit for Rust

gadk is an agent development kit for Rust, modelled on Google's
[Agent Development Kit](https://google.github.io/adk-docs/) (ADK). You use it to build, run, serve
and deploy AI agents: LLM agents with tools, multi-agent workflows, live (bidirectional streaming)
sessions, persistent sessions and memory, an ADK REST API server, the Agent2Agent (A2A) protocol, a
launcher and a deploy CLI, and OpenTelemetry and BigQuery observability.

- Rust: edition 2024, MSRV 1.95 (developed on the toolchain in `rust-toolchain.toml`)
- License: Apache-2.0 (see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE))
- Changes: [`CHANGELOG.md`](CHANGELOG.md)

## Quick start

Add gadk from crates.io:

```toml
[dependencies]
gadk = "0.4"   # default features: gemini, gcp, mcp
tokio = { version = "1", features = ["full"] }
futures = "0.3"
```

```rust,ignore
use futures::StreamExt;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Gemini API key from GOOGLE_API_KEY / GEMINI_API_KEY, or Vertex AI with
    // GOOGLE_GENAI_USE_VERTEXAI, GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION and ADC.
    let model = GeminiModel::new("gemini-2.5-flash", ClientConfig::default()).await?;
    let agent = llm_agent::new(LlmAgentConfig {
        name: "helper".into(),
        description: "Answers questions.".into(),
        model: Some(model),
        instruction: "Answer briefly.".into(),
        ..Default::default()
    })?;

    // In-memory session, artifact and memory services.
    let runner = Runner::in_memory("quickstart", agent)?;
    let msg = Content::from_text("What is the capital of France?", gadk::genai::ROLE_USER);
    let mut events = runner.run("user", "session-1", Some(msg), RunConfig::default(), RunOptions::default());
    while let Some(event) = events.next().await {
        if let Some(content) = &event?.response.content {
            for part in &content.parts {
                print!("{}", part.text);
            }
        }
    }
    runner.close().await?;
    Ok(())
}
```

`gadk::prelude` also brings in tools (`FunctionTool`, `AgentTool`), workflow agents
(`sequential_agent`, `parallel_agent`, `loop_agent`), graph workflows (`workflow_agent`,
`function_node`, `Edge`) and the session, artifact and memory services. The API follows ADK's
names, so ADK's documentation and samples map directly onto it.

## Façade features

The `gadk` crate re-exports the member crates as modules (`gadk::agent`, `gadk::engine`,
`gadk::session`, …). Optional parts are behind features:

| Feature | Default | Adds |
|---|---|---|
| `gemini` | yes | Gemini API and Vertex AI models (`gadk::gemini`) |
| `gcp` | yes | Google Cloud credentials in `gadk::auth` |
| `mcp` | yes | Model Context Protocol toolsets (`gadk::mcp`) |
| `anthropic` | | Claude models, Anthropic API and Vertex AI (`gadk::anthropic`) |
| `openai` | | OpenAI Responses and Chat Completions models (`gadk::openai`) |
| `database` | | Relational-database sessions: sqlite, or PostgreSQL (`gadk::session_database`) |
| `gcs` | | Artifacts in Google Cloud Storage (`gadk::artifact_gcs`) |
| `vertexai` | | Sessions and memory in Vertex AI Agent Engine (`gadk::vertexai`) |
| `server` | | ADK REST API server with authn/authz (`gadk::server`) |
| `a2a` | | Agent2Agent protocol: serve agents and call remote agents (`gadk::a2a`) |
| `launcher` | | Launcher: console and web (REST, A2A, web UI, triggers) (`gadk::launcher`) |
| `agentengine` | | Agent Engine server and launcher (`gadk::agentengine`) |
| `telemetry` | | OpenTelemetry export (`gadk::telemetry`) |
| `agentregistry` | | Google Cloud Agent Registry client (`gadk::agentregistry`) |
| `agentanalytics` | | BigQuery Agent Analytics plugin (`gadk::agentanalytics`) |
| `eval` | | Offline agent evaluation: eval sets, scorers, LLM judge (`gadk::eval`) |

## Crates

| Crate | What it is |
|---|---|
| `gadk` | Façade: re-exports, features, `prelude` |
| `gadk-genai` | Gemini-compatible content and config types |
| `gadk-platform` | Time, UUID, task and environment seams; shared helpers |
| `gadk-model` | `Llm` trait, model registry, HTTP seam, REST transport, SSE parser |
| `gadk-gemini` | Gemini API / Vertex AI models, live (WebSocket) connections |
| `gadk-anthropic` | Claude models |
| `gadk-openai` | OpenAI models |
| `gadk-session` | Sessions, events, in-memory session service, compaction config |
| `gadk-session-database` | sqlite / PostgreSQL session service |
| `gadk-artifact` | Artifact service API and in-memory service |
| `gadk-artifact-gcs` | GCS artifact service |
| `gadk-memory` | Long-term memory service |
| `gadk-vertexai` | Vertex AI Agent Engine sessions and memory |
| `gadk-agent` | Agent API: agents, contexts, tools, callbacks, plugins, fibers, cancel causes |
| `gadk-engine` | LLM agent and flow, workflows, workflow agents, runner, built-in tools |
| `gadk-tools` | Built-in tools that need no auth (Gemini tools, artifacts, memory, tool search, skills) |
| `gadk-auth` | Credentials, providers, credential store, refresh-and-retry transport |
| `gadk-mcp` | MCP toolset (on `rmcp`) |
| `gadk-server` | ADK REST API server, authn, authz, Agent Engine server |
| `gadk-a2a` | Agent2Agent server and remote agent (on the A2A Rust SDK) |
| `gadk-launcher` | Console and web launchers, web UI, triggers |
| `gadk-cli` | `gadk` deploy CLI (Cloud Run, Agent Engine) |
| `gadk-agentregistry` | Agent Registry client |
| [`gadk-telemetry`](crates/gadk-telemetry/README.md) | OpenTelemetry providers, OTLP and GCP export (Cloud Trace, Cloud Monitoring, Cloud Logging) |
| `gadk-agentanalytics` | BigQuery Agent Analytics plugin ([README](crates/gadk-agentanalytics/README.md)) |
| `gadk-goyaml` | YAML library that reads and writes YAML exactly as ADK does (internal) ([README](crates/gadk-goyaml/README.md)) |
| `gadk-configurable` | YAML agent configs, conformance record/replay, `adkcli` (internal) ([README](crates/gadk-configurable/README.md)) |
| `gadk-httprr` | HTTP record/replay for deterministic tests |
| [`gadk-cloudtrace`](crates/gadk-cloudtrace/README.md) | Cloud Trace v1 read client (list and get traces), used by the console and the eval monitors |
| [`gadk-console`](crates/gadk-console/README.md) | Local web console: Cloud Trace traces with their Cloud Logging entries, and the prompt registry |
| [`gadk-eval`](crates/gadk-eval/README.md) | Offline agent evaluation: YAML eval sets, deterministic, tool-trajectory and LLM-judge scorers, per-variant reports |
| [`gadk-eval-monitor`](crates/gadk-eval-monitor/README.md) | Production eval monitors: grade logged runs from the BigQuery analytics table or Cloud Trace with gadk-eval scorers; the `gadk-eval` CLI |

Binaries: `gadk` (the deploy CLI, [README](crates/gadk-cli/README.md)), `gadk-console` (the
local web console, [README](crates/gadk-console/README.md)), `gadk-eval` (production eval
monitors, [README](crates/gadk-eval-monitor/README.md)) and `adkcli` (the conformance CLI in
`gadk-configurable`).

Not included: ADK's experimental (`exp`) packages, the deprecated A2A v1 packages and the symbols
ADK marks deprecated.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p gadk-engine --no-default-features --all-targets -- -D warnings
cargo clippy -p gadk-server --no-default-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo deny check
cargo +1.95 build --workspace --all-features     # MSRV
python3 scripts/check-workspace-deps.py && python3 scripts/check_workspace_deps_test.py
python3 scripts/check-go-refs.py && python3 scripts/check_go_refs_test.py
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" \
  cargo doc --workspace --no-deps --all-features
```

Conventions:
- Every source file has the Apache-2.0 header, and a file with code derived from ADK carries a
  `Portions derived from` line.
- Non-test code uses no `unsafe`, `unwrap`, `expect` or `panic`.
- Dependencies come from `[workspace.dependencies]`, and `scripts/check-workspace-deps.py`
  enforces the allowed crate-to-crate edges.
- Tests that depend on time use Tokio's paused clock.

## License

Apache-2.0. gadk contains code derived from ADK-Go and the Google Gen AI Go SDK (both Apache-2.0)
and vendors the ADK web UI and googleapis protocol buffers; see [`NOTICE`](NOTICE).
