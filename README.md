# gadk — Agent Development Kit for Rust

gadk is a Rust port of Google's [Agent Development Kit for Go](https://github.com/google/adk-go)
(ADK-Go). You use it to build, run, serve and deploy AI agents: LLM agents with tools, multi-agent
workflows, live (bidirectional streaming) sessions, persistent sessions and memory, an ADK REST
API server, the Agent2Agent (A2A) protocol, a launcher and a deploy CLI, and OpenTelemetry and
BigQuery observability.

The first goal is **parity with ADK-Go**: the same behaviour, wire formats, error texts and
outputs, checked against ADK-Go's own tests (see [Parity with ADK-Go](#parity-with-adk-go)). New
functionality beyond ADK-Go will follow.

- Pinned ADK-Go commit: [`docs/parity/ADK_GO_COMMIT`](docs/parity/ADK_GO_COMMIT)
- Rust: edition 2024, MSRV 1.95 (developed on the toolchain in `rust-toolchain.toml`)
- License: Apache-2.0 (see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE))

## Quick start

gadk is not published to crates.io yet; depend on it from Git:

```toml
[dependencies]
gadk = { git = "https://github.com/sirhco/adk-rust" }   # default features: gemini, gcp, mcp
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

    // In-memory session, artifact and memory services (Go `runner.NewInMemory`).
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
`function_node`, `Edge`) and the session, artifact and memory services. The API follows ADK-Go's
names, so ADK-Go's documentation and samples map directly onto it.

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
| `launcher` | | ADK-Go launcher: console and web (REST, A2A, web UI, triggers) (`gadk::launcher`) |
| `agentengine` | | Agent Engine server and launcher (`gadk::agentengine`) |
| `telemetry` | | OpenTelemetry export (`gadk::telemetry`) |
| `agentregistry` | | Google Cloud Agent Registry client (`gadk::agentregistry`) |
| `agentanalytics` | | BigQuery Agent Analytics plugin (`gadk::agentanalytics`) |

## Crates

| Crate | What it is | ADK-Go source |
|---|---|---|
| `gadk` | Façade: re-exports, features, `prelude` | — |
| `gadk-genai` | Gemini-compatible content and config types | `google.golang.org/genai` |
| `gadk-platform` | Time, UUID and task seams; Go formatting helpers | `platform` |
| `gadk-model` | `Llm` trait, model registry, HTTP seam, REST transport, SSE parser | `model` |
| `gadk-gemini` | Gemini API / Vertex AI models, live (WebSocket) connections | `model/gemini` |
| `gadk-anthropic` | Claude models | `model/anthropicmodel` |
| `gadk-openai` | OpenAI models | `model/openaimodel` |
| `gadk-session` | Sessions, events, in-memory session service, compaction config | `session` |
| `gadk-session-database` | sqlite / PostgreSQL session service | `session/database` |
| `gadk-artifact` | Artifact service API and in-memory service | `artifact` |
| `gadk-artifact-gcs` | GCS artifact service | `artifact/gcsartifact` |
| `gadk-memory` | Long-term memory service | `memory` |
| `gadk-vertexai` | Vertex AI Agent Engine sessions and memory | `session/vertexai`, `memory/vertexai` |
| `gadk-agent` | Agent API: agents, contexts, tools, callbacks, plugins, fibers, cancel causes | `agent`, `tool`, `plugin` |
| `gadk-engine` | LLM agent and flow, workflows, workflow agents, runner, built-in tools | `agent/llmagent`, `internal/llminternal`, `workflow`, `runner` |
| `gadk-tools` | Built-in tools that need no auth (Gemini tools, artifacts, memory, tool search, skills) | `tool/*` |
| `gadk-auth` | Credentials, providers, credential store, refresh-and-retry transport | `auth` |
| `gadk-mcp` | MCP toolset (on `rmcp`) | `tool/mcptoolset` |
| `gadk-server` | ADK REST API server, authn, authz, Agent Engine server | `server/adkrest`, `server/authn`, `server/authz`, `server/agentengine` |
| `gadk-a2a` | Agent2Agent server and remote agent (on the A2A Rust SDK) | `server/adka2a/v2`, `agent/remoteagent/v2` |
| `gadk-launcher` | Console and web launchers, web UI, triggers | `cmd/launcher` |
| `gadk-cli` | `gadk` deploy CLI (Cloud Run, Agent Engine) | `cmd/adkgo` |
| `gadk-agentregistry` | Agent Registry client | `agentregistry` |
| `gadk-telemetry` | OpenTelemetry providers, OTLP and GCP export | `telemetry` |
| `gadk-agentanalytics` | BigQuery Agent Analytics plugin ([README](crates/gadk-agentanalytics/README.md)) | `plugin/agentanalytics` |
| `gadk-goyaml` | Byte-identical port of `gopkg.in/yaml.v3` (internal) ([README](crates/gadk-goyaml/README.md)) | `gopkg.in/yaml.v3` |
| `gadk-configurable` | YAML agent configs, conformance record/replay, `adkcli` (internal) ([README](crates/gadk-configurable/README.md)) | `internal/configurable`, `cmd/internal/adkcli` |
| `gadk-httprr` | HTTP record/replay for deterministic tests | `internal/httprr` |

Binaries: `gadk` (the deploy CLI, [README](crates/gadk-cli/README.md)) and `adkcli` (the
conformance CLI in `gadk-configurable`).

Not ported: ADK-Go's `exp/` packages, the deprecated A2A v1 packages and symbols ADK-Go marks
deprecated.

## Parity with ADK-Go

The port was built in sub-projects, and each one has a ledger in [`docs/parity/`](docs/parity/).
A ledger lists every in-scope ADK-Go test, the Rust test that ports it (or why it is skipped),
and the accepted divergences: places where gadk deliberately behaves differently from Go. Each
divergence is marked in the source with `// Divergence (ledger <name>)`.

- `scripts/parity-check` checks the ledgers against the pinned ADK-Go checkout. Ported tests carry
  `// parity:` annotations, and the ledger rows are generated from them.
- Go probes in `scripts/gen-go-*` run ADK-Go itself to record golden fixtures: wire exchanges,
  rows, YAML bytes and CLI output. `scripts/check-fixtures.sh` checks the fixture counts and,
  when `go` is installed, re-runs the probes and requires identical output.
- [`docs/parity/genai-inventory.md`](docs/parity/genai-inventory.md) tracks the ported
  `google.golang.org/genai` surface.

Run the checks against an ADK-Go checkout (by default the sibling `../adk-go`):

```sh
ADK_GO_DIR=../adk-go python3 scripts/parity-check --ledger all
ADK_GO_DIR=../adk-go scripts/check-fixtures.sh
```

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
python3 scripts/parity_check_test.py
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" \
  cargo doc --workspace --no-deps --all-features
```

Conventions:
- Every source file has the Apache-2.0 header, and a ported file names its ADK-Go source.
- Non-test code uses no `unsafe`, `unwrap`, `expect` or `panic`.
- Dependencies come from `[workspace.dependencies]`, and `scripts/check-workspace-deps.py`
  enforces the allowed crate-to-crate edges.
- Tests that depend on time use Tokio's paused clock.

## License

Apache-2.0. gadk contains code derived from ADK-Go and the Google Gen AI Go SDK, and vendors
the ADK web UI and googleapis protocol buffers; see [`NOTICE`](NOTICE).
