# gadk Foundation (Sub-project 1a) Design

## Goal

Start the Rust port of ADK-Go (`../adk-go`, module `google.golang.org/adk/v2`) as an idiomatic-Rust library named `gadk`: create the Cargo workspace and deliver the foundation layer — Gemini-compatible genai types, platform seams, the `Llm` model trait, a Gemini model supporting both the Gemini API and Vertex AI backends (including automatic continuation and the Live/bidi transport), sessions with an in-memory service, and an httprr record/replay test harness — with ADK-Go's tests for those packages ported.

## Context

**Target repo (`/Users/chrisolson/development/github/adk-rust`)** — contains only `LICENSE`, `.gitignore`, `.orchestration/`, `.agent-memory/` and one commit (`9675eb7 Initial commit`). No Rust code yet.

**Source repo (`/Users/chrisolson/development/github/adk-go`)** — ~111k lines of non-test Go, ~215k lines of tests, Go 1.26.10, depends on `google.golang.org/genai v1.75.0`.

Overall port roadmap (each sub-project gets its own spec → plan → implementation):

| # | Sub-project | Go packages |
|---|---|---|
| **1a** | **Foundation (this spec)** | genai types + client behavior, `platform/`, `model/` (+`model/gemini`), `session/` (in-memory), `internal/httprr`, `internal/llminternal/{converters,googlellm,stream_aggregator.go}` |
| 1b | Engine | `agent/` (+llmagent, workflowagents), `internal/llminternal` flows/processors, `tool/` + `functiontool`, `workflow/`, `runner/`, `plugin/` framework, `session/compaction` |
| 2 | Tools ecosystem | `tool/*` (mcptoolset, agenttool, skilltoolset, toolsearch, toolauth, toolconfirmation, geminitool, …) |
| 3 | Other models | `model/anthropicmodel`, `model/openaimodel`, `model/apigee` |
| 4 | Persistence & GCP | `session/database`, `session/vertexai`, `memory/`, `artifact/` (+gcs), `auth/` |
| 5 | Servers | `server/*` (adkrest, adka2a, agentengine, authn, authz) |
| 6 | CLI & launchers | `cmd/*` |
| 7 | Observability & extras | `telemetry/`, `plugin/*`, `agentregistry/`, `exp/*` |

Workflow cannot be separated from core: `runner/agent_node.go:43` wraps every agent as a `workflow.Node`, `runner/run_node.go:132` builds a per-run graph with `workflow.New`, and `agent/llmagent` imports `workflow`. Hence 1a stops below the agent layer.

### ADK-Go code this spec ports (all read)

- `model/llm.go` — `LLM` interface (`Name()`, `GenerateContent(ctx, *LLMRequest, stream bool) iter.Seq2[*LLMResponse, error]`), `LLMRequest{Model, Contents, Config, Tools map[string]any}`, `LLMResponse` (Content, CitationMetadata, GroundingMetadata, UsageMetadata, CustomMetadata, LogprobsResult, Input/OutputTranscription, ModelVersion, Partial, TurnComplete, Interrupted, SessionResumptionHandle, ErrorCode, ErrorMessage, FinishReason, AvgLogprobs).
- `model/registry.go` — `Factory func(ctx, name) (LLM, error)`; `Register(pattern, f)` panics on invalid regex or on a pattern string already registered; `NewLLM(ctx, name)` errors when 0 patterns match, calls the factory when exactly 1 matches, errors when >1 match.
- `model/gemini/gemini.go` (279 lines) — `NewModel(ctx, modelName, *genai.ClientConfig)`; `maybeAppendUserContent` appends user turn `"Continue processing previous requests as instructed. Exit or provide a summary if no more outputs are needed."` when contents are non-empty and the last content's role is not `user`; `addHeaders` sets `x-goog-api-client` and `user-agent`; `mergeHeadersInterceptor` joins duplicate values of those two headers with a space; `configPreservingEmptyTextThoughtSignatures` wraps the client/request `ExtrasRequestProvider` (user provider first, then) `preserveEmptyTextThoughtSignatureParts`, which sets `text: ""` on outgoing request-body parts that have `thoughtSignature` and no data field (non-data fields: `audioTranscription, mediaProcessing, mediaResolution, partMetadata, speechMetadata, thought, thoughtSignature, videoMetadata`); streaming routes through the stream aggregator; empty candidates → error `"empty response"`. Doc comment lines 123–139: the genai SDK resumes generations paused with `CONTINUATION` and merges responses.
- `model/gemini/continuation_test.go` — `TestContinuation_{PreservesClientProviderOnEveryRequest, GenerateResumedBySDK, GenerateStreamResumedBySDK, AutomaticContinuationOff, GenerateStreamKeepsMetadata, MaxTokensNotResumed}`.
- `model/gemini/testdata/*.httprr` — 4 fixtures; status lines are `HTTP/2.0 200 OK` / `HTTP/2.0 401 Unauthorized`; one (`…vertex_enabled.httprr`) is a Vertex REST request to `aiplatform.googleapis.com/v1beta1/publishers/google/models/…` returning 401.
- `internal/llminternal/converters/converters.go` — `Genai2LLMResponse`.
- `internal/llminternal/stream_aggregator.go` (472 lines) — `ProcessResponse`, partial function-call argument assembly via JSON-path (`parseJSONPath`, `setValueByPath`), `Close()` flush.
- `internal/llminternal/googlellm/variant.go` — `GoogleLLM` interface (`GetGoogleLLMVariant() genai.Backend`), `GetGoogleLLMVariant`, `IsGeminiModel`, `IsGemini1Model`, `IsGemini25OrLower`, `IsGemini2OrAbove`, `IsGeminiAPIVariant`, `CanUseOutputSchemaWithTools`, `NeedsOutputSchemaProcessor`.
- `internal/llminternal/googlellm/live_connection.go` (294 lines) — `LiveConnection` with `SendHistory`, `SendContent`, `SendRealtime`, `Recv`, `Close`; `Recv` ignores `goAway` and buffers finished input/output transcription responses until the `turnComplete` response.
- `platform/time.go`, `uuid.go`, `exec.go` — `TimeProvider`, `UUIDProvider`, `TaskRunner` stored in `context.Context`; defaults `time.Now`, `uuid.NewString`, goroutine-per-task + WaitGroup.
- `session/service.go` — `Service` interface (`Create, Get, List, Delete, AppendEvent`), request structs (`GetRequest.NumRecentEvents`, `GetRequest.After`), `ErrNotFound`.
- `session/session.go` — `Session`, `State`, `ReadonlyState`, `Events` interfaces; `Event` embeds `model.LLMResponse` and adds `ID, Timestamp, InvocationID, Branch, IsolationScope, Author, Actions, LongRunningToolIDs, Routes, RequestedInput, Output, NodeInfo`; `EventActions` (StateDelta, ArtifactDelta, RequestedToolConfirmations, SkipSummarization, TransferToAgent, Escalate, Compaction) whose `MarshalJSON` omits `stateDelta`/`artifactDelta` only when nil; `Event.UnmarshalJSON` accepts string or numeric timestamps and drops an empty `NodeInfo`; `IsFinalResponse`; `NewEvent(ctx, invocationID)`; key prefixes `app:`, `temp:`, `user:`; `ErrStateKeyNotExist`.
- `session/inmemory.go` — in-memory service; `AppendEvent` mutates the caller's `*Event` (assigns an ID when empty at line 212, trims `temp:` state); app/user state maps; `mergeStates`.
- `session/sessiontestsuite/` — `SuiteOptions{SupportsUserProvidedSessionID, ProvidesServerAssignedEventID, AppName}`; `RunServiceTests(t, opts, setup)`; used by `session/inmemory_test.go:53`.
- `internal/httprr/rr.go` — `httprr trace v1` file format; replay keyed on the scrubbed serialized request stored in a map (last duplicate wins); `-httprecord <regexp>` flag; `ScrubReq`/`ScrubResp`.

