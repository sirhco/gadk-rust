# Changelog

All notable changes to gadk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until 1.0, a minor version may break
the public API.

## [Unreleased]

## [0.5.2] - 2026-11-22

### Added

- `gadk-eval`: `Variant::with_model` and `Variant::model` give a variant an optional model name
  for the `build` closure; the evaluator does not read it.
- `gadk-eval-monitor`: experiment reports record each variant's model
  (`ExperimentVariant::model`, `ExperimentVariant::with_model`, copied from `Variant::model` by
  `ExperimentReport::new`; written only when set), and `summary()` adds a `model` column when
  any variant has one.
- `gadk-eval-monitor`: `gadk-eval experiment <spec.yaml> [--out <url>]` runs an experiment on
  one Gemini LLM agent (Vertex AI) described by a YAML experiment spec: eval set, project and
  location, runs per case, an inline instruction or a prompt from a `file://` or `firestore://`
  prompt store, the judge, and 2-10 variants with their model and prompt version. It exits 0
  when the experiment ran and 2 on any error. New public API: `ExperimentSpec`,
  `ExperimentAgent`, `AgentInstruction` (module `experiment_spec`) and `Backends::model` (a
  default body that refuses; `CloudBackends` builds Gemini on Vertex AI). `gadk-eval-monitor`
  now depends on `gadk-engine`, `gadk-prompts` and `gadk-prompts-cli`.
- `gadk-console`: the experiment comparison page shows a Model row when any variant records a
  model.

### Fixed

- `gadk-eval`, `gadk-eval-monitor`: `TokenCounts` and `RunMetrics` deserialize with missing
  fields defaulting, and `ExperimentReport` with missing `variants` or `metrics` as empty, so
  stored experiments with absent fields still load.
- `gadk-console`: the monitor-report and experiment caches also evict their oldest entries
  while they hold more than 20,000 runs in all (the newest entry always stays), so a few very
  large reports no longer keep unbounded memory.

## [0.5.1] - 2026-11-21

### Added

