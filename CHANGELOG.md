# Changelog

All notable changes to gadk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor version may break
the public API.

## [Unreleased]

### Changed

- **Re-synced to ADK-Go main** `0990f9e2cb9df71c579db6c9731efa403641fc51` (2026-11-13), from
  `bb1fd51` (v2.7.0-5). `docs/parity/ADK_GO_COMMIT` names the new pin. Every Go probe was re-run
  and its goldens regenerated, every ledger was regenerated at the new pin, and new or changed
  ADK-Go tests are ported.
- **106 ADK-Go fixes** between the two pins were checked against gadk:
  - 90 were real gadk bugs and 2 needed a new feature. All 92 are fixed: 91 fully, 3 of them with
    a ledgered divergence on a remaining edge. The 92nd, 869020a1 (A2A 0.3 interfaces), is a
    divergence (ledger 5c).
  - 12 were Go-only bugs; their ported tests are kept as guards.
  - 2 do not apply to gadk.

  Each row is in [`docs/parity/8a-bug-report.md`](docs/parity/8a-bug-report.md).
- **genai v1.77.0:** `gadk-genai` and `gadk-gemini` mirror google.golang.org/genai v1.77.0 (was
  v1.75.0), and `docs/parity/genai-inventory.md` is regenerated. Changes:
  - New types `GenerationOutputConfig`, `LiveClientContextUpdate` and
    `LiveClientContextUpdateTools`.
  - New method `LiveConnection::send_context_update`.
  - Request conversion as genai's converters do it:
    - The Gemini API sends `store`, and rejects `generation_output_config`.
    - Vertex AI sends `generation_output_config` as `outputConfig`, and rejects `store`.
    - Vertex AI rejects a function response's `will_continue` and a part's `media_processing`.
  - A streamed chunk carrying `"error"` ends the stream with the body's code (0 when absent).
- **SDK mirrors:** the comments and policy notes now name anthropic-sdk-go v1.84.0, openai-go
  v3.91.1 and a2a-go v2.7.0. gadk's own User-Agent is unchanged.
  - Anthropic streams decode event names outside the old allow-list, as SDK 1.84.0 does.
  - The A2A server-direction goldens follow a2a-go v2.7.0's headers.

### Added

