# Changelog

All notable changes to gadk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor version may break
the public API.

## [Unreleased]

## [0.2.2] - 2026-11-17

### Added

- `PromptStore::create_version_reporting`: like `create_version`, and also reports whether a
  version was written. It has a default body, so existing implementations keep compiling;
  `FileStore` and `FirestoreStore` implement it natively.

### Changed

- `gadk-prompts push` no longer reads the whole store to tell whether a version was created.
- `FirestoreStore::load_all` keeps the version documents it has read and fetches only new ones
  by name, instead of querying every version of every prompt on each call. An unchanged store
  now costs one read per prompt per poll.
- `gadk-launcher`'s embedded web UI module describes the bundle as a prebuilt google/adk-web
  build (see `NOTICE`).

## [0.2.1] - 2026-11-17

The first version published to crates.io. No code changes.

### Changed

- Every published crate has a crates.io description and keywords; the README and the
  `gadk-cli`, `gadk-console` and `gadk-prompts-cli` READMEs install from crates.io.
  `gadk-goyaml` and `gadk-configurable` stay unpublished.
- Documentation and code comments describe gadk in its own terms instead of as a port of ADK-Go
  (compatibility notes kept where ADK interoperability is a feature). No code changes; the new
  `scripts/check-go-refs.py` check runs in CI.
- Every published crate ships `LICENSE` and `NOTICE`. `NOTICE` describes gadk as an Agent
  Development Kit for Rust and now also credits google-api-go-client (BSD-3-Clause), from which
  `gadk-server`'s JWKS handling is derived; `gadk-server` ships that license as
  `LICENSE-google-api-go-client`. `gadk-server` and `gadk-agentanalytics` (which vendors the
  BSD-3-Clause protobuf well-known types) declare `Apache-2.0 AND BSD-3-Clause`.

## [0.2.0] - 2026-11-16

Observability and evaluation: Cloud Logging export, a local web console, and an offline
evaluator. Not published to crates.io.

### Added

- `gadk-telemetry` exports log records to Cloud Logging with `with_otel_to_cloud(true)`, each
  entry correlated with its Cloud Trace span (`trace`, `spanId`, `traceSampled`). New options
  `with_cloud_log_name` (default `gadk`) and `with_cloud_logging_stdout()` /
  `GADK_TELEMETRY_LOGS_STDOUT` (structured JSON on stdout instead of the API). Entries over
  256,000 bytes are shortened and marked `gadk.truncated`.
- `gadk-console`, a local web console (`gadk-console --project <p> [--store <url>] [--port <n>]
  [--log-id <id>]`, on 127.0.0.1:8765 by default). It lists Cloud Trace traces (filter by time
  range, root span name, agent and session), shows each trace as a span waterfall with its
  Cloud Logging entries attached by span id, links `render_prompt` spans to their prompt
  version, and browses a `gadk-prompts` store (file or Firestore): versions, diffs, and setting
  or removing labels. Loopback only, with Host, Origin, CSRF and CSP checks; it needs
  `roles/cloudtrace.user` and `roles/logging.viewer`. See
  [its README](crates/gadk-console/README.md).
- `gadk-prompts-cli` exports `open_store`, which opens a store from a `file://` or
  `firestore://` URL as `--store` does.
- `gadk-eval` (façade feature `eval`), offline agent evaluation. An eval set (YAML) holds
  multi-turn cases with `contains`, `not_contains`, `regex`, `equals`, `tools` (trajectory),
  `reference` and `llm_judge` scorers; `Evaluator::run` runs it against an agent built once per
  `Variant` (for example one per prompt `Selector`) with bounded concurrency, repeats and a
  timeout per run and per judge call, and returns an `EvalReport` with a text summary, JSON
  output and `all_passed()` (false on any failure or error, or with no scores). Run, build and
  judge errors are reported apart from failures. See
  [its README](crates/gadk-eval/README.md).

### Changed

- `with_otel_to_cloud(true)` now also exports log records (to Cloud Logging); before, only spans
  and metrics were exported. To opt out, supply a logger provider with `with_logger_provider`;
  note that a supplied logger provider also turns off the OTLP log exporter
  (`OTEL_EXPORTER_OTLP_*ENDPOINT`), as it always has. Log export needs the Cloud Logging API
  enabled and `logging.logEntries.create` (for example `roles/logging.logWriter`) on the
  identity; a failed export is reported once as a `tracing` warning (target
  `gadk_telemetry::cloud_logging`) until a later export succeeds.

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

[Unreleased]: https://github.com/sirhco/gadk-rust/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/sirhco/gadk-rust/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sirhco/gadk-rust/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sirhco/gadk-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sirhco/gadk-rust/releases/tag/v0.1.0