### genai v1.75.0 behavior this spec ports

Source: `google.golang.org/genai@v1.75.0` (not in the local module cache; obtain with `go mod download -json google.golang.org/genai@v1.75.0`). Read:

- `client.go` `NewClient`: credentials + API key both set → error. Backend when unspecified: `GOOGLE_GENAI_USE_ENTERPRISE` (if set; `1`/`true` case-insensitive → Vertex, else Gemini API; warns if it conflicts with `GOOGLE_GENAI_USE_VERTEXAI`), else `GOOGLE_GENAI_USE_VERTEXAI`, else Gemini API; `BackendEnterprise` → Vertex. Env API key: `GOOGLE_API_KEY` over `GEMINI_API_KEY` (warn if both). Env location: `GOOGLE_CLOUD_LOCATION`, else `GOOGLE_CLOUD_REGION`. Explicit fields override env fields individually. Vertex precedence ladder (four warn-and-clear cases, lines ~285–305), default location `global` when no location, no API key and no base URL; error if neither (project and location) nor API key nor custom base URL; base URL: explicit → `GOOGLE_VERTEX_BASE_URL` → `https://aiplatform.googleapis.com/` when location is `global` or API key set (express mode) → `https://aiplatform.{loc}.rep.googleapis.com/` for `us`/`eu` → `https://{loc}-aiplatform.googleapis.com/`. Gemini API: base URL explicit → `GOOGLE_GEMINI_BASE_URL` → `https://generativelanguage.googleapis.com/`; error if no API key; error if project or location given explicitly. ADC with scope `https://www.googleapis.com/auth/cloud-platform` when Vertex, no credentials, no API key and not (custom base URL with no project/location/key). Default API version `v1beta1` (Vertex) / `v1beta` (Gemini API). Vertex with credentials sends `X-Goog-User-Project: <quota project>`.
- `continuation.go` + `models.go:5555–5620`: automatic continuation for both backends (`shouldEnableAutomaticContinuation`, `shouldContinueGeneration`, `prepareContinuationConfig`, `mergeContinuationResponses`, `newStreamContinuation`); request field `continuationToken`.
- `live.go:75–135`: Vertex WS path `ws/google.cloud.aiplatform.{apiVersion}.LlmBidiService/BidiGenerateContent` with `Authorization: Bearer <token>`; Gemini API WS path `ws/google.ai.generativelanguage.{apiVersion}.GenerativeService.BidiGenerateContent` with header `x-goog-api-key`, or `…BidiGenerateContentConstrained` with `Authorization: Token <key>` for `auth_tokens/` keys (v1alpha only); setup message carries the full model name.

### External facts verified 2026-11-04

- Google publishes **no** official Rust crate for the Gemini Developer API. `google-cloud-aiplatform-v1` 1.18.0 speaks Vertex `v1` over gRPC/REST via gax, lacks `continuationToken`, rejects custom `x-goog-api-client`, and has no Live API — so it is **not** used in 1a (user decision: own REST transport for both backends, official `google-cloud-auth` for credentials).
- User decision: Vertex AI is reached the GCP Agent Platform way (project + location, ADC/credentials); API keys are for the Gemini API only.
- `google-cloud-auth` 1.18.0 (MSRV 1.91): `credentials::anonymous::Builder` (test credentials), `credentials::Builder::default().with_scopes(..).build() -> BuildResult<Credentials>` (ADC), `Credentials::headers(Extensions) -> Result<CacheableResource<HeaderMap>>`, `credentials::api_key_credentials::Builder`.
- `tokio-tungstenite 0.30.0` with `rustls-tls-webpki-roots` depends on `webpki-roots` (license `CDLA-Permissive-2.0`).
- Crate names `adk-rust`, `adk-core`, `adk-rs` are taken on crates.io; every `gadk-*` name used here is free.

### Counts

190 distinct `genai.*` identifiers referenced by ADK-Go (`grep -rhoE 'genai\.[A-Z][A-Za-z0-9]+' --include='*.go' ../adk-go | sort -u`). 52 `.httprr` fixtures in ADK-Go. In-scope Go `Test*` functions: `model/` 6, `model/gemini/` 20, `internal/llminternal/stream_aggregator_test.go` 21, `internal/llminternal/googlellm/` 5, `session/` 27, `platform/` 19, `internal/httprr/` 2 — 100 total.

## Scope and Non-Goals

- In:
  - Cargo workspace skeleton, shared lints, CI, `cargo-deny` config, Dependabot config, parity-ledger tooling.
  - Crates `gadk-genai`, `gadk-platform`, `gadk-model`, `gadk-gemini`, `gadk-session`, `gadk-httprr`, `gadk` (façade).
  - Gemini model with both backends over REST (generate, SSE stream, automatic continuation), Live/bidi WebSocket transport for both backends. Vertex AI is the GCP Agent Platform path: project + location, authenticated with ADC or explicit `google-cloud-auth` credentials (no API key). API keys are used only for the Gemini API.
  - In-memory session service and the reusable session conformance suite.
  - Porting all 100 in-scope Go test functions (or justified skips) and `examples/quickstart` (text-only).
