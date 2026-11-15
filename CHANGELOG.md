# Changelog

All notable changes to gadk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor version may break
the public API.

## [Unreleased]

## [0.1.0] - 2026-11-14

The first version: a port of ADK-Go at the commit in `docs/parity/ADK_GO_COMMIT`, aimed at
behaviour, wire and output parity with ADK-Go. Not published to crates.io. Each area below lists
its parity ledger in `docs/parity/`. A ledger maps every in-scope ADK-Go test to its Rust port and
lists the accepted divergences, each marked in the source with `// Divergence (ledger …)`.

### Added

- **Foundation** (`1a`):
  - `gadk-genai`: Gemini-compatible content and config types.
  - `gadk-platform`: time, UUID and task seams, plus Go formatting helpers.
  - `gadk-model`: the `Llm` trait, the model registry and the HTTP seam.
  - `gadk-gemini`: the Gemini API and Vertex AI.
  - `gadk-session`: sessions, events and the in-memory session service.
  - `gadk-httprr`: HTTP record/replay.
  - The `gadk` façade with its `prelude`.
- **Engine** (`1b`, `1b-live`, `1c`):
  - `gadk-agent`: the agent API, covering contexts, callbacks, tools and plugins.
  - `gadk-engine`: the LLM agent and flow; sequential, parallel and loop agents; graph workflows; and the runner.
  - Live (bidirectional streaming) sessions over WebSocket, with proxy support.
  - Context compaction.
  - `gadk-artifact` and `gadk-memory`.
- **Tools** (`2a`, `2b`, `2c`):
  - `gadk-tools`: built-in tools that need no authentication (Gemini tools, artifacts, memory, examples, tool search, skills) and the built-in plugins.
  - `gadk-auth`: credentials, the credential store and a refresh-and-retry transport.
  - `gadk-mcp`: MCP toolsets.
- **Models** (`3a`, `3b`):
  - `gadk-anthropic`: Claude on the Anthropic API, on Vertex AI and through Apigee.
  - `gadk-openai`: the Responses and Chat Completions APIs.
  - A shared REST transport in `gadk-model`.
- **Persistence** (`4a`, `4b`, `4c`):
  - `gadk-session-database`: sqlite and PostgreSQL sessions.
  - `gadk-artifact-gcs`: GCS artifacts.
  - `gadk-vertexai`: Vertex AI Agent Engine sessions and memory.
- **Servers** (`5a`, `5b`, `5c`):
  - `gadk-server`: the ADK REST API, Pub/Sub and Eventarc triggers, inbound authn/authz, and the Agent Engine server. It is wire-compatible with an ADK-Go server.
  - `gadk-a2a`: the Agent2Agent v2 server and remote agent.
  - `gadk-agentregistry`: an Agent Registry client.
- **Launcher and CLIs** (`6`, `6b`, `6c-1`, `6c-2`):
  - `gadk-launcher`: console and web launchers with ADK-Go's command lines, the ADK web UI, triggers and the Agent Engine launcher.
  - `gadk-cli`: the `gadk deploy cloudrun | agentengine` deploy CLI.
  - `gadk-goyaml`: a byte-identical port of `gopkg.in/yaml.v3`.
  - `gadk-configurable`: YAML agent configs, conformance record/replay and the `adkcli` binary.
- **Observability** (`7a`, `7b-1`, `7b-2`, `7b-3a`, `7b-3b`):
  - `gadk-telemetry`: OpenTelemetry tracer and logger providers, with OTLP/HTTP and Google Cloud export.
  - `gadk-agentanalytics`: the BigQuery Agent Analytics plugin. It covers config and schema, table creation and upgrade, views, the Storage Write client, the batch processor, the GCS offloader, the plugin and its events, and the trace manager with Go's goroutine attribution.
- **Context support** in `gadk-agent`, standing in for Go's goroutines and contexts:
  - `fiber`, for execution identity.
  - Cancel causes: `CancelCause`, `InvocationContext::{cancel_with_cause, cause}` and `CancelGuard::release`.
  - `Context::context_path`.
  - The per-invocation teardown registry, `Teardowns`.
- **Parity tooling:**
  - `scripts/parity-check` and its ledgers.
  - Go probes (`scripts/gen-go-*`) that record ADK-Go's own output as golden fixtures.
  - `scripts/check-fixtures.sh` and `scripts/check-workspace-deps.py`.

### Behaviour notes

- Dropping a runner stream early follows Go's unwinding instead of aborting. Work the run had
  already started is cancelled plainly and finishes before the after-run callbacks. That work
  includes tool calls, ParallelAgent branches and workflow node tasks. A tool that ignores
  cancellation therefore delays after-run until it returns, as Go's `RunTasks` waits for it.
  See `Runner::run`.
- The ledgers record where gadk deliberately differs from ADK-Go, for example in error texts
  that come from Rust crates and in limits Rust cannot reach.

### Not included

- ADK-Go's `exp/` packages, the deprecated A2A v1 packages and symbols ADK-Go marks deprecated.

### Known issues

- `deny.toml` ignores RUSTSEC-2026-0459 (`libsqlite3-sys`, through `sqlx-sqlite`) until sqlx
  moves to a fixed version.
- Some `gadk-goyaml` type-name error texts differ from yaml.v3.
- The live GCS artifact test has not been run against a real bucket.

[Unreleased]: https://github.com/sirhco/gadk-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sirhco/gadk-rust/releases/tag/v0.1.0