- **The five 8a core features:**
  - **Agent error callback** (Go #2229, #2260):
    - `gadk_agent::PluginConfig.on_agent_error_callback`, of type `OnAgentErrorCallback`.
    - `Plugin::on_agent_error_callback`.
    - `PluginManager::run_on_agent_error`, which suppresses cancellation causes, catches a
      panicking callback and still notifies every plugin.
  - **Requested auth configs** (Go #2114):
    - The field `gadk_session::EventActions.requested_auth_configs`.
    - Session services check it at append time (in-memory, database and Vertex AI), with Go's
      texts.
    - The LLM flow merges it across parallel calls and answers it with an
      `adk_request_credential` event.
    - The REST model writes it without secret keys and ignores it inbound.
    - Agent Engine encodes it.
    - `SuiteOptions.checks_requested_auth_configs` adds the suite cases.
  - **`artifact://` references** in `InMemoryArtifactService` (Go #1164):
    - References are parsed, scope-checked and resolved up to depth 5.
    - Parts are copied on save and load (#1782).
  - **Incremental memory** (Go #2278):
    - New `gadk_memory::EventAdder`, `MemoryService::as_event_adder`,
      `add_events_to_memory`, `AddEventsRequest` and `MemoryError::AddEventsUnsupported`.
    - `InMemoryMemoryService` implements them, with adk-python's word rules: NFC, full Unicode
      lower-casing, and runs of Latin letters split from other scripts.
  - **MCP** (Go #1821, #2133):
    - `gadk_mcp::Config.prefix` exposes tools as `<prefix>_<name>` and calls the server with
      the original name.
    - `gadk_mcp::to_mcp_tool` converts a gadk tool into an MCP tool definition, with new
      `gadk_mcp::Error` variants.
- **Plugin propagation** (Go #1777, 24d33b35): `PluginManager::has_plugins`,
  `InvocationContext::{with_inherited_plugins, take_inherited_plugins}` and
  `Runner::run_with_context`. An `AgentTool` sub-run inherits the parent runner's plugins when it
  has none of its own. Judge runners started from plugin callbacks, and separate runners started
  from tools, do not inherit.
- **Other API additions:**
  - `gadk_model`: `is_gemini_3x_live`, `is_gemini_35_live_translate`,
    `LIVE_ARTIFACT_NAMESPACE` and `is_internal_live_artifact_part`.
  - `gadk_platform::safe_subpath`, a port of Go `utils.SafeSubpath`, with `is_link_like` and
    Go's two sentinel texts.
  - `gadk_session::compare_by_last_update`.
  - `InvocationContext::{with_graph_resume_done, graph_resume_done}`.
  - `BoundModes::{with_output_schema, output_schema}`.

### Upgrade notes

- **New public fields.** These structs are not `#[non_exhaustive]`, so a struct literal that
  lists every field without `..Default::default()` must add the new field:
  - `gadk_agent::PluginConfig.on_agent_error_callback` (`None` keeps the old behaviour).
  - `gadk_session::EventActions.requested_auth_configs` (an empty map keeps the old behaviour).
  - `gadk_session::testsuite::SuiteOptions.checks_requested_auth_configs` (set it to `true` for
    a service that checks the field on append).
  - From genai v1.77.0:
    - `gadk_genai::GenerateContentConfig.{store, generation_output_config}`
    - `FunctionResponsePart.media_processing`
    - `LiveConnectConfig.labels`
- **`BeforeToolCallback` hands back the tool arguments** (ADK-Go e0cbfd30; breaking,
  user-approved).
  - The type is `gadk_agent::BeforeToolCallback`. It is used by
    `LlmAgentConfig::before_tool_callbacks` and `PluginConfig::before_tool_callback`.
  - It now returns
    `BoxFuture<'static, (Map<String, Value>, Result<Option<Map<String, Value>>, AgentError>)>`
    instead of `BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>`, mirroring
    `BeforeModelCallback`.
  - Return the arguments you received, edited or not, as the first element.
    - Edits reach the later before-tool callbacks, the tool, and the on-tool-error and
      after-tool callbacks.
    - The function call event keeps the model's original arguments.
  - `PluginManager::run_before_tool` likewise returns `(args, result)`.
  - Migration:
    - Change `Box::pin(async { Ok(None) })` to `Box::pin(async move { (args, Ok(None)) })`,
      naming the third closure parameter `args`.
    - Where you call a before-tool callback or `run_before_tool` directly and need only the
      result, append `.1`.
- **`DebugSpan.attributes` keeps JSON types** (ADK-Go #1988; breaking, user-approved).
  - `gadk_server::rest::DebugSpan::attributes` is now `BTreeMap<String, serde_json::Value>`; it
    was `BTreeMap<String, String>`.
  - Debug span attributes keep their JSON types (numbers, booleans, arrays and objects).
  - A float attribute that is NaN or infinite is left out.
  - To read a string attribute, use `attributes.get(k).and_then(Value::as_str)`.
- **toolauth moved into `gadk-session`.** The toolauth wire model now lives in
  `gadk_session::toolauth`. `gadk_tools::toolauth` re-exports it unchanged, so existing paths
  keep working.
- **gadk-configurable config paths name the config directory** (ADK-Go #1299). These now carry
  the directory of the config file, not the file's path:
  - the first argument of `Registry::resolve_agent_reference`
  - the `PathBuf` handed to an `AgentFactory`
  - `LoadCtx.parent_path`
  - the `config_path` field of the agent config structs

  References are resolved with `safe_subpath`:
  - A reference that leaves the base directory, directly or through a link, is refused with
    Go's texts (`config reference "<ref>": …`).
  - gadk's old refusal texts are gone.
  - Workflow `.yaml` node references are checked before the node cache.

### Behaviour notes

Each of these follows the matching ADK-Go fix; the bug report has the commit and the tests.

- **Runner:**
  - Empty agent names are rejected (`error creating agent: name must not be empty`).
  - `MultiLoader` lists agents in sorted order.
  - A session is auto-created only on `SessionError::NotFound` or an empty session ID.
  - The inbound message is a user turn.
  - A user message that contains a function call is refused.
  - A plugin's replacement event keeps the original event's identity.
  - An event yielded without a timestamp is stamped.
  - A live `BeforeRun` reply runs `on_event` and `after_run`.
  - Function responses are routed to the calling sub-agent, and a new invocation starts for
    answers that span invocations.
  - Composite agents narrow a resumed nested graph workflow.
- **LLM flow:**
  - A tool's empty result reaches the model as `{"result": null}` (ledger 1b), and the
    agent-analytics `TOOL_COMPLETED` row records `"result": null`.
  - Tool panics reach the model as ``Invoking `<tool>()` failed with an internal error.``.
  - Streamed partials share one event ID with the response that completes them.
  - An after-model replacement inherits `partial`, `turn_complete` and `usage_metadata`.
  - Grounding-only responses are kept.
  - A preprocessed final tool response ends the turn.
  - Tool confirmations resume only from `adk_request_confirmation`.
  - Inline-data text artifacts are injected into instructions.
- **Contents:**
  - Tool sub-branch events, internal live-artifact parts and metadata-only foreign parts are
    left out.
  - A user's answer to another agent's call is shown as context.
  - The current turn looks past hand-overs and posted-back tool results.
- **Live:**
  - Empty responses are skipped.
  - `LiveRunConfig`'s realtime input, affective dialog and proactivity reach the setup; Gemini
    3.x Live does not get the last two.
  - Gemini 3.x Live routing applies on Vertex AI too.
  - Live responses carry usage metadata.
  - Tool responses go through the sender.
- **Workflows and agents:**
  - A workflow with no edges is accepted.
  - The error text is "use_as_output delegate".
  - Input requests fall back to a random UUID.
  - `ParallelWorker` returns the context error.
  - `SequentialAgent::run_live` stops on a sub-agent error.
  - The live `task_completed` instruction is appended to an `InstructionProvider`'s result.
  - Parallel branches keep the parent's isolation scope.
  - `finish_task` rejects a null result, and a task agent's accepted output is written to
    `output_key`.
  - A typed agent node passes its output schema to the model.
  - A parsed structured reply keeps its parsed node output.
  - `transfer_to_agent` reports an unknown or empty agent name to the model.
- **Tools and plugins:**
  - `list_skills` answers under `result`, and SKILL.md instructions are trimmed.
  - `load_artifacts` answers non-string names with an `INVALID_ARGUMENTS` result.
  - `preload_memory` searches all text parts.
  - The example tool gives Gemini models plain fences.
  - `AgentTool`:
    - An empty reply is `{"result": ""}`.
    - An unresolved long-running call in the sub-run is an error.
    - `skip_summarization` is set only after valid input.
    - A workflow agent's input schema is honoured.
  - Function and streaming-function tools accept integral floats for integer arguments, as Go's
    JSON round trip does.
  - `FunctionCallModifier` leaves content-less responses to the rest of the chain.
  - `RetryAndReflect`:
    - It drops failure counts when a run ends.
    - Its guidance names `max_retries`.
    - A tool panic is reported as `"panic"`.
  - A `Command` MCP transport keeps the command's stderr and can reconnect. Close kills a child
    that has not exited 5 s after its stdin closes (ledger 2c).
- **Sessions, artifacts and memory:**
  - A final error event counts as a final response.
  - In-memory `get` with `after` filters by timestamp.
  - `SessionService::list` is ordered by last update time, then user ID, then session ID, for
    the in-memory, database and Vertex AI services.
  - The database deletes a session's events and locks app and user state rows on create.
  - Vertex AI drops `temp:` keys from the initial state.
  - `InMemoryArtifactService::get_artifact_version` reports an empty media type for a part with
    neither inline data nor text.
  - `InMemoryMemoryService` search tokenises as Go does: NFC, full Unicode lower-casing, marks
    as word breaks, and Latin runs.
- **Servers:**
  - REST events carry `timestamp`. An implausible inbound timestamp is a 400, and no session is
    created.
  - `/run` answers `[]` for no events.
  - `AutoCreateSession` reaches `/run` and `/run_sse`.
  - A session lookup failure other than not-found is a 500.
  - Agent Engine keeps fractional seconds.
  - A2A:
    - Video metadata travels as `adk_video_metadata`.
    - Empty text parts are converted.
    - A finished task is the final response.
    - A cancel without a task ID is refused.
    - Agent card skills use the agent's description.
    - The remote agent returns errors instead of error events, and resumes tasks with mixed
      tool responses.
  - The agent registry refuses interfaces of an unknown A2A major. It speaks A2A 1.0 to 0.x
    interfaces (ledger 5c).
- **Models:**
  - Gemini answers a candidate-less unary response instead of failing with `EmptyResponse`.
  - Apigee sends the bare model ID.
  - OpenAI:
    - Code-execution parts are sent as text.
    - Every non-function `Tool` field is refused with `ErrorKind::UnsupportedConfigField`.
    - Terminal text is kept after the call-ID fallback.
    - Recovered terminal conversion failures emit an
      `openai.terminal_response_conversion_failed` tracing event.
  - The stream aggregator keeps the first text thought signature, the continuation call ID and
    `model_version`.
- **Observability:**
  - gadk-telemetry leaves `gcp.project_id` off the resource when no project is known.
  - agentanalytics:
    - GCS path prefixes are sanitized and object paths are capped (#2018).
    - Response summaries and function names are capped (#2246).
    - JSON-formatter output redacts credential-named types and map keys (#2249).

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