- Out:
  - Agents, LLM flows/processors, tools, workflow engine, runner, plugins, compaction (1b).
  - Agent-level `LiveSession`/live run loop (1b; 1a provides only `LiveConnection`).
  - Behavior of the Gemini Files API, image generation, context caching (types for `File`, `GenerateImagesConfig`, `GenerateImagesResponse`, `GeneratedImage`, `Image` are defined as data only).
  - Ephemeral `auth_tokens/` Live keys (rejected with `GeminiError::Config`).
  - Vertex express mode (an API key with the Vertex backend): rejected with `GeminiError::Config` after the precedence ladder; may be added later.
  - Deploying to Vertex AI Agent Engine (sub-projects 5–6: `server/agentengine`, `cmd/adkgo deploy`).
  - `google-cloud-aiplatform-v1` (may be used later for non-model Vertex APIs).
  - Persistent session backends, memory, artifacts, servers, CLI, telemetry export (sub-projects 2–7).
  - Publishing to crates.io (names kept publishable; no release in 1a).
  - API-level resemblance to Go (idiomatic Rust chosen); only JSON wire shapes are kept compatible.

## Global Constraints

- Crate prefix `gadk-`; façade crate `gadk`; workspace members under `crates/`.
- Rust edition 2024; `rust-version = "1.95"` set once in `[workspace.package]` and inherited (`rust-version.workspace = true`); CI also builds on stable 1.99.1; `rust-toolchain.toml` channel `1.99.1`.
- Crate `version`, `edition`, `license`, `repository` inherited from `[workspace.package]`; `version = "0.1.0"`.
- License Apache-2.0 for all crates; every `.rs` file starts with the Apache 2.0 header.
- All dependency versions declared once in root `[workspace.dependencies]` as caret requirements with these exact values (latest stable on 2026-11-04); member crates use `{ workspace = true }` only: `tokio = "1.54.1"`, `tokio-util = "0.7.20"`, `futures = "0.3.34"`, `async-trait = "0.1.92"`, `reqwest = "0.13.5"` (`default-features = false`, features `json`, `stream`, `http2`, `rustls`, `charset`, `system-proxy`), `tokio-tungstenite = "0.30.0"` (features `connect`, `rustls-tls-webpki-roots`), `serde = "1.0.229"`, `serde_json = "1.0.154"`, `thiserror = "2.0.21"`, `tracing = "0.1.44"`, `uuid = "1.29.0"` (feature `v4`), `chrono = "0.4.45"` (feature `serde`), `bytes = "1.12.1"`, `base64 = "0.23.1"`, `http = "1.5.0"`, `url = "2.5.8"`, `regex = "1.13.1"`, `parking_lot = "0.13.0"`, `google-cloud-auth = "1.18.0"`; dev: `rstest = "0.28.0"`, `insta = "1.49.0"` (feature `json`), `pretty_assertions = "1.4.1"`, `anyhow = "1.0.104"`, `tracing-subscriber = "0.3.23"`.
- `cargo-deny` license allowlist: `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `MIT`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`, `Zlib`, `CDLA-Permissive-2.0`, `OpenSSL`; any other license reported by `cargo deny check licenses` is resolved by switching the dependency/feature, not by widening the list.
- No `anyhow` in library (non-test, non-example) code.
- Lints in `[workspace.lints]`: `unsafe_code = "forbid"`; clippy `-D warnings`; `clippy::unwrap_used` and `clippy::expect_used` = `deny` (allowed in tests/examples via `#[cfg_attr(test, allow(...))]` and `clippy.toml` `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`).
- Library code does not panic on any input from network, files, or callers.
- JSON field names and omission rules of `gadk-genai` types, `LlmResponse`, and `Event` match ADK-Go/genai encoding (camelCase), following the omitempty rule in Design § Cross-cutting mappings.
- CI never calls live Google APIs.
- genai source of truth: `google.golang.org/genai@v1.75.0`. ADK-Go source of truth: the commit recorded in `docs/parity/ADK_GO_COMMIT` (the `../adk-go` HEAD when the plan starts).

## Requirements

