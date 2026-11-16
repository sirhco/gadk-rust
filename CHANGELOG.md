# Changelog

All notable changes to gadk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor version may break
the public API.

## [Unreleased]

## [0.1.0] - 2026-11-15

The first version: a Rust port of ADK-Go (main `0990f9e`, 2026-11-13) aimed at behaviour, wire
and output parity with ADK-Go, plus a prompt registry. Not published to crates.io. Where gadk
deliberately differs from ADK-Go, the source is marked `// Divergence (ledger …)`.

### Added

- **Foundation:**
  - `gadk-genai`: Gemini-compatible content and config types, mirroring
    google.golang.org/genai v1.77.0.
  - `gadk-platform`: time, UUID and task seams, Go formatting helpers and `safe_subpath`.
  - `gadk-model`: the `Llm` trait, the model registry, the HTTP seam and a shared REST
    transport.
  - `gadk-gemini`: the Gemini API and Vertex AI.
  - `gadk-session`: sessions, events (including requested auth configs) and the in-memory
    session service.
  - `gadk-httprr`: HTTP record/replay.
  - The `gadk` façade with its `prelude`.
- **Engine:**
  - `gadk-agent`: the agent API, covering contexts, callbacks (including the agent error
    callback), tools and plugins.
  - `gadk-engine`: the LLM agent and flow; sequential, parallel and loop agents; graph
    workflows; and the runner. `AgentTool` sub-runs inherit the parent runner's plugins.
  - Live (bidirectional streaming) sessions over WebSocket, with proxy support.
  - Context compaction.
  - `gadk-artifact` (with `artifact://` references) and `gadk-memory` (with incremental
    memory).
- **Tools:**
  - `gadk-tools`: built-in tools that need no authentication (Gemini tools, artifacts, memory,
    examples with pluggable example providers, tool search, skills) and the built-in plugins.
  - `gadk-auth`: credentials, the credential store and a refresh-and-retry transport.
  - `gadk-mcp`: MCP toolsets, with tool name prefixes and gadk-to-MCP tool conversion.
- **Models:**
  - `gadk-anthropic`: Claude on the Anthropic API, on Vertex AI and through Apigee.
  - `gadk-openai`: the Responses and Chat Completions APIs.
- **Persistence:**
  - `gadk-session-database`: sqlite and PostgreSQL sessions.
  - `gadk-artifact-gcs`: GCS artifacts.
  - `gadk-vertexai`: Vertex AI Agent Engine sessions and memory, and a Vertex AI Example Store
    example provider.
- **Servers:**
  - `gadk-server`: the ADK REST API, Pub/Sub and Eventarc triggers, inbound authn/authz, and
    the Agent Engine server. It is wire-compatible with an ADK-Go server.
  - `gadk-a2a`: the Agent2Agent v2 server and remote agent.
  - `gadk-agentregistry`: an Agent Registry client.
- **Launcher and CLIs:**
  - `gadk-launcher`: console and web launchers with ADK-Go's command lines, the ADK web UI,
    triggers and the Agent Engine launcher.
  - `gadk-cli`: the `gadk deploy cloudrun | agentengine` deploy CLI, with Agent Engine
    deployment environment variables (`--env`, `--default_env`).
  - `gadk-goyaml`: a byte-identical port of `gopkg.in/yaml.v3`.
  - `gadk-configurable`: YAML agent configs, conformance record/replay and the `adkcli`
    binary.
- **Observability:**
  - `gadk-telemetry`: OpenTelemetry tracer, logger and meter providers, with OTLP/HTTP and
    Google Cloud export.
  - `gadk-agentanalytics`: the BigQuery Agent Analytics plugin. It covers config and schema,
    table creation and upgrade, views, the Storage Write client, the batch processor, the GCS
    offloader, the plugin and its events, and the trace manager.
- **Prompt registry:**
  - `gadk-prompts`: versioned, labelled prompts; `FileStore`; `PromptRegistry` with polling
    refresh, `InstructionProvider`s rendered with session-state templating, a `render_prompt`
    span and refresh metrics.
  - `gadk-prompts-firestore`: a Firestore REST store.
  - `gadk-prompts-cli`: the `gadk-prompts` CLI (push, promote, unlabel, list, show, diff).
  - `gadk_engine::llm::inject_session_state`: session-state templating for instruction
    providers.
  - Façade features `prompts` and `prompts-firestore` (not in `default`).
- **Context support** in `gadk-agent`, standing in for Go's goroutines and contexts:
  - `fiber`, for execution identity.
  - Cancel causes: `CancelCause`, `InvocationContext::{cancel_with_cause, cause}` and
    `CancelGuard::release`.
  - `Context::context_path`.
  - The per-invocation teardown registry, `Teardowns`.
- **Tooling:** `scripts/check-workspace-deps.py` enforces the crate dependency rules.

### Behaviour notes

- Dropping a runner stream early follows Go's unwinding instead of aborting. Work the run had
  already started is cancelled plainly and finishes before the after-run callbacks. That work
  includes tool calls, ParallelAgent branches and workflow node tasks. A tool that ignores
  cancellation therefore delays after-run until it returns, as Go's `RunTasks` waits for it.
  See `Runner::run`.
- `BeforeToolCallback` returns the (possibly edited) tool arguments with its result, mirroring
  `BeforeModelCallback`.
- `gadk_server::rest::DebugSpan::attributes` keeps JSON types.
- Accepted differences from ADK-Go are marked in the source with `// Divergence (ledger …)`,
  for example in error texts that come from Rust crates and in limits Rust cannot reach.

### Not included

- ADK-Go's `exp/` packages, the deprecated A2A v1 packages and symbols ADK-Go marks deprecated.
- ADK-Go's BigQuery toolset, Agent Engine client and Vertex AI RAG memory service.

### Known issues

- `deny.toml` ignores RUSTSEC-2026-0459 (`libsqlite3-sys`, through `sqlx-sqlite`) until sqlx
  moves to a fixed version.
- Some `gadk-goyaml` type-name error texts differ from yaml.v3.
- The live GCS artifact test has not been run against a real bucket.

[Unreleased]: https://github.com/sirhco/gadk-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sirhco/gadk-rust/releases/tag/v0.1.0