- `gadk-eval`: `Evaluator::run_measured` returns a `MeasuredReport`, the `EvalReport` of `run`
  plus each run's `RunMetrics`: its duration in milliseconds and its `TokenCounts` per model
  (from the responses' usage metadata).
- `gadk-eval-monitor`: experiments. `run_experiment` runs one eval set against several variants
  and returns an `ExperimentReport` (`summary`, `variant_stats`, `case_verdicts`, `to_json`,
  `from_json`; `ExperimentVariant`, `VariantStats`, `CaseVerdicts`, `Verdict`), and
  `ReportStore::{put_experiment, experiment_names, find_experiment}` keep experiments under
  `experiments/` in the report store. The `experiment` example compares two prompt versions on
  Gemini end to end.
- `gadk-console`: `/experiments` lists the stored experiments and `/experiments/{report}`
  compares their variants (pass rate and its difference from the baseline, judge score, errors,
  p50/p95 duration, tokens, and cost with `--prices`) and lists the cases where they disagree
  (`?cases=all` for every case); the nav bar gains "Experiments".

## [0.5.0] - 2026-11-21

### Added

- `gadk-agentanalytics`: `EVENT_FEEDBACK` and `BigQueryAgentAnalyticsPlugin::record_score`
  (`Score`, `ScoreValue`, `ScoreTarget`, `ScoreError`) record end-user scores of an invocation
  as `FEEDBACK` rows (`content` `{"name", "value", "data_type", "comment"}`, `attributes.source`
  `app` and `attributes.author`); the event filters apply to them.
- `gadk-console`: `/traces/{id}` lists a trace's scores and adds new ones (`POST
  /traces/{id}/scores`, one parameterized `INSERT`, needs `roles/bigquery.dataEditor`);
  `--author` (`Cli::with_author`) names their author; `/prompts/{name}` adds a Feedback table
  per version. The trace list and trace detail leave `FEEDBACK` rows out. Scores need
  `--traces bigquery://…`.
- `gadk`: the `feedback_e2e` example runs a prompt version, analytics rows and scores end to
  end.
- `gadk-eval`: `EvalSet::to_yaml` and `to_yaml_with_comments` write an eval set as YAML that
  `EvalSet::from_yaml` reads back unchanged, and `append_cases` appends cases to an eval-set
  file's text, keeping the existing text byte for byte.
- `gadk-eval-monitor`: `gadk-eval export` writes runs from the agent-analytics table as eval-set
  cases (by trace, or by window, agent and latest-score filters, with the session's earlier user
  messages as turns): positive runs become `reference` (and `tools`) cases, the others
  `llm_judge` cases. Library: the `export` module (`ExportSource`, `run_to_case`, `export_runs`,
  `cases_text`, `write_set_file`, `ScoreFilter`, `CompareOp`, `RunScore`, `ExportContext`, `ExportOptions`,
  `ExportedCase`, `TraceInvocation`), `Backends::export_source` (default: not supported) and
  `BigQueryTranscriptSource::{with_billing_project, with_max_bytes_billed}`.
- `gadk-console`: `--datasets <dir>` (`Cli::with_datasets`, `ConsoleError::Datasets`) adds an
  "Add to dataset" form to `/traces/{id}` (BigQuery source) that appends the trace's runs as
  cases to `<dir>/<dataset>.yaml`.

## [0.4.11] - 2026-11-20

### Added

- `gadk-agent`: `PromptUse` and `ReadonlyContext::{record_prompt, clear_recorded_prompt,
  recorded_prompt}`, a per-invocation record (by agent name) of the prompt version an agent's
  instruction was rendered from.
- `gadk-prompts`: `PromptInstruction` records the version it renders (with the selector's label,
  `latest`, or none for a pinned version) and clears the record on a fallback render. The model
  request is unchanged.
- `gadk-agentanalytics`: `LLM_REQUEST` and `LLM_RESPONSE` rows carry `attributes.prompt`
  (`{"name", "version", "label"}`) when the agent's instruction came from a recorded prompt
  version.
- `gadk-console`: `--prices <file>` (`Cli::with_prices`, `PriceTable`, `ModelPrice`,
  `ConsoleError::Prices`) prices token counts in USD per 1,000,000 tokens; `/traces/{id}` lists
  each model call's tokens and cost; `/usage` shows calls, tokens and cost by model, agent and
  prompt version plus the 20 traces with the most tokens; `/prompts/{name}` adds production
  metrics per version (calls, mean tokens and cost, p50/p95 latency, and eval pass rate with
  `--reports`). `/usage` and the production metrics need `--traces bigquery://…`; LLM spans from
  the BigQuery source now carry the `gen_ai.*` usage labels.

## [0.4.10] - 2026-11-20

### Added

- `gadk-vertexai`: `VertexAiRagMemoryService` supports incremental adds through
  `gadk_memory::add_events_to_memory` (each batch is one more RAG file named like the session's
  file; a full add replaces them), and `VertexAiRagMemoryConfig::with_timeout` sets a deadline
  for each RAG Engine request.

## [0.4.9] - 2026-11-20

### Added

- `gadk-vertexai`: `memory::VertexAiRagMemoryService` with `memory::VertexAiRagMemoryConfig`,
  a `MemoryService` backed by a Vertex AI RAG Engine corpus: each session is stored as one RAG
  file of its text events (replaced when the session is added again), and search runs RAG
  retrieval filtered to the requesting app and user. The README covers corpus setup, behavior,
  and teardown for Serverless and Spanner modes.

## [0.4.8] - 2026-11-20

### Added

- `gadk-telemetry`: structured stdout log lines carry a `message` summary
  (`<event.name>: <body>`, at most 200 characters) when the record has an event name or a string
  body; the README explains flushing at exit (5 s shutdown wait, `OTEL_BLRP_*` tuning).
- `gadk-console`: `--traces bigquery://<project>.<dataset>.<table>` reads traces from the
  agent-analytics BigQuery table instead of Cloud Trace and Cloud Logging, with a per-query
  byte cap (`--bigquery-max-bytes`, default 1 GiB). New public API: `traces::BigQueryTraceSource`,
  `traces::BigQueryTable`, `Cli`, `TracesFrom`, `parse_cli`, `run_cli`, `bind_with_source`, and
  the `SourceError::BigQuery` variant.
- `gadk-bigquery`: `QueryOptions::query_parameters` / `with_query_parameters` send named query
  parameters.

### Fixed

- `gadk-console`: the trace page carries each log payload once instead of three times (the span
  pane copies the server-rendered entries); the README lists `X-Content-Type-Options: nosniff`
  and `Cache-Control: no-store` with the CSP.
- `gadk-console`: the "trace has more log entries" banner states how many entries are shown
  (it always said 1,000, also when fewer had been read).

## [0.4.7] - 2026-11-20

### Changed

- `gadk-engine`: the function-response ordering errors list ids as `[a, b]` instead of
  `map[a:{} b:{}]`, and read `function response ids` (was `function responses ids`).
- `gadk-session` / `gadk-artifact`: the test-suite failure messages and step labels use Rust method
  and field names.

### Fixed

- `gadk-configurable`: the `adkcli` REST replay test no longer fails when another process takes
  its port first under a parallel test run.

## [0.4.6] - 2026-11-19

### Fixed

- `gadk-agentanalytics`: Application Default Credentials are requested with the `cloud-platform`
  scope alone (for both the Storage Write API and the REST table client), so user credentials from
  `gcloud auth application-default login` work; asking for the narrower BigQuery scopes made
  their refresh fail. The `agentanalytics` example no longer builds its own credentials.
- `gadk-bigquery`: project, dataset, table and job ids are percent-encoded in request URL paths
  (`:` is kept, so domain-scoped projects such as `example.com:my-project` work); list calls stop
  after 100 pages, so a server that keeps sending a `nextPageToken` with empty pages cannot loop
  forever; `BigQueryToolset::new` / `with_client` reject a `compute_project` that is not a valid
  project id and a `max_query_result_rows` of 0 with `ToolError::InvalidArgument`.

## [0.4.5] - 2026-11-19

### Added

- `gadk-bigquery`: a BigQuery toolset for agents (`gadk::bigquery`, feature `bigquery`). Tools
  `list_dataset_ids`, `get_dataset_info`, `list_table_ids`, `get_table_info` and `execute_sql`;
  `execute_sql` is read-only by default (`WriteMode::Blocked`, checked with a dry run), with
  `WriteMode::Protected` (writes only to temporary tables of a BigQuery session tied to the agent
  session) and `WriteMode::Allowed`. Results are capped at `max_query_result_rows` (default 50)
  with a truncation flag; `compute_project`, `location`, `maximum_bytes_billed` and `job_labels`
  are configurable. Includes the `bigquery_agent` example.
- `agentanalytics` example (`gadk`, features `gemini,agentanalytics`): an agent with a function
  tool whose runs are recorded in BigQuery by the Agent Analytics plugin.

## [0.4.4] - 2026-11-19

### Changed

- Runtime messages name config fields by their Rust field names (snake_case) instead of Go's
  PascalCase names, and no longer say `nil`. Only message text changed: error variants, HTTP
  statuses and decisions are the same.
  - `gadk-session`: compaction config errors read `compaction_interval must not be negative`,
    `overlap_size is set to 1 but compaction_interval is 0, …`, `token_threshold …`,
    `event_retention_size …` and `no compaction strategy is enabled, set compaction_interval or
    token_threshold (or leave the compaction config as None to disable compaction)` (were
    `CompactionInterval …`, `… leave the whole config nil …`); the summarizer says
    `LlmSummarizerConfig.model is required` and `prompt_template must contain the placeholder
    …`.
  - `gadk-engine`, `gadk-launcher`: the compaction wrapper is `invalid compaction config: …`
    (was `invalid Compaction: …`); `agent x is not an LlmAgent` (was `LLMAgent`); `… not
    declared in state_schema` (was `StateSchema`); a dynamic child failure without a cause ends
    `<no cause>` (was `<nil cause>`); `content for the first event is missing` and `schema is
    missing` (were `… should not be nil` / `schema is nil`); the webui flag error is `failed to
    parse webui flags: the flag set was not parsed` (was `…: <nil>`).
  - `gadk-server`: the trigger-controller compaction warnings name `compaction_interval`,
    `token_threshold` and `event_retention_size`; a missing token validator is `identity token
    validation failed: no token validator is configured` (was a Go nil-pointer panic text); the
    Agent Engine `Processing event` log line prints `none` for the absent side (was `<nil>`).
  - `gadk-anthropic`: config errors use Rust field paths: `response_mime_type
    "application/json" requires response_schema or response_json_schema`,
    `thinking_config.thinking_budget must be set explicitly …`,
    `tool_config.function_calling_config.allowed_function_names …`, `tools.google_search is not
    supported by Claude`, `http_options.base_url is not supported; set ClientConfig.base_url
    instead`, `top_k must be non-negative`, `seed is not supported`, `custom base_url on
    Backend::Vertex requires explicit ClientConfig.credentials; …`, `unsupported prompt_cache.ttl
    …`, `unsupported effort …` (were `ResponseMIMEType … ResponseSchema or ResponseJsonSchema`,
    `ThinkingConfig.ThinkingBudget …`, `Tools.GoogleSearch …`, `HTTPOptions.BaseURL …
    ClientConfig.BaseURL`, `BackendVertex`, `Credentials or TokenSource`, …).
  - `gadk-openai`: unsupported-field errors name Rust fields (`openai: unsupported generation
    config field: top_k`, `thinking_config.thinking_budget -5`, `negative max_output_tokens`,
    `non-positive http_options.timeout 0s`, `unsupported content part: media_resolution`, …;
    were `TopK`, `ThinkingConfig.ThinkingBudget`, …); `openai: request is missing`, `openai:
    tool 0 is missing` and `openai: function declaration is missing` (were `… is nil` / `nil
    function declaration`).
  - `gadk-a2a`: `either agent_card or agent_card_provider must be set` from both the remote
    agent constructor and card resolution (were `either AgentCard or AgentCardProvider must be
    provided` / `… must be set`).
  - `gadk-agentanalytics`: config errors read `batch_size and queue_max_size must be >= 1`,
    `batch_flush_intv and shutdown_timeout must be > 0`, `max_content_len must be -1 or >= 1`,
    `invalid retry_config: needs max_retries >= 0, … got {max_retries:… initial_delay:…
    max_delay:… multiplier:…}`, `custom_metadata_allowlist cannot be used when
    payload_column_denylist contains "attributes"`, `invalid view_prefix …`, `unsupported
    payload_column_denylist entry …`; the warnings name `gcs_bucket_name`,
    `Config.gcs_path_prefix` and `sync_views`; client construction says
    `ClientOptions.without_authentication is incompatible with …` (was
    `options.WithoutAuthentication`); a resumable upload that was never sent says `choose a
    larger chunk retry deadline` (was `ChunkRetryDeadline`).
  - `gadk-artifact`, `gadk-artifact-gcs`: `missing required fields: app_name, user_id,
    session_id, file_name, part` and `part.inline_data or part.text has to be set` (were
    `AppName, UserID, SessionID, FileName, Part` / `Part.InlineData or Part.Text`).
  - `gadk-auth`: `gcp: retrieve_credential requires a resource` / `… a user_id` and the
    `gcp: retrieve_credential: …` prefix (were `RetrieveCredential … Resource` / `UserID`).
  - `gadk-gemini`: `live module requires api_version to be set. You can set api_version to
    v1beta1 for Backend::VertexAi or v1alpha for Backend::GeminiApi` (was `APIVersion`,
    `BackendVertexAI`, `BackendGeminiAPI`, and the `v1apha` typo).
  - `gadk-agentregistry`: `agentregistry: project_id and location must be set` (was `ProjectID
    and Location`); `gadk-vertexai`: `vertexai: example_store_name must match …` (was
    `ExampleStoreName`).
  - `gadk-agent`: `function call or its arguments are missing` (was `functionCall or its
    arguments cannot be nil`); `gadk-configurable`: tool factories say `args is missing` and a
    workflow tool node `tool is missing` (were `args is nil` / `tool cannot be nil`).
  - Test kits (`gadk-artifact` and `gadk-session` `testsuite`): failure texts say `want
    Ok(…)` / `got Ok` / `returned Ok` instead of Go's `(…, nil)` / `got nil` / `error = nil`.

## [0.4.3] - 2026-11-19

### Changed

- Runtime messages no longer name Go packages, receivers, methods or types; they use plain
  context or Rust names instead. Only message text changed: error variants, HTTP statuses and
  decisions are the same.
  - `gadk-server`: Agent Engine handlers say `failed to create session: …`, `failed to get
    session: …`, `failed to list sessions: …`, `failed to delete session: …`, `failed to run
    agent: …`, `failed to ensure backend session: …`, `failed to emit JSON chunk: …` and
    `failed to emit JSON error chunk: …` (were `c.sessionservice.Create() failed`, `s.run()
    failed`, `helper.EmitJSON() failed`, …); the controller logs `failed to handle query: …`
    (was `handleQuery failed`) and `failed to set write deadline: …`; a duplicate method is
    `failed to create the agent engine API controller (for …): …`; the dev-feature 501 bodies
    say `… is not implemented in the gadk server` instead of ADK Go, and the eval one points to
    the `gadk-eval` crate and CLI and the `gadk-console` Evals pages (was adk-python); a live upgrade failure is `websocket: the connection cannot be
    upgraded` (was `… does not implement http.Hijacker`); live runs log `live run failed`;
    `authn: google_oidc requires …` (was `NewGoogleOIDC`).
  - `gadk-vertexai`: Memory Bank errors read `generate memories failed`, `waiting for the
    generate-memories operation failed`, `adding the whole session to memory failed`, `adding
    new session events to memory failed`, `retrieve memories failed`, `reading state key <key>
    failed` and `failed to create the Memory Bank client` (were `v.client.GenerateMemories
    failed`, `op.Wait for GenerateMemories (whole session) failed`, …). The client-construction
    error is now one wrapper, `failed to create the Memory Bank client: …`, instead of the two
    nested `newVertexAIClient failed: aiplatform.NewMemoryBankClient failed: …`; the wait
    wrapper no longer says `(whole session)` on the newer-events path.
  - `gadk-platform` and `gadk-cli`: bad int and bool flag values read `parsing "x" as an
    integer: invalid syntax` / `parsing "x" as a boolean: …` (were `strconv.ParseInt: parsing
    "x": …`); safe-subpath says `failed to compute relative path` (was `filepath.Rel failed`);
    an invalid IPv6 host is `invalid host: unable to parse IP "…"`; the deploy `--temp_dir`
    help says `the system temp directory` (was `os.TempDir()`); duration errors (CLI
    `--mem_ttl`, launcher duration flags, Agent Engine duration fields) drop Go's `time: `
    prefix (`invalid duration "x"`, `unknown unit "d" in duration "1d"`).
  - `gadk-auth`: `gcp: new_provider requires a scheme name` and `gcp: new_provider: …` (were
    `gcp: NewProvider …`).
  - `gadk-a2a`: `the runner config has no agent` (was `runner.Config.Agent is not provided`);
    the auth-with-custom-client-provider error names the Rust API (`A2AConfig::auth`,
    `client_provider`, `ClientContext::Invocation` / `Detached`, `credential_scope`).
  - `gadk-engine`: `workflow: run_node called outside a dynamic node`, `finish task tool: "x"
    is not an LLM agent`, `sub-agent x live run failed` and the task-mode validation hint
    `via run_node` (were `RunNode`, `NewFinishTaskTool`, `RunLive`).
  - `gadk-model`: an over-long SSE line is `SSE line too long` (was `bufio.Scanner: token too
    long`).
  - `gadk-session`: an out-of-range timestamp is `timestamp year outside of range [0,9999]`
    (was `Time.MarshalJSON: …`).
  - `gadk-telemetry`: GCP detector integer errors read `parsing "x" as an integer: …` (was
    `strconv.Atoi: …`).
  - `gadk-launcher`: `failed to create the agent engine handler: …` (was
    `agentengine.NewHandler failed`).
  - `gadk-agentanalytics`: `agentanalytics: a write client is required when the plugin is
    enabled` and `no write client` (were `writeClient must not be nil` / `writeClient is
    nil`); `send called after the request stream was closed` (was `SendMsg called after
    CloseSend`).
  - The `agentengine` example logs `failed to set session state`, `no memory service is
    configured` and `failed to add session to memory`.
  - Internal crates (`publish = false`): `gadk-goyaml` type errors name Rust targets (`into
    i64`, `into String`, `into Vec<u8>`, the serde struct name, `into a sequence`, `into a
    mapping`; were `int64`, `string`, `[]uint8`, `[]interface {}`, `map[string]interface {}`)
    and `invalid map key` shows the key as `["a", "b"]` / `{"k": null}`; YAML parse and emit
    output is unchanged. `gadk-configurable` callback type errors say `BeforeAgentCallback` /
    `AfterAgentCallback` (`GO_TYPE_NAME` → `TYPE_NAME`, `Callback::go_type_name` →
    `type_name`), duplicate registrations name `register_tool_factory` /
    `register_toolset_factory` / `register_callback`, and config structs' serde names are their
    Rust paths (`gadk_configurable::config::AgentRefConfig`), so YAML errors no longer print
    `configurable.agentRefConfig`.

## [0.4.2] - 2026-11-19

### Added

- `gadk-platform`: `json_kind`, `json_kind_article`, `json_text_kind_article` and
  `json_expected_found`, the shared wording for JSON type-mismatch errors.

### Changed

- JSON decode and type errors are worded for Rust instead of copying Go's `encoding/json` texts
  (`json: cannot unmarshal … into Go value of type map[string]interface {}`). They name the
  expected JSON shape and what was found, e.g. `expected a JSON object, found an array`:
  - `gadk-server`: Agent Engine request errors name the JSON path (``invalid
    `Query.class_method`: expected a string, found a number``; `invalid Query: expected a JSON
    object, found an array`; ``number 1.5 is not an integer (expected i32)``; ``number 256 is
    out of range for u8``; ``number 1e400 is out of range for f64``; `invalid base64 data at
    byte 4`), including inside `videoMetadata` and `audioTranscription`; the
    `json.Unmarshal() failed` wrappers read `failed to decode request: …`, the stream_query one
    `could not decode the request as StreamQueryRequest (…) or StreamQueryTextRequest (…)`, and
    the streaming_agent_run_with_events ones `failed to decode
    StreamingAgentRunWithEventsRequest: …` and `invalid streaming_agent_run_with_events request:
    failed to decode input.request_json: …`; a body read failure is `failed to read request
    body: …` (was `io.ReadAll with LimitReader failed`); an encode failure is `failed to encode
    JSON: …` (was `json.NewEncoder failed`), also the `Display` of `agentengine::Error::Encode`;
    the REST runtime says `failed to decode request: unknown field "x"`; NaN/Inf encode errors
    read `cannot encode NaN as JSON`; the Eventarc trigger says `event data is not valid JSON`.
  - `gadk-engine`: tool, transfer, `set_model_response`, finish-task, single-turn, agent-tool and
    parallel-worker argument errors (`unexpected args type: expected a JSON object, found a
    string`); schema type mismatches show the value as JSON; `failed to parse output JSON:
    expected a JSON object, found an array`; `empty agent_name: {…}` shows the args as JSON.
  - `gadk-openai`: function call args and JSON schemas (`expected a JSON object, found an
    array`); a non-object `error` in an API error body (``invalid API error response: `error`
    must be a JSON object, found a string``); a non-text system instruction part is `non-text
    system instruction part` (the `*genai.Part` suffix is dropped); `gadk-openai` and
    `gadk-anthropic`: a non-JSON response is `unexpected response content-type '<ct>': expected
    'application/json'`.
  - `gadk-a2a` (non-object metadata), `gadk-mcp` (non-object input schema; schema types shown as
    JSON), `gadk-agent` (`originalFunctionCall`), `gadk-tools` (`load_artifacts`, `load_memory`: `invalid query: expected a string, found …`),
    `gadk-telemetry` (GKE `machineType`), `gadk-agentanalytics` (`expirationMs`; `max retries
    exceeded`), `gadk-configurable` (MCP toolset `args` / `tool_filter` elements; values with no
    JSON form).
  - `gadk-session`: `invalid number 1e999: out of range for f64` in requested auth configs;
    toolauth model errors name Rust types (`cannot use a JSON string as i64`, `Vec<String>`,
    `HttpAuth`) and say `expected a JSON object, found …`.
  - `gadk-vertexai`: `expected an RFC 3339 timestamp, found a number (1)` for a memory state
    value; `operation finished without a response` for a done operation with no result.
- Examples and docs use `gemini-3.8-flash` (`gemini-2.5-flash` is being deprecated); the
  `gadk-gemini` README also names `gemini-4-hydrogen`. Vertex AI setups in the examples, READMEs
  and the `gadk-eval` live test use location `us` (the US multi-region), since newer Gemini models
  are served from `us` and `global`, not single regions such as `us-central1`; the
  `agentengine` and `vertexai_session` examples give the model location `us` apart from the
  Agent Engine's region. Live and native-audio models are unchanged.
- `gadk-eval-monitor`: `DEFAULT_JUDGE_LOCATION` (used when a monitor spec's `judge` sets no
  `location`) is now `us` instead of `us-central1`, so current Gemini judge models are found.

## [0.4.1] - 2026-11-19

### Added

- `gadk-cloudtrace`: `Debug` for `CloudTraceClient` (project and endpoint) and `Adc` (whether the
  credentials are built); `gadk-eval-monitor`: `Debug` for `CloudTraceTranscriptSource` (project).
  None prints credentials or tokens.

### Changed

- `gadk-launcher`: duration flag help states the accepted syntax (`'10s'`, `'2m'`, `'1h30m'`;
  units `ns`, `us`, `ms`, `s`, `m`, `h`), and `-otel_to_cloud` help points at
  `gadk_telemetry::with_otel_to_cloud` in `Config::telemetry_options` and the `gadk-telemetry`
  README.
- Error messages name Rust items: a missing model says `ensure LlmAgentConfig::model is set`,
  `run_llm_agent_as_node` errors carry that prefix, and the agent engine stream methods report
  `streaming JSONL failed: <err>`; the telemetry missing-project error says to set
  `with_gcp_quota_project` / `with_gcp_resource_project` or `GOOGLE_CLOUD_PROJECT`; the launcher's
  kept-subscriber log names `Config::telemetry_options`.
- Docs: `gadk-eval-monitor`'s Application Default Credentials `# Errors` sections say some
  credential failures only show on the first request; `Filter::LabelExact` notes the label key
  is sent verbatim (only the value is quoted); the `gadk-telemetry` README documents credentials,
  the quota and resource projects and the needed roles.

### Fixed

- `gadk` examples use `gemini-2.5-flash` (`gemini-flash-latest` returns 404 on Vertex AI).

## [0.4.0] - 2026-11-18

### Added

- `gadk-console`: Evals pages. `--reports <url>` (`file://<dir>`, a directory or
  `gs://<bucket>[/<prefix>]`) points the console at a `gadk-eval-monitor` report store; `/evals`
  shows a card per monitor with its latest pass rate and a pass-rate trend, then the newest 100
  reports (`?spec=<name>` for one monitor), and `/evals/{report}` shows each run's scorer verdicts,
  judge rationales and transcript, linking to the run's trace. Read-only.
- `gadk-eval-monitor`: `ReportStore::names` (the newest report names without reading them),
  `ReportStore::is_report_name` and `ReportStore::find` (`Ok(None)` for a missing report).
- New crate `gadk-cloudtrace`: a Cloud Trace v1 read client. `CloudTraceClient::list` (a time
  window, `ROOTSPAN` or `COMPLETE` view, `root:` / `span:` / exact-label filters, newest-first
  order, page size and token) and `get` return traces with 16-hex span ids; `TraceError` tells
  HTTP errors (with the `google.rpc.ErrorInfo` reason), not-found, transport, decode and
  credential failures apart; `Adc::lazy()` and `Adc::resolve()` supply Application Default
  Credentials.
- `gadk-eval-monitor`: the Cloud Trace transcript source. A monitor spec's
  `source: { cloud_trace: { project: <p> } }` (`SourceSpec::CloudTrace`) grades runs rebuilt from
  the agent's spans (`CloudTraceTranscriptSource`, `source::trace_mapping::build_runs`); the agent
  must run with `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY` or
  `SPAN_AND_EVENT`. `SourceSpec::cloud_trace(project)` builds the source in code.
- Documentation: every crate has a README (its crates.io page), every public item has rustdoc,
  and the workspace now warns on `missing_docs` (an error under the clippy gate). The `gadk`
  crate's crates.io page is now `crates/gadk/README.md`.

### Changed

- `gadk-console`: `ConsoleConfig::reports` is a new field; code that builds `ConsoleConfig` with a
  struct literal must set it.
- `gadk-console`: Cloud Trace list and get calls go through `gadk-cloudtrace`. The URLs sent,
  the results, the errors and the public API are unchanged.

## [0.3.0] - 2026-11-18

### Added

- `gadk-eval`: `score_transcript` grades a `RunTranscript` with a list of `ScorerSpec`s exactly
  as `Evaluator` does, and `ScorerSpec::from_value` parses one scorer in the eval-set syntax
  outside an eval set (no turn bound; `llm_judge` needs its own `rubric`). Eval-set parsing is
  unchanged.
- New crate `gadk-eval-monitor`: production eval monitors. A YAML monitor spec selects recent
  runs from the BigQuery agent-analytics table (`BigQueryTranscriptSource`, or any
  `TranscriptSource`), `Monitor` grades them with gadk-eval scorers (an `llm_judge` judge is
  Gemini on Vertex AI), and `MonitorReport`s are kept in a directory or `gs://` bucket
  (`ReportStore`). The `gadk-eval` binary (`cargo install gadk-eval-monitor`) runs a monitor
  (`gadk-eval monitor`, exit 1 below `min_pass_rate`) and reads stored reports
  (`gadk-eval report`).

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

[Unreleased]: https://github.com/sirhco/gadk-rust/compare/v0.5.2...HEAD
[0.5.2]: https://github.com/sirhco/gadk-rust/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/sirhco/gadk-rust/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/sirhco/gadk-rust/compare/v0.4.11...v0.5.0
[0.4.11]: https://github.com/sirhco/gadk-rust/compare/v0.4.10...v0.4.11
[0.4.10]: https://github.com/sirhco/gadk-rust/compare/v0.4.9...v0.4.10
[0.4.9]: https://github.com/sirhco/gadk-rust/compare/v0.4.8...v0.4.9
[0.4.8]: https://github.com/sirhco/gadk-rust/compare/v0.4.7...v0.4.8
[0.4.7]: https://github.com/sirhco/gadk-rust/compare/v0.4.6...v0.4.7
[0.4.6]: https://github.com/sirhco/gadk-rust/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/sirhco/gadk-rust/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/sirhco/gadk-rust/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/sirhco/gadk-rust/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/sirhco/gadk-rust/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/sirhco/gadk-rust/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/sirhco/gadk-rust/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/sirhco/gadk-rust/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/sirhco/gadk-rust/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sirhco/gadk-rust/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sirhco/gadk-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sirhco/gadk-rust/releases/tag/v0.1.0