- **R1** The repo is a Cargo workspace whose members are exactly `gadk-genai`, `gadk-platform`, `gadk-model`, `gadk-gemini`, `gadk-session`, `gadk-httprr`, `gadk`; dependency versions appear only in root `[workspace.dependencies]`; `cargo build --workspace --all-features` succeeds on Rust 1.95 and 1.99.1.
- **R2** `docs/parity/genai-inventory.md` lists all 190 `genai.*` identifiers; each row maps to a Rust path in `gadk-genai` or `gadk-gemini`, or is marked N/A with a reason. `genai.Client`, `ClientConfig`, `HTTPOptions`, `HTTPRetryOptions`, `ExtrasRequestProvider`, `Session`, `APIError`, `NewClient`, `Backend*` map to `gadk-gemini`/`gadk-model` equivalents; `genai.Ptr` is N/A (Rust has `Some`). Every type reachable from a mapped type is also defined.
- **R3** `gadk-genai` types serialize/deserialize to the Gemini REST JSON shape: camelCase names; Go value-type `omitempty` fields omitted when zero; Go pointer fields omitted only when absent (a present zero, e.g. `"temperature":0`, is kept); bytes as standard base64; unknown enum strings deserialize to `Unknown(String)` and re-serialize unchanged.
- **R4** `gadk-platform` provides `Platform` with pluggable time, UUID and task-runner providers; `Platform::default()` uses system UTC time, UUID v4 strings, and runs tasks concurrently on tokio, returning after all finish; `Platform::deterministic(seed)` yields reproducible time/UUID sequences.
- **R5** `gadk-model` defines the object-safe `Llm` trait, `LlmRequest`, `LlmResponse` (all Go `LLMResponse` fields, same JSON), the `StreamingResponseAggregator`, the `GenerateContentResponse → LlmResponse` converter, and helpers `google_variant`, `is_gemini_model`, `is_gemini1_model`, `is_gemini25_or_lower`, `is_gemini2_or_above`, `is_gemini_api_variant`, `can_use_output_schema_with_tools`, `needs_output_schema_processor` returning Go-identical results.
- **R6** `gadk-model` provides a model registry: `register` returns `Err(RegistryError::DuplicatePattern)` for an already-registered pattern string and `Err(RegistryError::InvalidPattern)` for an invalid regex; `new_llm(name)` is async, errors when 0 or >1 patterns match, and otherwise awaits the single matching factory.
- **R7** `gadk-model` defines an `HttpClient` trait; all `gadk-gemini` REST traffic (both backends) goes through it; the default implementation wraps `reqwest`.
- **R8** `gadk-gemini` `GeminiModel` implements `Llm` for the Gemini API and Vertex AI backends over REST (`{base}{apiVersion}/…:generateContent` and `:streamGenerateContent?alt=sse`), resolving `ClientConfig` + environment exactly as genai v1.75.0 `NewClient` does (Context § genai), with one deviation: if the resolved Vertex config still holds an API key, resolution fails with `GeminiError::Config("API keys are only supported for the Gemini API backend; use ADC or credentials for Vertex AI")`. Vertex authenticates with ADC or `ClientConfig.credentials` via `google-cloud-auth`.
- **R9** For both backends `GeminiModel` reproduces Go behavior: the continuation user turn is appended exactly when Go appends it; `x-goog-api-client` and `user-agent` each appear once per request (pre-existing values space-joined); in the outgoing request body only, signature-only parts carry `"text": ""`, applied after any user `extras_request_provider`; a non-empty request `model` overrides the configured name; zero candidates → `ModelError::EmptyResponse`; streaming output passes through the aggregator.
- **R10** `GeminiModel` performs automatic continuation for both backends exactly as genai v1.75.0 `continuation.go` does: enabled by default, disabled by `automatic_continuation: Some(false)`; resumes when `candidates[0].continuationToken` is present and the finish reason is resumable per `isResumableFinishReason` (`CONTINUATION`, empty, or `FINISH_REASON_UNSPECIFIED`; never `MAX_TOKENS`); in streams, a chunk carrying prompt feedback stops resumption (`canResumeStream`); non-stream hops are merged as `mergeContinuationResponses` does; stream chunks of all hops are yielded in order with metadata kept; a stream reconnects after a transport error up to `maxStreamReconnectsWithoutProgress` = 3, except API errors before the first chunk; the extras provider runs on every hop.
- **R11** `gadk-gemini` `LiveConnection` supports `send_history`, `send_content`, `send_realtime`, `send_tool_response`, `recv`, `close` over WebSocket for both backends using genai's URLs and headers; `setup` may carry a `SessionResumptionConfig` handle and `recv` surfaces `sessionResumptionUpdate` handles in `LlmResponse.session_resumption_handle`; `recv` ignores `goAway` and buffers finished transcriptions until `turnComplete`, as `live_connection.go` does.
- **R12** `gadk-session` defines object-safe async `SessionService`, `Session`, `State`, `ReadonlyState` traits; `Event`, `EventActions`, `EventCompaction`, `EventRef`, `NodeInfo`, `RequestInput`, `ToolConfirmation` (data only), key-prefix constants, `SessionError::{NotFound, StateKeyNotExist, ..}`; `Event::new(&Platform, invocation_id)` takes ID and timestamp from the platform; `append_event` takes `&mut Event` so service-assigned fields are visible to the caller.
- **R13** `Event`/`EventActions` JSON matches ADK-Go: `LlmResponse` fields flattened; `stateDelta`/`artifactDelta` omitted only when `None`; timestamps decoded from RFC 3339 strings or numeric seconds within Go's limits; empty `nodeInfo` decodes to `None`; `is_final_response()` matches Go for every case.
- **R14** `InMemorySessionService` reproduces `session/inmemory.go`: `app:`/`user:` state routed to shared maps and merged on read, `temp:` deltas trimmed before storage, empty event IDs assigned, `get` returns deep copies, `num_recent_events`/`after` filtering, `NotFound` for missing sessions.
- **R15** `gadk_session::testsuite::run(opts: SuiteOptions, factory)` ports `RunServiceTests` with `SuiteOptions{supports_user_provided_session_id, provides_server_assigned_event_id, app_name}`; `InMemorySessionService` passes it with the options `inmemory_test.go:53` uses.
- **R16** `gadk-httprr` reads/writes `httprr trace v1`; replays ADK-Go fixtures unmodified (response status lines `HTTP/1.0`, `HTTP/1.1`, `HTTP/2.0`); matches requests on method + full URL + canonicalized JSON body (raw bytes if not JSON), ignoring all headers; for duplicate keys the last entry wins (as Go); records when env `GADK_HTTPRR_RECORD` is a regex matching the fixture file path; scrubs `x-goog-api-key`, `Authorization` and `key=` query params when recording; an unmatched request fails with a body diff.
- **R17** Every in-scope Go `Test*` function has a row in `docs/parity/1a.md` marked `ported` (Rust file + test fn named by the conversion rule in Design § Parity tooling) or `skipped` (reason); `scripts/parity-check` exits non-zero if an in-scope Go test lacks a row or a `ported` row names a missing Rust fn.
- **R18** For each event in `crates/gadk-session/tests/fixtures/go_events/` (the ten cases listed in Design § Testing), the Rust-serialized JSON and ADK-Go's JSON are equal as JSON values (object key order, whitespace and HTML-escaping ignored; numbers compared as f64; RFC 3339 timestamp strings compared as instants).
- **R19** `gadk` re-exports `gadk-genai`, `gadk-platform`, `gadk-model`, `gadk-session`, `gadk-gemini` as modules `genai`, `platform`, `model`, `session`, `gemini` (not `gadk-httprr`); feature `gemini` (default) gates the `gemini` module; both backends are always compiled into `gadk-gemini`; `examples/quickstart` performs a text-only `generate_content` on the backend selected by env vars.
- **R20** CI (GitHub Actions) runs `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo deny check`, `scripts/parity-check`, and the 1.95 build, with no network calls to Google APIs.

## Design

### Workspace layout

```
Cargo.toml                 # [workspace], [workspace.package], [workspace.dependencies], [workspace.lints]
clippy.toml  deny.toml  rust-toolchain.toml
.github/workflows/ci.yml   .github/dependabot.yml (cargo, weekly)
scripts/parity-check       # python3, stdlib only
scripts/check-workspace-deps.py  # R1: member deps must be { workspace = true }
scripts/gen-go-events/     # Go program (module importing ADK-Go via replace ../adk-go) emitting R18 reference JSON
docs/parity/ADK_GO_COMMIT  docs/parity/1a.md  docs/parity/genai-inventory.md
crates/
  gadk-genai/  gadk-platform/  gadk-model/  gadk-gemini/  gadk-session/  gadk-httprr/
  gadk/        (gadk/examples/quickstart.rs)
```

Dependency graph (acyclic): `genai ← model ← gemini`; `platform ← session`; `genai, model ← session`; `model ← httprr`; `gadk → genai, platform, model, session, gemini`. `gadk-httprr` is a dev-dependency of `gadk-gemini`.

### Cross-cutting Go → Rust mappings

| Go | Rust |
|---|---|
| `iter.Seq2[T, error]` | `futures::stream::BoxStream<'static, Result<T, E>>`; early consumer stop = dropping the stream (cancels in-flight I/O) |
| `context.Context` cancellation | Future/stream drop (`CancellationToken` introduced in 1b where Go cancels siblings) |
| `context.Context` values (platform) | explicit `&Platform` (cheap clone, `Arc` inside); not `task_local!` (does not propagate into spawned tasks) |
| `any` | `serde_json::Value` |
| pointer field `*T` with `omitempty` | `Option<T>`, `skip_serializing_if = "Option::is_none"` — `Some(zero)` is serialized |
| value field `T` (string/bool/number) with `omitempty` | plain `T` with `#[serde(default, skip_serializing_if = "is_zero")]` (`gadk_genai::serde_util::is_zero` for `String`, `bool`, integers, floats) |
| slice/map with `omitempty` | `Vec<T>` / `Map` with `default` + `skip_serializing_if = "…::is_empty"`; nil-vs-empty-significant maps (`stateDelta`, `artifactDelta`) use `Option<Map>` |
| value field without `omitempty` | plain `T`, always serialized |
| string enums | `enum` + `Unknown(String)` via `string_enum!` macro (serde, `Display`, `FromStr`) |
| `time.Time` | `chrono::DateTime<Utc>` |
| sentinel errors / `errors.Is` | `thiserror` enum variants matched with `matches!` |
| interfaces used as values | object-safe traits (`Arc<dyn Llm>`, `Arc<dyn SessionService>`); `async-trait` where methods are async |

