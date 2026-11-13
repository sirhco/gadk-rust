# gen-go-agentanalytics scenario

`scripts/gen-go-agentanalytics` records what ADK-Go's `plugin/agentanalytics` (at
`docs/parity/ADK_GO_COMMIT`) computes and sends, as fixtures for `gadk-agentanalytics` (sub-project
7b-1, spec R1–R9): the `Config` defaults, the BigQuery and Arrow events schemas, the analytics view
SQL, the content formatter and redaction outputs over a corpus, and the BigQuery REST requests the
`cloud.google.com/go/bigquery` v1.79.0 client sends for table creation, schema upgrade and view DDL.

It is an overlay test, not a Go module: `run.sh` copies ADK-Go's `plugin/agentanalytics/*.go` into a
temp dir, installs `module.go.mod` / `module.go.sum` as its `go.mod` / `go.sum` (ADK-Go's
`plugin/agentanalytics/go.mod` plus `replace google.golang.org/adk/v2 => ../../../adk-go`, rewritten to
the absolute ADK-Go path, then `go mod tidy` under go1.26.10), adds `overlay/zz_probe_test.go.txt` as
`zz_probe_test.go` (package `agentanalytics`, so unexported functions are callable), and runs
`go test -run '^TestGenerateFixtures$'` with `AGENTANALYTICS_PROBE_OUT` set. ADK-Go is not modified.

Regenerate (from the repo root):

```
scripts/gen-go-agentanalytics/run.sh            # writes crates/gadk-agentanalytics/tests/fixtures
scripts/gen-go-agentanalytics/run.sh OUT_DIR    # writes OUT_DIR instead (replaced as a whole)
```