### `gadk-genai`

- Files: `content.rs` (Content, Part, Blob, FileData, FunctionCall, FunctionResponse, FunctionResponsePart/Blob/FileData, FunctionResponseScheduling, PartialArg, ExecutableCode, CodeExecutionResult, Language, Outcome, VideoMetadata, PartMediaResolution(+Level), Role consts), `schema.rs` (Schema, Type), `config.rs` (GenerateContentConfig incl. `automatic_continuation: Option<bool>` and `continuation_token: Option<Bytes>`, ThinkingConfig, ThinkingLevel, SafetySetting, HarmCategory, HarmBlockThreshold, ToolConfig, FunctionCallingConfig(+Mode), SpeechConfig, ImageConfig, ModelSelectionConfig, ModelArmorConfig, GenerationConfigRoutingConfig, ServiceTier, MediaResolution, Modality), `tool.rs` (Tool, FunctionDeclaration, GoogleSearch, GoogleSearchRetrieval, GoogleMaps, URLContext, ToolCodeExecution, Retrieval, EnterpriseWebSearch, FileSearch, ComputerUse, MCPServer, ExternalAPI, ToolParallelAISearch, ToolExaAISearch, ResourceScope(+Collection), …), `response.rs` (GenerateContentResponse, Candidate incl. `continuation_token: Option<Bytes>` (genai types.go:3694), FinishReason, UsageMetadata, ModalityTokenCount, MediaModality, CitationMetadata, Citation, GroundingMetadata + chunks/supports/segments/RAG types, SearchEntryPoint, RetrievalMetadata, LogprobsResult*, PromptFeedback, BlockedReason, Transcription, SpeechMetadata), `live.rs` (LiveConnectConfig, LiveClientContentInput, LiveRealtimeInput, LiveToolResponseInput, ActivityStart/End, RealtimeInputConfig, SessionResumptionConfig, AudioTranscriptionConfig, ProactivityConfig, ToolCall, ToolResponse, LiveServerMessage + parts), `media.rs` (File, Image, GeneratedImage, GenerateImagesConfig, GenerateImagesResponse — data only), `helpers.rs` (`Content::from_text(text, role)`, `from_parts`, `from_function_call`, `from_function_response`, `from_bytes`, `from_uri`, `from_executable_code`, `from_code_execution_result`; matching `Part::from_*`), `serde_util.rs`.
- Struct derive: `Debug, Clone, Default, PartialEq, Serialize, Deserialize`; `#[serde(rename_all = "camelCase")]`; field rules from the mapping table, decided per field by the Go type in genai `types.go`. No `#[non_exhaustive]` on structs (struct-literal + `..Default::default()` stays usable).
- Bytes fields (`Blob.data`, `Part.thought_signature`, `Candidate.continuation_token`, `GenerateContentConfig.continuation_token`) are `bytes::Bytes` with a standard-base64 serde adapter.
- `Part` serde is plain; the thought-signature rule lives in `gadk-gemini`'s request builder.
- Depends on: serde, serde_json, bytes, base64, chrono.

### `gadk-platform`

```rust
#[derive(Clone)] pub struct Platform { /* Arc<dyn TimeProvider>, Arc<dyn UuidProvider>, Arc<dyn TaskRunner> */ }
pub trait TimeProvider: Send + Sync { fn now(&self) -> DateTime<Utc>; }
pub trait UuidProvider: Send + Sync { fn new_uuid(&self) -> String; }
#[async_trait] pub trait TaskRunner: Send + Sync { async fn run(&self, tasks: Vec<BoxFuture<'static, ()>>); }
impl Platform { pub fn now(&self) -> DateTime<Utc>; pub fn new_uuid(&self) -> String;
  pub async fn run_tasks(&self, tasks: Vec<BoxFuture<'static, ()>>);
  pub fn with_time(self, p: impl TimeProvider + 'static) -> Self; /* with_uuid, with_task_runner */
  pub fn deterministic(seed: u64) -> Self; }
```
`deterministic(seed)`: time starts at `2000-01-01T00:00:00Z + seed s` and advances 1 ms per call; UUIDs are RFC 4122 v4-formatted strings from a seeded counter. Default task runner: spawn each into a `tokio::task::JoinSet` (dropping the `run` future aborts the tasks), await all; a task panic aborts the siblings, then is re-raised on the caller with `std::panic::resume_unwind` (matches Go crash semantics; not a library-originated panic). Empty list returns immediately. Files: `lib.rs`, `time.rs`, `uuid.rs`, `exec.rs`.

### `gadk-model`

```rust
pub trait Llm: Send + Sync {
    fn name(&self) -> &str;
    fn generate_content(&self, req: LlmRequest, stream: bool) -> BoxStream<'static, Result<LlmResponse, ModelError>>;
    fn google_variant(&self) -> Option<Backend> { None }
    fn as_any(&self) -> &dyn Any;                                  // downcast Arc<dyn Llm> to the concrete model (e.g. GeminiModel::live_connect)
}
pub struct LlmRequest { pub model: String, pub contents: Vec<Content>, pub config: Option<GenerateContentConfig>,
                        pub http_options: Option<HttpOptions>,                   // per-request override (Go: config.HTTPOptions)
                        pub tools: HashMap<String, Arc<dyn Any + Send + Sync>> }   // not serialized; tightened in 1b
#[derive(Clone, Default)] pub struct HttpOptions { pub base_url: Option<String>, pub api_version: Option<String>,
   pub headers: http::HeaderMap, pub timeout: Option<Duration>, pub extras_request_provider: Option<ExtrasRequestProvider> }
pub type ExtrasRequestProvider = Arc<dyn Fn(serde_json::Map<String, Value>) -> serde_json::Map<String, Value> + Send + Sync>;
// HttpOptions: hand-written Debug (provider shown as "<fn>"); not Serialize/PartialEq. Lives in gadk-model so gadk-genai stays plain data.
pub struct LlmResponse { /* every Go LLMResponse field, same JSON names and omitempty per mapping table */ }
pub enum Backend { GeminiApi, VertexAi }
#[non_exhaustive] pub enum ModelError {
   Api { code: u16, status: String, message: String, details: Vec<Value> },
   Transport(Box<dyn Error + Send + Sync>), Decode { message: String, body_prefix: String /* ≤ 2048 bytes */ },
   EmptyResponse, Config(String), Other(Box<dyn Error + Send + Sync>) }
```
- `registry.rs`: `pub type Factory = Arc<dyn Fn(String) -> BoxFuture<'static, Result<Arc<dyn Llm>, ModelError>> + Send + Sync>`; `Registry::{new, global, register(&self, pattern: &str, f: Factory) -> Result<(), RegistryError>, async new_llm(&self, name: &str) -> Result<Arc<dyn Llm>, RegistryError>}`; `RegistryError::{InvalidPattern, DuplicatePattern(String), NoMatch(String), Ambiguous { name, patterns }, Factory(ModelError)}`. Go's `TestRegisterDuplicatePatternPanics` ports as an assertion on `DuplicatePattern`.
- `aggregator.rs`: `StreamingResponseAggregator::{new, process_response(&mut self, GenerateContentResponse) -> Vec<Result<LlmResponse, ModelError>>, close(&mut self) -> Option<LlmResponse>}` — direct port incl. JSON-path parser.
- `convert.rs`: `llm_response_from_genai(&GenerateContentResponse) -> LlmResponse`.
- `variant.rs`: the eight helpers in R5; model name stripped of any path prefix before matching `^gemini-(\d+(\.\d+)?)`.
- `http.rs`:
  ```rust
  pub type BodyStream = BoxStream<'static, Result<Bytes, ModelError>>;
  #[async_trait] pub trait HttpClient: Send + Sync {
      async fn send(&self, req: http::Request<Bytes>) -> Result<http::Response<BodyStream>, ModelError>;
  }
  pub struct ReqwestClient(reqwest::Client);   // default
  ```
- HTTP status ≥ 400 → parse `{"error":{"code","status","message","details"}}` into `ModelError::Api`; if unparseable, `Api` with `message` = body prefix.

### `gadk-gemini`

```rust
pub struct ClientConfig { pub backend: Option<Backend>, pub api_key: Option<String>, pub project: Option<String>,
   pub location: Option<String>,
   pub credentials: Option<google_cloud_auth::credentials::Credentials>,
   pub http_options: gadk_model::HttpOptions, pub http_client: Option<Arc<dyn HttpClient>>, pub env: Arc<dyn EnvProvider> /* default: process env */ }
impl GeminiModel { pub async fn new(model_name: &str, cfg: ClientConfig) -> Result<Arc<dyn Llm>, GeminiError>;
                   pub async fn live_connect(&self, cfg: LiveConnectConfig) -> Result<LiveConnection, GeminiError>; }
```
`LlmRequest.http_options` is the per-request override (Go puts it on `GenerateContentConfig.HTTPOptions`; in Rust it sits on `LlmRequest` so `gadk-genai` stays plain serde data). Its `extras_request_provider` takes precedence over the client's, and its headers/timeout are merged over the client's (Go `configPreservingEmptyTextThoughtSignatures` order).

- **Config resolution** (`config.rs`): a line-by-line port of genai `NewClient` (Context § genai) producing `ResolvedConfig { backend, api_key, project, location, base_url, api_version, auth: Auth::{ApiKey, Credentials, None} }`. Warnings via `tracing::warn!` with genai's wording. Errors → `GeminiError::Config` with genai's wording. ADC via `google_cloud_auth::credentials::Builder::default().with_scopes(["https://www.googleapis.com/auth/cloud-platform"]).build()` when `credentials` is `None`. `google-cloud-auth` is a non-optional dependency of `gadk-gemini` (no `vertex` feature). The ladder's warn-and-clear cases run first (so an env API key is dropped when project/location are set); a Vertex config that still has an API key afterwards → `Config` (see R8). `auth_tokens/` keys → `Config`.
- **Request building** (`request.rs`): ports genai `generateContentParametersToMldev` (Gemini API) and `…ToVertex` (Vertex): splits `GenerateContentConfig` into top-level (`systemInstruction`, `tools`, `toolConfig`, `safetySettings`, `cachedContent`, `labels`, `serviceTier`, `continuationToken`, `modelSelectionConfig`/`modelArmorConfig` where the backend supports them) and `generationConfig`; serializes to `serde_json::Map`; applies the user `extras_request_provider`; then applies the thought-signature rule to `contents[*].parts[*]`. Model path: Gemini API `{base}{v}/models/{m}`; Vertex with project `{base}{v}/projects/{p}/locations/{l}/publishers/google/models/{m}`; a name already containing `/` is used as genai's `tModel` does.
- **Auth headers** per request: Gemini API `x-goog-api-key`; Vertex → merge the `HeaderMap` from `Credentials::headers(Extensions::new())` as-is (`CacheableResource::New` or the cached value; google-cloud-auth already includes `authorization` and `x-goog-user-project` when a quota project is set).
- **Shared pipeline** (`model.rs`, R9): clone request → `maybe_append_user_content` → ensure config → tracking headers (`gadk-rust/<CARGO_PKG_VERSION> rust/<rustc version from build.rs>`, appended to any existing `x-goog-api-client`/`user-agent` values with a space, each header set once) → model name (`req.model` if non-empty) → continuation driver → convert/aggregate.
- **Continuation driver** (`continuation.rs`, R10): ports genai `continuation.go` (`should_enable`, `is_resumable_finish_reason`, `should_continue`, `prepare_config`, `merge_responses`, `StreamContinuation` with reconnect limit 3). Each hop rebuilds the request body (so the extras provider runs every hop).
- **Transport**: `POST …:generateContent`; `POST …:streamGenerateContent?alt=sse` parsed by a hand-written SSE line parser (`data:` lines, blank-line terminated events) over `BodyStream`. Mid-stream error: yield one `Err`, then end.
- **Live** (`live/`, R11): `tokio-tungstenite` connect to `wss://{host}/{path}` per genai `live.go`; Gemini API header `x-goog-api-key`; Vertex header `Authorization: Bearer` from credentials headers. First frame `{"setup": {model: <full name>, generationConfig, systemInstruction, tools, sessionResumption, inputAudioTranscription, outputAudioTranscription, realtimeInputConfig, proactivity, …}}` from `LiveConnectConfig`. `send_history`/`send_content` → `clientContent`; `send_realtime` accepts `Blob`, `ActivityStart`, `ActivityEnd`, audio-stream-end → `realtimeInput`; `send_tool_response` → `toolResponse`; `recv` maps `serverContent`/`toolCall`/`sessionResumptionUpdate`/transcriptions to `LlmResponse` exactly as `live_connection.go` `Recv`, ignoring `goAway`, buffering finished transcriptions until `turnComplete`; `close` sends a close frame. Sink and read half each behind their own `tokio::sync::Mutex`, so `send_*` and `recv` all take `&self` and a pending `recv` never blocks a send (the connection is shared via `Arc` for concurrent receive and send, as ADK-Go `base_flow.go` does).
- **Errors** (`error.rs`): `GeminiError::{Config(String), Auth(String), Live(String), Model(ModelError)}`; `Llm` surfaces `ModelError` (auth failures → `ModelError::Other`).