It prints `wrote 68 fixture files`. ADK-Go is `$ADK_GO_DIR`, else `adk-go` beside the main checkout
(the parent of git's common dir, so it works from a git worktree), else `../adk-go`. The Go toolchain
is `$GADK_PROBE_GOTOOLCHAIN` (default `go1.26.10`, fetched by the go command's `GOTOOLCHAIN` support
on first use); the outputs are byte-identical under go1.26.10 and go1.27.2. The run uses no clock,
no randomness reaches a fixture (BigQuery job IDs are replaced by `{{JOB_ID}}`), and no Go map
iteration order decides an output, so `scripts/check-fixtures.sh` reruns it into a temp dir and
requires an empty `diff -r`, and asserts the per-directory file counts (arrow 9, config 3,
formatter 8, redaction 2, schema 3, views 2, wire 41; 68 in all).

To refresh `module.go.mod` / `module.go.sum` after `ADK_GO_COMMIT` moves: copy ADK-Go's
`plugin/agentanalytics` into a temp dir, `go mod edit -replace google.golang.org/adk/v2=<abs ADK-Go>`,
`GOTOOLCHAIN=go1.26.10 go mod tidy`, then copy `go.mod` (with the replace path rewritten to
`../../../adk-go`) and `go.sum` back.

## Fixture formats

All paths are under `crates/gadk-agentanalytics/tests/fixtures/`. JSON files are UTF-8, written by
Go `encoding/json` without HTML escaping, with a trailing newline. Map keys are sorted (Go), except
in the `projection` objects, whose keys follow `Config`'s field order. Two layouts:

- **case files** (`{"about": "...", "cases": [...]}`): `about` restates the call in prose; each
  case is one compact JSON object on its own line.
- **document files** (`config/default_config.json`, `config/constants.json`,
  `schema/events_schema.json`, `wire/*.json`): one object, 2-space indented.

Conventions in every file:

- A Go string that is not valid UTF-8 is written `{"invalid_utf8_base64": "<std base64>"}` (no
  current fixture has one; the corpus leaves such inputs out because a Rust `&str` cannot hold them).
- An `error` field is the Go `err.Error()` text, or `null`.
- Durations are integer nanoseconds.
- Go nil slices are `null`; empty slices `[]`. Where a fixture shows `"parts": null` or
  `"pending": null`, Go returned a nil slice (no entries), which Rust's empty `Vec` stands for.

### config/

- `default_config.json` — `{about, projection, derived}`. `projection` is `DefaultConfig()` as a
  *Config projection*: one key per `Config` field, named exactly like the Go field, in declaration
  order: `Enabled`, `ProjectID`, `DatasetID`, `TableName`, `MaxContentLen`, `BatchSize`,
  `BatchFlushIntv` (ns), `QueueMaxSize`, `ClientOptions`, `ClusteringFields` (array or null),
  `CustomTags` (object or null), `LogMultiModalContent`, `ShutdownTimeout` (ns), `Logger`,
  `RetryConfig` (`{MaxRetries, InitialDelay (ns), MaxDelay (ns), Multiplier}`),
  `DisableMultiModalContent`, `DisableSessionMetadata`, `CaptureFullMessageContent`,
  `DisableSensitiveKeyRedaction`, `ContentFormatter`, `DebugContentFormatterErrors`,
  `EventAllowlist`, `EventDenylist`, `PayloadColumnDenylist`, `CustomMetadataAllowlist`,
  `FinalResponseToolNames` (arrays or null), `DisableFlushOnRunEnd`, `DisableSchemaUpgrade`,
  `EnableOTelCorrelation`, `GCSBucketName`, `GCSClientOptions`, `GCSPathPrefix`, `ConnectionID`,
  `MaxInlineTextSize`, `GCSMaxConcurrentUploads`, `GCSUploadTimeout` (ns),
  `GCSMaxPendingUploadBytes`, `GCSOffloader`, `DisableCreateViews`, `ViewPrefix`,
  `ExactlyOnceDelivery`. `ClientOptions`, `GCSClientOptions`, `ContentFormatter` and `GCSOffloader`
  are `"nil"` or `"non-nil"`; `Logger` is `"nil"`, `"log.Default()"` (Go's default logger, i.e. the
  Rust default logger) or `"custom"`. `derived` holds the unexported accessors:
  `logMultiModalContent`, `logSessionMetadata`, `redactSensitiveKeys`, `flushOnRunEnd`,
  `createViews`, `maxContentLength` (int; -1 when `CaptureFullMessageContent`), `viewLogfIsNil`
  (true when Logger is nil or `log.Default()`).
- `apply_defaults.json` — cases `{name, input, output, input_derived, output_derived}`: `input` and
  `output` are Config projections with `applyDefaults(input) == output`; the `*_derived` objects are
  `derived` of each. Cases: `zero`, `default_config`, `partial`, `retry_max_retries_only`,
  `retry_multiplier_only`, `retry_full`, `negatives`, `gcs_positive`, `flags`, `lists`,
  `default_logger_nil`, `default_capture_full`.
- `constants.json` — the package constants the port needs (default sizes, timeouts in `*_ns`, every
  placeholder text, `uploadFailedText`, storage modes, `sensitiveKeys` sorted, `patternWords`, the
  generated regex sources `fixedSensitiveKeyRegex` / `sensitiveKeyRegex` / `bareSensitiveKeyRegex` /
  `quotedValueRegex`, `regexes` (the `String()` of each compiled `regexp`), and
  `eventTypesWithViews` (the 25 event types of `eventViewSpecs`, in order).

### schema/

- `events_schema.json` — `{about, schema}`: `EventsSchema()` as the BigQuery REST `TableSchema`
  object the bigquery client puts in the `tables.insert` body (`{"fields": [{"name", "type",
  "mode"?, "description", "fields"?}]}`; captured from the wire, so it is exactly Go's shape:
  `mode` is `"REQUIRED"` / `"REPEATED"` and omitted for nullable fields).
- `projections.json` — cases `{name, denied, schema}`: `projectEventsSchema(parsePayloadColumnDenylist
  (denied))` in the same REST shape. Names (`denied`): `none` (null), `content`, `content_parts`,
  `attributes`, `latency_ms`, `attributes_latency_ms`, `all` (all four).
- `parse_denylist.json` — cases `{input, denied, error}`: `parsePayloadColumnDenylist(input)`;
  `denied` is the set sorted (null on error).

### arrow/

- `events.arrows` and `projected_<name>.arrows` (the 7 denylist names above) — raw Arrow IPC
  **stream** bytes: `SerializedArrowSchema()` and `serializeArrowSchema(projectArrowSchema(...))`
  (the schema message then the end-of-stream marker `ff ff ff ff 00 00 00 00`).
  `projected_none.arrows` equals `events.arrows`.
- `schemas.json` — cases `{name, denied, file, byte_len, sha256, schema}` (`name` is `serialized`
  or `projected_<denylist>`), where `schema` describes the schema decoded from the bytes
  (`ipc.NewReader`), which the probe checks equals the in-memory one:
  `{endianness: "little", metadata: [[k, v]...], fields: [Field]}` with
  `Field = {name, nullable, metadata: [[k, v]...] (ordered pairs), type: Type}` and
  `Type = {id, go_string, ...}`: `id` is `timestamp` (+ `unit: "us"`, `timezone: "UTC"`), `utf8`,
  `int64`, `bool`, `list` (+ `element: Field`, named `element`) or `struct` (+ `fields: [Field]`).
  The JSON columns carry metadata `[["ARROW:extension:name", "google:sqlType:json"]]`.

### views/

- `view_definitions.json` — cases `{name, fn, config | args, result}`:
  - `fn: "Config.ViewDefinitions"`: `config` is the receiver as a Config projection (build it, call
    `view_definitions()`; Go applies `applyDefaults` and `parsePayloadColumnDenylist` first);
  - `fn: "viewDefinitionsWithDenied"`: `args = {projectID, datasetID, tableName, viewPrefix,
    denied}` (`denied` a list or null = no denylist);
  - `result` is `{views: [{name, event_type, query}]}` (25, in `eventViewSpecs` order; `query` is
    the exact SQL text) or `{error}`.
- `referenced_columns.json` — cases `{expr, denied, referencesDeniedColumn, stripSQLStringLiterals}`.

### formatter/

Common result object **R** (SmartTruncate): `{output, truncated, error}` where `output` is the exact
JSON text Go returns — Go `encoding/json` bytes: compact, map keys sorted, `<` `>` `&` escaped as
`<` `>` `&`, U+2028/U+2029 as ` `/` `, floats in Go's shortest form
(`1`, `1e+21`, `1e-7`, `0.0000025`, `100000000000000000000`, `-0`), float32 values formatted at
32 bits. When the output is longer than 65536 bytes, `output` is replaced by `output_len`,
`output_sha256` (hex), `output_head` and `output_tail` (first/last 256 bytes).

- `smart_truncate_json.json` — cases `{name, input | input_generator, max_len, redact: R,
  no_redact: R}`. `redact` = `SmartTruncate(v, max_len)`, `no_redact` = `smartTruncate(v, max_len,
  false)`. `input` is a JSON value; Go decodes it with `json.Unmarshal` into `any` (objects become
  `map[string]any`, numbers `float64`), so build the Rust value with `serde_json::from_str::<Value>`
  (the inputs only use numbers whose float64 value prints like the literal). `input_generator` is
  `{kind, value?, count?, then?, depth?}`: `repeat` = array of `count` copies of `value`;
  `repeat_then` = that followed by the items of `then`; `nested_maps` / `nested_lists` = the string
  `"leaf"` wrapped `depth` times in `{"n": ...}` / `[...]`.
- `smart_truncate_typed.json` — cases `{name, go_type, max_len, rust, go_json, redact: R,
  no_redact: R}` for Go values JSON cannot describe (NaN/±Inf, float32, int64/uint64 extremes,
  `json.Number`, `[]byte` binary/UTF-8/empty/nil, maps with int/bool/float/unsupported keys,
  tagged/embedded structs, `time.Time`, `json.RawMessage`, genai values). `rust` describes the
  equivalent serde value (a Rust expression or struct definition); `go_json` is set for the
  `genai_*` cases (deserialize the `gadk_genai` type from it). Byte slices are Rust
  `serialize_bytes` (`serde_bytes`); `struct_embedded_promotion` is an adaptation (serde has no
  embedding).
- `format_content_parts.json` — cases `{name, content, max_len, redact: P, no_redact: P}` with
  `P = {parts, truncated}` from `formatContentParts(content, max_len, redact)`; `content` is
  `genai.Content` JSON or `null` (nil `*Content`). `redact.parts` also equals
  `FormatContentParts(content, max_len)`. Each part is an object with `part_index` (int),
  `storage_mode`, and as applicable `mime_type`, `text`, `uri`, `object_ref`, `part_attributes`
  (a string holding exact SmartTruncate JSON text).
- `content_summaries.json` — cases `{name, content, max_len, redact: S, no_redact: S}` with
  `S = {summary, summary_truncated, response, response_truncated}` from
  `contentSummary(content, max_len, redact)` and `formatResponseContent(content, max_len, redact)`.
  Same 35 contents as `format_content_parts.json`.
- `parts_collector.json` — cases `{name, opts, contents, parts, pending, truncated,
  nonUploadTruncated, hasPendingText, reserve_calls}`. Build `partsCollector{opts}` and call
  `addContent(contents[i], i)` for each i (`null` = nil `*Content`). `opts = {maxLen, redact,
  offload, inlineLimit, connectionID, reserve: {mode, budget?}}` where `reserve.mode` is `none`
  (`reserveBytes` nil), `accept` / `reject` (always true / false) or `budget` (true while the running
  total of accepted reservations stays `<= budget`). `reserve_calls` lists the `n` of each
  `reserveBytes` call. `pending` entries are `{slot, contentOrd, partIdx, mimeType, isText, text,
  data_base64, data_nil, fallbackTruncated, reserved, size, contentType, payload_base64}` (the
  `pendingUpload` fields plus its `size()`, `contentType()`, `payload()`).
- `object_ref.json` — cases `{uri, details, authorizer, objectRef}`: `objectRef(uri, details,
  authorizer)`.
- `summary_builder.json` — cases `{name, spec, build_before, build_before_truncated, after_ops,
  build_after, build_after_truncated, apply_to_row}`. `spec = {isLLMRequest, isLLMResponse,
  contentDenied, maxLen, redact, roleTruncated, otherTruncated, usage, messages, ops}`; each message
  `{content, contentOrd, response, role, hasRole, isSystem}` becomes
  `newResponseMessageSummary(content, contentOrd, maxLen, redact)` when `response`, else
  `newMessageSummary(...)`, with `role`/`hasRole`/`isSystem` then set; `ops` are `{op: "omit",
  contentOrd, partIdx}` (`omitTextFromSummary`) or `{op: "retain", contentOrd, partIdx,
  fallbackText, fallbackTruncated}` (`retainInlineSummary`). `build_*` = `buildContent()` before and
  after the ops; `after_ops[i] = {needsRebuild, hasFailedUploadTruncated}` after op i;
  `apply_to_row` = an empty row after `applyToRow` (`content` is a JSON string).
- `normalize_function_response.json` — cases `{name, function_response, max_len, normalized, redact,
  no_redact}`: `function_response` is `genai.FunctionResponse` JSON (or null); `normalized` =
  `json.Marshal(normalizeFunctionResponse(fr))` (null for nil; inline `data` appears base64 only
  because of `json.Marshal`); `redact`/`no_redact` = `{part_attributes, truncated}` from
  `formatPartAttributes(map{"function_response": normalized-map}, max_len, redact)`, where the data
  bytes are sanitized as bytes (`[BINARY_DATA]` or text).

### redaction/

- `text_corpus.json` — cases, one per distinct input string, sorted: `{input, source, isSensitiveKey,
  sanitizeURI: [string, changed], sanitizeRawText: [string, changed], redactErrorMessage,
  scrubErrorText, redactJSONTextWithBudget: [string, changed, contentLost],
  canonicalizeCredentialMarker, hasCredentialPattern, mayMatchTextPatterns, SmartTruncate100: R}`.
  `redactJSONTextWithBudget` is called with depth 0 and a fresh budget of `maxSanitizeNodes`;
  `SmartTruncate100` is `SmartTruncate(input, 100)`. `source` is `json_formatter_test.go` (every
  string literal and constant `+` concatenation of string literals in ADK-Go's
  `json_formatter_test.go`, except test names, arguments of `t.Error*`/`t.Fatal*`/`t.Log*`/`t.Skip*`/
  `Sprintf`, struct tags and imports: 970 inputs) or `extra` (60 probe inputs: credential URIs,
  quoted/escaped/percent/`\x`/`\u`-encoded keys, Unicode-folded keys, BOM/NBSP-led JSON, deep JSON,
  duplicate keys). A function that panicked would be `{"panic": text}` (none does).
- `keys.json` — cases `{key, isSensitiveKey, normalizeKey, removeKeySeparators}`.

### wire/

One document per scenario, `wire/<name>.json`:

```
{
  "name", "description",
  "client": {"project", "endpoint": "<server>/bigquery/v2/", "options"},
  "call": {...},                 // what ran (see below)
  "exchanges": [{"request": Req, "response": Resp}],   // in order; [] when none
  "result": {"error": text|null, "error_type": [Go %T of each error in the Unwrap chain]|null},
  "logs": [Config.Logger.Printf lines],
  "job_id": {"pattern": "^[0-9a-f]{32}$", "distinct_ids": n} | null   // n = distinct IDs sent
}
Req  = {method, path, raw_query, query: {k: [v]}, headers: {Canonical-Key: [v]},
        body: JSON value | null, body_text: exact bytes as a string ("" when empty)}
Resp = {status, headers?: {k: v}, body: JSON value}
```

- The Go client ran against an `httptest` server with `bq.NewClient(ctx, project,
  option.WithEndpoint(server+"/bigquery/v2/"), option.WithoutAuthentication())`; the server answered
  the canned `Resp`s in order, each with `Content-Type: application/json; charset=UTF-8`.
- Recorded headers leave out `Authorization`, `User-Agent` and `X-Goog-Api-Client`. `Accept-Encoding:
  gzip` and `Content-Length` are added by Go's `net/http` transport; compare `Content-Type` and
  `If-Match` and the absence of others, not those two.
- `{{JOB_ID}}` stands for the job ID the client generated for `jobs.insert` (32 lowercase hex
  chars, `job_id.pattern`), in recorded paths, queries, bodies and error texts; in canned response
  bodies it was replaced by the generated ID before serving (a Rust fake must do the same).
- `body` was decoded with `UseNumber` and re-encoded with sorted keys; `body_text` keeps Go's field
  order. Compare bodies semantically (R9).
- `call.fn` is one of
  - `ensureTableAndSchema(...)`: `call.config` describes the `Config` (always `DefaultConfig()` with
    `ProjectID "test-project"` and a recording Logger, plus the noted overrides) and `call.denied`
    the denylist; the table is `client.DatasetInProject(cfg.ProjectID, cfg.DatasetID).Table(cfg.TableName)`.
  - `md := table.Metadata(ctx); maybeUpgradeSchema(...)` (`maybe_upgrade_direct`).
  - `bqViewRunner{client, projectID, datasetID}.RunViewDDL(ctx, view_name, query)`:
    `call.runner = {projectID, datasetID, client}` with `client` `live`, `live (project
    client-project)` (the bq client was created for `client-project`) or `dropped` (the weak
    pointer is empty), plus `view_name` and `query`.

Scenarios (41): table creation — `table_missing_create`, `table_missing_create_denylist`,
`table_missing_create_custom_clustering`, `table_missing_create_nil_clustering`,
`create_conflict_reread_patch`, `create_conflict_disable_upgrade`, `create_conflict_reread_fails`,
`create_fails`; reads — `get_permission_denied`, `get_unavailable_retried` (Go's client retries a
503 `tables.get`), `columns_match_no_write`,
`disable_schema_upgrade_existing`, `incompatible_schema`; upgrades — `additive_patch`,
`additive_patch_denylist`, `patch_412_retry`, `patch_412_exhausted`, `patch_412_then_columns_match`,
`patch_412_reread_fails`, `patch_fails`, `table_without_etag_patch` (no `If-Match` when the etag is
empty), `maybe_upgrade_direct`; views — `view_identical_no_job`, `view_non_view_table`,
`view_non_view_materialized`, `view_missing_create_done`, `view_differs_replace`,
`view_get_error_still_creates`, `view_job_pending_query_path` (`jobs.getQueryResults` with
`maxResults=0`, `location`, `formatOptions.useInt64Timestamp=true` until `jobComplete`, then
`jobs.get` with `fields=status,statistics`), `view_job_pending_no_config` (polls `jobs.get`),
`view_job_error`, `view_job_pending_then_error`, `view_job_pending_no_config_then_error`,
`view_job_insert_400`, `view_job_insert_409`, `view_job_insert_503_retried` (the client retries
`jobs.insert` with the same job ID: `distinct_ids` 1), `view_no_dataset_skips_get`, `view_query_without_as_newline`,
`view_runner_project_differs_from_client` (`tables.get` under the runner's project, `jobs.insert`
under the client's), `view_runner_empty_project_uses_client`, `view_dropped_client`.