### `gadk-session`

```rust
#[async_trait] pub trait SessionService: Send + Sync {
  async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError>;
  async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError>;
  async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError>;
  async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError>;
  async fn append_event(&self, session: &dyn Session, event: &mut Event) -> Result<(), SessionError>; }
pub trait Session: Send + Sync { fn id(&self) -> &str; fn app_name(&self) -> &str; fn user_id(&self) -> &str;
  fn state(&self) -> &dyn State; fn events(&self) -> Vec<Event>; fn last_update_time(&self) -> DateTime<Utc>;
  fn as_any(&self) -> &dyn Any; }
pub trait State: Send + Sync { fn get(&self, k: &str) -> Result<Value, SessionError>; fn set(&self, k: &str, v: Value) -> Result<(), SessionError>; fn all(&self) -> Vec<(String, Value)>; }
pub trait ReadonlyState: Send + Sync { fn get(&self, k: &str) -> Result<Value, SessionError>; fn all(&self) -> Vec<(String, Value)>; }
```
- Request structs mirror Go (`GetRequest { app_name, user_id, session_id, num_recent_events: usize, after: Option<DateTime<Utc>> }`, `CreateRequest { app_name, user_id, session_id: Option<String>, state: Option<Map<String, Value>> }`).
- `event.rs`: `Event { #[serde(flatten)] response: LlmResponse, id, timestamp, invocation_id, branch, isolation_scope, author, actions, long_running_tool_ids, routes, requested_input, output: Option<Value>, node_info }`; custom `Deserialize` for timestamp/nodeInfo; `is_final_response()`. `EventActions.state_delta: Option<Map<String, Value>>`, `artifact_delta: Option<Map<String, i64>>`; `Event::new` sets both to `Some(empty)`. `RequestInput.response_schema: Option<Value>`.
- `tool_confirmation.rs`: `ToolConfirmation { hint, confirmed, payload }` with Go JSON.
- `inmemory.rs`: `InMemorySessionService` (port of `inmemory.go`) with `parking_lot::RwLock` maps; `InMemorySession` holds locked state/events; `append_event` ignores partial events as Go does, assigns empty IDs via the service's `Platform`, trims `temp:` keys from the stored copy and the caller's event as Go does.
- `testsuite.rs` (feature `testsuite`): `pub struct SuiteOptions { pub supports_user_provided_session_id: bool, pub provides_server_assigned_event_id: bool, pub app_name: Option<String> }`; `pub async fn run<F, Fut>(opts: SuiteOptions, factory: F) where F: Fn() -> Fut, Fut: Future<Output = Arc<dyn SessionService>>` — one async fn per Go subtest, all executed, panicking with the subtest path on failure (test-support code, not library code).
- `SessionError::{NotFound, StateKeyNotExist, InvalidRequest(String), Backend(Box<dyn Error + Send + Sync>)}`.

### `gadk-httprr`

- `RecordReplay::open(path, real: Arc<dyn HttpClient>) -> Result<Self, HttprrError>`; implements `HttpClient`. Record mode iff `GADK_HTTPRR_RECORD` is set and its regex matches the fixture file path; otherwise replay (missing file → error naming the env var to set).
- File format as Go: header line `httprr trace v1`, then per entry `"{reqLen} {respLen}\n"` + request wire + response wire. Hand-written parser (no `httparse`): request line, headers, body by length; response status line accepts `HTTP/1.0`, `HTTP/1.1`, `HTTP/2.0`; headers; body as stored (no chunked decoding needed — Go writes de-chunked bodies with `Content-Length`). Writer emits `HTTP/1.1` request lines and the received response version.
- Replay key = `(method, full URL, canonical body)`; canonical body = JSON re-serialized with sorted keys and no whitespace when the body parses as JSON, else raw bytes. Headers are not part of the key. Duplicate keys: last entry wins.
- Scrubbing (record): `x-goog-api-key`, `Authorization`, `key=` query param removed; extra hooks `scrub_req`/`scrub_resp` as in Go.
- Unmatched request → `ModelError::Transport(HttprrError::NoMatch { method, url, diff })`; `diff` is a line diff of the canonical body against the closest logged body with the same method + URL (or "no entry for method+URL").
- Response body returned as a single-chunk `BodyStream`.

### `gadk` façade

`pub use gadk_genai as genai; pub use gadk_platform as platform; pub use gadk_model as model; pub use gadk_session as session; #[cfg(feature = "gemini")] pub use gadk_gemini as gemini;`. `examples/quickstart.rs`: builds `GeminiModel` for `gemini-2.5-flash` (override `GADK_MODEL`) from env, sends "Say hello in one sentence.", prints the response text; exits non-zero with the error on failure.

### Parity tooling

- `docs/parity/genai-inventory.md`: `| genai identifier | Rust path | note |` for all 190 identifiers.
- `docs/parity/1a.md`: `| Go file | Go test | status | Rust file | Rust fn / reason |`.
- **Test-name conversion rule** (Go `Test*` func → Rust fn): drop `Test`; split into words at `_`, before an uppercase letter preceded by a lowercase letter or digit, and before an uppercase letter that is followed by a lowercase letter and preceded by an uppercase letter; letters and digits are not split; lowercase; join with `_`; prefix `test_`. Examples: `TestNewUUIDDefaultIsRandomAndValid` → `test_new_uuid_default_is_random_and_valid`; `TestIsGemini25OrLower` → `test_is_gemini25_or_lower`; `TestNewLLMSingleMatch` → `test_new_llm_single_match`; `TestModel_Generate` → `test_model_generate`; `TestContinuation_GenerateResumedBySDK` → `test_continuation_generate_resumed_by_sdk`. Go subtests become `rstest` cases or loop iterations inside that fn; extra Rust tests may exist without rows.
- `scripts/parity-check` (python3, stdlib only): reads `ADK_GO_DIR` (default `../adk-go`), enumerates `^func (Test\w+)\(` in the in-scope Go test files, applies the rule, and fails if a Go test has no row, or a `ported` row's Rust file lacks `fn <name>`. CI checks out `google/adk-go` at the commit in `docs/parity/ADK_GO_COMMIT`.

### Testing

- Port the 100 Go test functions; table-driven subtests → `rstest` cases or loops.
- Copy the 4 `model/gemini/testdata/*.httprr` fixtures and replay the 3 Gemini API ones unmodified. `TestModel_TrackingHeaders_verifies_headers_are_set_vertex_enabled.httprr` uses Vertex express mode (API key), which is out of scope, so the `vertex_enabled` case is adapted: it runs with project + location and `google_cloud_auth::credentials::anonymous::Builder` credentials against a hand-written fixture for `{loc}-aiplatform.googleapis.com/v1beta1/projects/{p}/locations/{loc}/publishers/google/models/gemini-2.0-flash:generateContent` (ledger row: `ported (adapted: express → credentials)`). An extra test asserts that an API key with the Vertex backend returns the R8 `Config` error. other HTTP tests use httprr files recorded once against the live API by the implementer (scrubbed) or hand-written fixture files; continuation tests use a scripted fake `HttpClient` returning queued responses (as Go's tests use an `httptest` server).
- Live tests: in-process tokio-tungstenite server with scripted frames, both backends.
- Additional Rust tests: genai serde round-trips on captured Gemini JSON (incl. `"temperature":0` kept, `"text":""` dropped on decode→encode of a value-type field); unknown-enum round-trip; continuation merge; config-ladder table tests with a fake `EnvProvider`.
- R18 fixture events (Go JSON produced by `scripts/gen-go-events`, committed): (1) bare `NewEvent`; (2) text content + usage metadata + model version; (3) function call + function response parts; (4) `stateDelta`/`artifactDelta` nil vs empty; (5) compaction with excluded events; (6) requested tool confirmations; (7) node info + requested input + output + routes; (8) part with thought signature and no text; (9) partial/turnComplete/interrupted + transcriptions; (10) error code/message + finish reason + `<>&` in text.

### Delivery order (for the plan)

Batch A: workspace, `gadk-genai`, `gadk-platform`, `gadk-model`, `gadk-httprr`, `gadk-session`. Batch B: `gadk-gemini` Gemini API REST + config ladder + continuation. Batch C: Vertex (ADC/credentials, regional/`global`/`.rep.` endpoints). Batch D: Live. Batch E: façade, quickstart, parity ledger completion, CI. Each batch ends green.

## Acceptance Criteria

- R1 → `cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(sorted(p["name"] for p in json.load(sys.stdin)["packages"]))'` prints exactly the seven names; `python3 scripts/check-workspace-deps.py` (asserts every member dependency entry is `{ workspace = true }`) exits 0; `cargo +1.95 build --workspace --all-features` and `cargo +1.99.1 build --workspace --all-features` succeed.
- R2 → every row of `docs/parity/genai-inventory.md` has a Rust path or N/A reason (190 rows); `cargo test -p gadk-gemini --all-features --test inventory` (a test file referencing every listed Rust path) compiles and passes.
- R3 → `cargo test -p gadk-genai` passes, including `serde_temperature_zero_kept`, `serde_value_zero_omitted`, `serde_unknown_finish_reason_roundtrip`, and captured-response round-trips.
- R4 → `cargo test -p gadk-platform` passes (ported platform tests + `deterministic_sequence_reproducible`).
- R5 → `cargo test -p gadk-model` passes ported `stream_aggregator_test.go`, `googlellm/variant_test.go` and `model/llm_test.go` tests.
- R6 → `cargo test -p gadk-model registry` passes ported `registry_test.go` tests, incl. `test_register_duplicate_pattern_panics` asserting `DuplicatePattern`.
- R7 → `grep -rn 'reqwest::' crates/gadk-gemini/src` returns nothing; gemini tests run entirely through httprr / fake `HttpClient`.
- R8 → `cargo test -p gadk-gemini --all-features` passes `test_model_generate`, `test_model_generate_stream`, `test_model_tracking_headers` (both cases), the `config_ladder_*` table tests, and `config_vertex_api_key_rejected`.
- R9 → ported `gemini_test.go` tests pass, incl. `test_config_preserving_empty_text_thought_signatures`, `…_nil`, `…_provider_order`, `test_model_preserves_client_extras_request_provider`, `test_preserve_empty_text_thought_signature_parts_map_slices`.
- R10 → the six ported `test_continuation_*` tests pass for both backends, plus `continuation_unspecified_reason_resumes`, `continuation_prompt_feedback_stops`, `continuation_stream_reconnect_limit`.
- R11 → `cargo test -p gadk-gemini --all-features live_` passes against the in-process WS server for both backends, covering all six methods, resumption handle, goAway ignore, transcription buffering.
- R12 → `cargo test -p gadk-session` passes; doc-test builds `Arc<dyn SessionService>` and `Event::new(&Platform::deterministic(1), "inv")` with deterministic ID/timestamp, and shows `append_event` filling an empty ID.
- R13 → ported `event_json_test.go`/`event_test.go` tests pass.
- R14 → ported `inmemory_test.go` tests pass.
- R15 → `cargo test -p gadk-session --features testsuite --test testsuite_inmemory` passes, executing every ported suite case.
- R16 → `cargo test -p gadk-httprr` passes ported `rr_test.go` tests plus `replays_go_fixture_http2_status`, `unmatched_request_diff`, `record_scrubs_keys`, `duplicate_key_last_wins`.
- R17 → `python3 scripts/parity-check` exits 0; deleting any row from `docs/parity/1a.md` makes it exit non-zero.
- R18 → `cargo test -p gadk-session go_event_json` passes for all ten fixture cases.
- R19 → `cargo build -p gadk` and `cargo build -p gadk --no-default-features` succeed; `GOOGLE_API_KEY=… cargo run -p gadk --example quickstart` and (after `gcloud auth application-default login`) `GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=… GOOGLE_CLOUD_LOCATION=… cargo run -p gadk --example quickstart` each print a greeting (manual, recorded in the PR description).
- R20 → the CI run on the 1a PR is green with all six steps present.

## Risks

- **genai behavior drift** (v1.75.0 converters, continuation, config ladder are intricate). Mitigation: port from the downloaded genai source function-by-function, cite the Go function in a doc comment, and validate against Go fixtures (canonical-body match fails on drift).
- **Go httprr fixtures not matching Rust requests**. Mitigation: canonical JSON keys, header-free matching (R16); a semantic body mismatch is treated as a bug, not a fixture to re-record.
- **Live protocol details** (Vertex setup fields, transcription buffering). Mitigation: mirror `live.go` + `live_connection.go`; scripted-frame tests; manual smoke test during the plan.
- **Value-vs-pointer omitempty mistakes** in ~190 types. Mitigation: the field rule is mechanical from genai `types.go`; round-trip tests on captured JSON for every top-level response/config type.
- **Fast-moving `google-cloud-auth`** (monthly releases). Mitigation: single pin point; weekly Dependabot PRs; `cargo deny`.
- **TLS license tree** (`webpki-roots` CDLA, `aws-lc-sys` OpenSSL/ISC). Mitigation: allowlist set in Global Constraints; anything else is resolved by changing features.
