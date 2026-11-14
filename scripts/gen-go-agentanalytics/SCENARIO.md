# gen-go-agentanalytics scenario

`scripts/gen-go-agentanalytics` records what ADK-Go's `plugin/agentanalytics` (at
`docs/parity/ADK_GO_COMMIT`) computes and sends, as fixtures for `gadk-agentanalytics`. Sub-project
7b-1 (spec R1–R9): the `Config` defaults, the BigQuery and Arrow events schemas, the analytics view
SQL, the content formatter and redaction outputs over a corpus, and the BigQuery REST requests the
`cloud.google.com/go/bigquery` v1.79.0 client sends for table creation, schema upgrade and view DDL.
Sub-project 7b-2 (transports): Arrow record batches, AppendRows requests, the gapic BigQuery Write
client's per-RPC metadata and retries, the Cloud Storage offloader's requests, `validateConfig`,
CRC32C and the pure helpers of `gcs_offloader.go` (see "7b-2 fixture formats" below). Sub-project
7b-3a (plugin): the rows ADK-Go's plugin writes for scripted runner scenarios (see "7b-3a plugin row
goldens" below).

It is an overlay test, not a Go module: `run.sh` copies ADK-Go's `plugin/agentanalytics/*.go` into a
temp dir, installs `module.go.mod` / `module.go.sum` as its `go.mod` / `go.sum` (ADK-Go's
`plugin/agentanalytics/go.mod` plus `replace google.golang.org/adk/v2 => ../../../adk-go`, rewritten to
the absolute ADK-Go path, then `go mod tidy` under go1.26.10), adds `overlay/zz_probe_test.go.txt` as
`zz_probe_test.go`, `overlay/zz_probe_7b2_test.go.txt` (the 7b-2 generators, called from
`TestGenerateFixtures`) as `zz_probe_7b2_test.go` and `overlay/zz_probe_7b3a_test.go.txt` (the 7b-3a
plugin scenarios, likewise) as `zz_probe_7b3a_test.go` (package `agentanalytics`, so unexported
functions and the package's own test helpers are callable), and runs
`go test -run '^TestGenerateFixtures$'` with `AGENTANALYTICS_PROBE_OUT` set. ADK-Go is not modified.

Regenerate (from the repo root):

```
scripts/gen-go-agentanalytics/run.sh            # writes crates/gadk-agentanalytics/tests/fixtures
scripts/gen-go-agentanalytics/run.sh OUT_DIR    # writes OUT_DIR instead (replaced as a whole)
```

It prints `wrote 140 fixture files`. ADK-Go is `$ADK_GO_DIR`, else `adk-go` beside the main checkout
(the parent of git's common dir, so it works from a git worktree), else `../adk-go`. The Go toolchain
is `$GADK_PROBE_GOTOOLCHAIN` (default `go1.26.10`, fetched by the go command's `GOTOOLCHAIN` support
on first use); the outputs are byte-identical under go1.26.10 and go1.27.2. No clock reading and no
randomness reaches a fixture (BigQuery job IDs are replaced by `{{JOB_ID}}`; the 7b-2 captures
template multipart boundaries, idempotency tokens, the test server's address and the Go version, and
record server-side deadlines rounded to whole seconds and retries by attempt count, never their
random pauses; the 7b-3a rows replace IDs, timestamps, latencies and the ADK version by
placeholders), and no Go map iteration order decides an output, so `scripts/check-fixtures.sh`
reruns it into a temp dir and requires an empty `diff -r`, and asserts the per-directory file counts
(append_rows 12, arrow 9, config 4, formatter 8, gapic 3, gcs 12, helpers 11, record_batch 27,
plugin 6, redaction 2, schema 3, views 2, wire 41; 140 in all). The 7b-2 GCS captures upload four payloads of
about 16 MiB to a local test server and wait out the storage client's real (random, at most a few
seconds in all) retry pauses, so a run takes a few seconds.

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

## 7b-2 fixture formats

Same conventions as above (case files / document files, `encoding/json` without HTML escaping,
`error` = Go `err.Error()` or `null`, durations in nanoseconds unless a key says otherwise).
`sha256` values are lowercase hex of the named bytes. Every `*_file` / `file` path is relative to
`crates/gadk-agentanalytics/tests/fixtures/` (`file` within its own directory).

### Row specs

A Go row is a `map[string]any`; a fixture writes it as a **row spec**: a JSON object mapping each
key to a **value spec** `{"t": tag, "v": value}`, or `null` for a nil map (equivalent to an empty
row: every lookup misses). The probe builds the Go value from the spec it writes (the spec is
round-tripped through JSON first), so the spec is exactly what Go encoded. Tags, the Go value, and
the Rust `RowValue` (spec R1) to build:

| tag | `v` | Go value | Rust `RowValue` |
|---|---|---|---|
| `str` | string | `string` | `Str(v)` |
| `ts` | RFC 3339 text (any offset, up to 9 fraction digits) | `time.Time` (`time.Parse(time.RFC3339Nano, v)`) | `Timestamp(DateTime<Utc>)` of the same instant |
| `bool` | bool | `bool` | `Bool(v)` |
| `int64` | integer | `int64` | `Int(v)` |
| `int` | integer | `int` | `Int(v)` (Go reads `int` and `int64` alike wherever it accepts either) |
| `float64` | number literal | `float64` | `Float(v)` |
| `null` | (absent) | `nil` (the key is present, its value nil) | `Null` |
| `parts` | array of row specs or `null` | `[]map[string]any` (`null` element = nil map) | `Parts(Vec<Row>)` (`null` element = empty `Row`) |
| `map` | object of value specs | `map[string]any` | `Map(Row)` |
| `int32` | integer | `int32` | `Other(..)` |
| `any_list` | array of value specs | `[]any` | `Other(..)` |
| `str_map` | object of strings | `map[string]string` | `Other(..)` |
| `bytes` | std base64 | `[]byte` | `Other(..)` |

Every `Other` value is a type Go's type assertions reject everywhere (a scalar column → NULL,
`content_parts` → empty list, `object_ref` → NULL struct, `part_index` → NULL, estimates → 0).

### record_batch/

- `corpus.json` — cases `{name, about, denied, schema_file, rows, file, byte_len, sha256, num_rows,
  null_counts, decoded}`. Go: `bp := NewBatchProcessor(ctx, nil, "", Config{PayloadColumnDenylist:
  denied})` (so `bp.arrowSchema = projectArrowSchema(...)`, whose serialized bytes are
  `schema_file`: `arrow/events.arrows` or `arrow/projected_<denylist>.arrows`), `rec :=
  bp.buildRecordBatch(rows)`, `serializeRecord(rec, nil)` → `record_batch/<file>` (`<name>.batch`).
  `file` holds only the record batch IPC message (`ipc.GetRecordBatchPayload` + `WritePayload`:
  continuation marker, metadata length, flatbuffer `Message`, body; no schema, no end-of-stream
  marker). To decode it, read the IPC stream `schema_file[..len-8] ++ file ++ ff ff ff ff 00 00 00
  00`; the probe does exactly that and checks the decoded record equals the built one.
  `decoded` lists the decoded rows: one object per row with **every** column of the schema (sorted
  keys): `null` for NULL, strings, bools, `timestamp` as int64 microseconds since the epoch (Go
  `UnixMicro`: floor, so `1969-12-31T23:59:59.9999995Z` is `-1`), `content_parts` as an array of
  objects with all 7 part fields, `object_ref` as `null` or an object with all 4 fields.
  `null_counts` = `[[column, NullN], ...]` in schema order. Compare by decoding (arrow-rs and
  arrow-go bytes differ, as in 7b-1); `byte_len`/`sha256` document Go's bytes. Cases: `all_columns`,
  `empty_batch` (0 rows), `empty_row`, `nil_row`, `explicit_nil_values`, `wrong_typed_scalars`,
  `content_parts_missing`, `content_parts_wrong_type` (5 rows), `content_parts_empty`,
  `part_index_types` (`int64` 9007199254740993, `float64` 2.9 → 2, -2.9 → -2, 1e3 → 1000, `int`,
  negative, then `int32`/`str`/`bool`/`null`/absent → NULL), `part_fields_wrong_type`,
  `nested_object_ref`, `nil_part`, `unknown_and_side_channel_keys`, `timestamps` (6 rows incl.
  year 1 and 9999), `strings_unicode_and_empty`, `booleans` (3 rows), `multi_row` (3 rows), and
  `projected_<denylist>` for `content`, `content_parts`, `attributes`, `latency_ms`,
  `attributes_latency_ms`, `all` (the `all_columns` row). Note `empty_row` / `nil_row` /
  `explicit_nil_values` / `wrong_typed_scalars`: Go writes a NULL into the non-nullable `timestamp`
  field (arrow-go allows it; the IPC validity bitmap carries it).
- `estimate_row_bytes.json` — cases `{name, row, bytes}`: `estimateRowBytes(row)` (`row` a row spec;
  `null` = nil map).
- `split_batch.json` — cases `{name, rows, estimates, limit, chunk_lens}`:
  `splitBatchByEstimatedBytes(rows, limit)` returns consecutive chunks of lengths `chunk_lens` (an
  empty batch returns one empty chunk: `[0]`); `estimates[i] = estimateRowBytes(rows[i])`.

### append_rows/

- `requests.json` — cases `{name, about, stream_name, exactly_once, stream_offset, denied,
  writer_schema_file, batch_file, num_rows, trace_id, offset, file, byte_len, sha256,
  serialized_request_size, protojson}`. Go: `sw := newStreamWriter(ctx, nil, stream_name,
  <writer_schema_file bytes>, Config{ExactlyOnceDelivery: exactly_once})`,
  `sw.streamOffset.Store(stream_offset)`, `req := sw.newAppendRowsRequestLocked(<batch_file bytes or
  nil when batch_file is null>, num_rows)`; `append_rows/<file>` (`<name>.pb`) = `proto.Marshal(req)`
  (identical to the deterministic encoding; fields in field-number order), `byte_len` its length,
  `serialized_request_size` = `sw.serializedRequestSize(...)` (= `proto.Size(req)` = `byte_len`).
  `offset` = `req.Offset.Value` or `null` when unset (exactly-once only; offset 0 is a present,
  empty `Int64Value`). `trace_id` is the probe binary's `streamWriterTraceID()`
  (`google-adk-bq-logger-go`, see `trace_ids.json`): to compare a Rust request (whose trace id is
  gadk's), set its `trace_id` to this value first, then compare bytes and size. `protojson` =
  `protojson.Marshal(req)` decoded and re-encoded with sorted keys (bytes std base64, int64 as
  strings; empty `write_stream` and zero `row_count` omitted). Cases: `default_stream`,
  `named_stream`, `empty_stream_name`, `multi_row`, `empty_batch`, `nil_batch_bytes`,
  `projected_schema`, `exactly_once_offset_0`, `exactly_once_offset_42`, `exactly_once_offset_large`.
- `trace_ids.json` — `{about, cases: [{version, trace_id}], probe_adk_version, probe_trace_id}`:
  `streamWriterTraceIDForVersion(version)`; the probe's test binary has no ADK module version in its
  build info, so `probe_adk_version` is `""` and `probe_trace_id` the bare prefix.

### gapic/

The real `bqstorage.BigQueryWriteClient` (bigquery v1.79.0, `option.WithGRPCConn` to a grpc-go
server on `127.0.0.1:0` serving a recording `BigQueryWrite` fake).

- `call_options.json` — `{about, <RPC>: {timeout_ns, retry: {codes, initial_ns, max_ns,
  multiplier} | null}}` for the six RPCs: `defaultBigQueryWriteCallOptions()` as read from a new
  client's `CallOptions` (gax settings resolved; the `OnCodes` retryer's codes and `Backoff` read by
  reflection). `timeout_ns` 0 = none. gax applies the timeout only when the call context has no
  deadline, and pauses `1 + rand(cur)` ns before each retry, then `cur = min(cur*multiplier, max)`.
- `metadata.json` — cases `{name, about, calls, result}`: `calls` = every RPC the server saw, in
  order: `{rpc, metadata, timeout_s, request | requests}`. `metadata` = the incoming metadata
  (`metadata.FromIncomingContext`) with sorted keys and value lists in order, `:authority` left
  out, `gl-go/<version>` in `x-goog-api-client` written `gl-go/{{GO_VERSION}}`; it includes grpc-go's
  own `content-type`, `grpc-accept-encoding` and `user-agent` (transport-specific: compare
  `x-goog-request-params`, `x-goog-api-client`'s presence and the caller keys). `timeout_s` = the
  server-side deadline's remaining time rounded to whole seconds, `null` without a deadline.
  `request` = protojson of the unary request; AppendRows has `requests` = `[{write_stream,
  trace_id, offset, row_count}]`. `result = {value, error}`: the created stream name, or the
  response as protojson. Cases: `create_write_stream` (caller metadata `x-test-caller:
  preserve-me`, 1200 s), `create_write_stream_escaped_parent`, `create_write_stream_caller_deadline`
  (30 s caller deadline wins), `create_write_stream_caller_request_params` (the caller's
  `x-goog-request-params` value first, then the client's), `create_write_stream_caller_api_client`
  (a caller `x-goog-api-client` in outgoing metadata is replaced by the client's),
  `finalize_write_stream` (600 s), `finalize_write_stream_escaped_name`, and `append_rows_*` through
  `streamWriter.append` (no timeout; `x-goog-request-params: write_stream=<url.QueryEscape(name)>`
  only for a non-empty stream name, plus the constructor context's metadata): `named_stream`,
  `default_stream`, `escaped_stream`, `empty_stream_name`, `exactly_once` (offset 0).
- `retry.json` — cases `{rpc, script, attempts, status, [open_status, send]}`: the server answers
  call *i* with `script[i]` (`OK` = success, otherwise `status.Errorf(code, "zz scripted <Code>")`);
  the probe keeps the default retry codes and timeouts but sets the backoff to 1 ms (pauses are
  random; only `attempts` = calls the server saw and the final `status` = `{code, message, error}`
  or `null` are recorded). CreateWriteStream runs through `createCommittedWriteStream` (so `error`
  carries `failed to create committed write stream: ` and `message` is the wrapped text),
  FinalizeWriteStream through the client, each with `[code, code, OK]` for Unavailable,
  DeadlineExceeded, ResourceExhausted (retried: 3 attempts, success), Internal, Aborted, Unknown,
  PermissionDenied, NotFound, InvalidArgument (1 attempt, error). AppendRows with `[code]` for
  Unavailable, ResourceExhausted, Internal: `open_status` (`client.AppendRows(ctx)`) is `null`
  because grpc-go's `NewStream` does not wait for the server, `status` is the first `Recv` error,
  `send` = `{is_eof, error}` of a `Send` after that `Recv`.

### gcs/

One document per scenario, `gcs/<name>.json`: `newGCSOffloader(ctx, "test-bucket",
option.WithEndpoint(server+"/storage/v1/"), option.WithoutAuthentication())` (cloud.google.com/go/
storage v1.69.0, `STORAGE_EMULATOR_HOST` empty), one `UploadContent(ctx, GCSUploadRequest{Path,
Data, ContentType})`, then `Close()`, against an `httptest` server answering each request in order.

```
{
  "name", "description",
  "client": {"bucket", "endpoint": "{{SERVER}}/storage/v1/", "options"},
  "call": {"path", "content_type",
           "data": {"len", "sha256", "crc32c" (uint32), "crc32c_base64" (std base64 of the
                    big-endian bytes), "base64" | ("pattern": "i % 251", byte i = i mod 251)}},
  "exchanges": [{"request": GReq, "response": {"status", "headers": {k: v}, "body": text}}],
  "result": {"uri", "error", "error_type" (Go %T chain), "is_owned_upload_error", "googleapi_code",
             "is_precondition_failed", "safe_upload_error_summary", "close_error"}
}
GReq = {method, path, raw_query, query: {k: [v]}, headers: {Canonical-Key: [v]}, body: GBody}
GBody = {len, multipart: [{headers, len, sha256, text?, json?}], boundary_len}   // multipart/related
      | {len, sha256, text?, json?}                                             // otherwise
```

- Templating: `{{SERVER}}` = the server's `http://127.0.0.1:<port>`; `{{BOUNDARY}}` = the random
  multipart boundary (60 hex chars, `boundary_len`); `{{IDEMPOTENCY_TOKEN_n}}` = the n-th distinct
  `X-Goog-Gcs-Idempotency-Token` (a random UUID; the storage client also sends it as
  `gccl-invocation-id/…` in `X-Goog-Api-Client`); `gl-go/{{GO_VERSION}}`. `User-Agent` and
  `Authorization` are not recorded; `Accept-Encoding: gzip` and `Content-Length` come from Go's
  `net/http`.
- Multipart parts: `text`/`json` (sorted keys) for the `application/json` metadata part — Go's
  metadata JSON is `{"bucket","contentType","crc32c","name"}` in that order plus a trailing newline;
  the media part by `len` + `sha256`. Other bodies: `text`/`json` when the body is JSON under
  64 KiB, else only `len` + `sha256`.
- Resumable uploads: the initiation `POST …/o?alt=json&ifGenerationMatch=0&name=…&prettyPrint=false
  &projection=full&uploadType=resumable` (metadata JSON body without `crc32c`,
  `X-Upload-Content-Type`) is answered `200` with `Location: {{SERVER}}/upload/storage/v1/b/
  test-bucket/o?uploadType=resumable&upload_id=zz-upload-1`; each chunk is a `POST` to that
  location with `Content-Range: bytes a-b/*` (non-final), `bytes a-b/total` or `bytes */total`
  (an empty final chunk) and `X-Guploader-No-308: yes`; the final chunk carries `X-Goog-Hash:
  crc32c=<base64>`. The fake answers a non-final chunk `200` with `X-Http-Status-Code-Override: 308`
  and `Range: bytes=0-<received-1>` (what Cloud Storage does for `X-Guploader-No-308`), the final
  chunk with the object resource. Each chunk gets a new idempotency token; a resent chunk keeps it
  and increments `gccl-attempt-count`.
- Error responses are Google JSON errors `{"error":{"code","message","errors":[{"message",
  "domain":"global","reason"}]}}`.

Scenarios (12): `multipart_small` (7 bytes `image/png`), `multipart_text` (UTF-8 text, `text/plain;
charset=utf-8`), `multipart_empty` (0 bytes), `multipart_largest` (16 MiB − 1: still multipart),
`resumable_exact_16mib` (one 16 MiB chunk `bytes 0-16777215/*` then an empty final chunk `bytes
*/16777216`), `resumable_over_16mib` (16 MiB chunk then a 1-byte final chunk),
`resumable_chunk_503_retried` (the first chunk answered 503 once, resent), `precondition_412`
(success, `gs://` URI), `retry_503` (503 then 200: 2 attempts), `retry_429_then_500` (3 attempts),
`error_403`, `error_400` (owned upload errors, 1 attempt).

### config/validate_config.json

Cases `{name, input, result, with_defaults}`: `input` is a Config projection (as in
`apply_defaults.json`); `result` = `validateConfig(input)` and `with_defaults` =
`validateConfig(applyDefaults(input))`, each `{denied, error}` with `denied` the
PayloadColumnDenylist set sorted (`[]` when empty, `null` on error). The RetryConfig error formats
the struct with `%+v` (`{MaxRetries:-1 InitialDelay:100ms MaxDelay:10s Multiplier:2}`: Go
`time.Duration.String()` and `%v` floats).

### helpers/

- `constants.json` — document: the 7b-2 constants (`maxAppendRequestBytes`,
  `maxSerializedAppendRequestBytes`, `perRowOverheadBytes`, `perContentPartOverheadBytes`, the three
  `projected*` field lists, `defaultRotateTimeout_ns`, `streamWriterTraceIDPrefix`,
  `gcsUploadTimeout_ns`, `mediaOffloadedText`, `uploadFailedText`, `textPreviewRunes`,
  `textPreviewSuffix`, `textUploadContentType`, `missingTraceID`/`missingSpanID`/`missingEventID`,
  `fallbackExtension`, `defaultMaxPendingUploadBytes`, `mimeExtensions` as sorted `[type, ext]`
  pairs, the texts of `errInvalidGCSURI` / `errUploadCooldown` / `errBatchUploadTimeout`,
  `googleapiDefaultUploadChunkSize`).
- `crc32c.json` — cases `{name, len, crc32c, hex, base64_be, data_base64 | data_pattern}`:
  `crc32.Checksum(data, crc32.MakeTable(crc32.Castagnoli))`; `base64_be` = std base64 of the 4
  big-endian bytes (the `crc32c` metadata field / `X-Goog-Hash` value). Inputs over 64 bytes are
  `data_pattern` `i % 251` of `len` bytes (1 MiB, 16 MiB).
- `gcs_object_path.json` — cases `{args: {prefix, ts, trace_id, span_id, event_id, content_ord,
  part_idx, ext}, path}`: `gcsObjectPath(...)`, `ts` RFC 3339.
- `path_segment.json` — cases `{id, fallback, result}`.
- `extension_for_mime.json` — cases `{mime_type, ext}`; only inputs answered by `mimeExtensions` or
  the `.bin` fallback (other types consult the host's MIME tables through `mime.ExtensionsByType`).
- `is_safe_extension.json` — cases `{ext, safe}`.
- `text_preview.json` — cases `{name, text, preview}` (valid UTF-8 inputs only).
- `gcs_metadata_details.json` — cases `{content_type, size, details}` (`details` is Go
  `json.Marshal` text: HTML characters and U+2028/U+2029 escaped).
- `strip_uri_queries.json` — cases `{input, output}`.
- `safe_upload_error_summary.json` — cases `{name, go, error_text, summary}`: `go` is the Go
  expression of the error, `error_text` its `Error()`; the `error type %T` summaries name Go types
  (a Rust port maps its own error kinds to these classes).
- `object_path.json` — cases `{name, gcs_path_prefix, row, upload: {content_ord, part_idx,
  mime_type}, path}`: `bp.objectPath(row, pendingUpload{...})` on a processor with `Config{GCSPathPrefix:
  gcs_path_prefix}`; every row has a `ts` timestamp (a row without one uses `time.Now()`).

## 7b-3a plugin row goldens

`overlay/zz_probe_7b3a_test.go.txt` (`gen7b3a`) runs ADK-Go's real plugin under an ADK runner and
records the rows it writes, one file per scenario under `plugin/`. `tests/golden_plugin_rows.rs`
runs the same scenarios through a gadk `Runner` and compares its rows, normalized by the same
rules, with these files.

### How a scenario runs (Go)

- Plugin: `newTestPluginWithConfig` of `bigquery_agent_analytics_plugin_test.go`, i.e.
  `NewBigQueryAgentAnalyticsPluginWithClients` over `DefaultConfig()` with `Enabled = true`,
  `ProjectID "test-project"`, `DatasetID "test_dataset"`, `TableName "test-table"` (plus the
  scenario's `CustomMetadataAllowlist`), a BigQuery REST client whose transport answers every
  request `200 {}` (table creation succeeds), and a write client connected to the package's fake
  BigQuery Storage Write server, which records every `AppendRowsRequest`.
- `adkVersion` is pinned to `"2.0.0-probe"` while the scenarios run (a test binary has no version
  for the replaced ADK module, and the key would be missing), so `attributes.adk.adk_version` is
  present; it is normalized away.
- Runner: `runner.New` with `AppName "test-app"`, the scenario's root agent,
  `session.InMemoryService()`, `AutoCreateSession: true` and the plugin. Each turn is
  `r.Run(ctx, "user-1", "session-1", genai.NewContentFromText(<turn>, "user"), agent.RunConfig{})`,
  iterated to the end; errors yielded by the run are ignored (the error paths are part of the
  scenarios).
- After the last turn the plugin is closed (`Close` flushes the queue), and the rows of every
  recorded `AppendRowsRequest` are decoded in order with the package's `decodeArrowRows`: a column
  that is NULL is left out of the row, and so is an empty `content_parts` list.
- Scripted model: `zzScriptedModel`, named `"mock"`, whose every `GenerateContent` call yields
  exactly the next queued item (one response, or one error) and the error `"no data"` once the
  queue is empty: `gadk_engine::testutil::MockModel::new(vec![...])`. A text item is
  `LLMResponse{Content: genai.NewContentFromText(text, "model")}`; a call item is
  `LLMResponse{Content: {Role "model", Parts [{FunctionCall {ID, Name, Args}}]}}`; no other response
  field is set.

### Scenarios

| file | root agent | script | rows | row types |
|---|---|---|---|---|
| `tool_call.json` | LLM agent `weather_agent` (description "Answers weather questions.", instruction "Answer weather questions with the get_weather tool.") | model: call `get_weather {"city":"Paris"}` id `call-weather-1`; text "It is sunny in Paris." Function tool `get_weather` ("Returns the weather forecast for a city.", args `{city string}`) sets state delta `last_city = <city>` and returns `{"forecast":"sunny"}` | 13 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, LLM_REQUEST, LLM_RESPONSE, TOOL_STARTING, TOOL_COMPLETED, STATE_DELTA, LLM_REQUEST, LLM_RESPONSE, AGENT_RESPONSE, AGENT_COMPLETED, INVOCATION_COMPLETED |
| `tool_error.json` | LLM agent `calc_agent` | model: call `divide {"a":1,"b":0}` id `call-divide-1`; text "Cannot divide by zero." Function tool `divide` ("Divides a by b.", args `{a int, b int}`, result `{quotient int}`) fails with `division by zero` when b is 0 | 12 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, LLM_REQUEST, LLM_RESPONSE, TOOL_STARTING, TOOL_ERROR, LLM_REQUEST, LLM_RESPONSE, AGENT_RESPONSE, AGENT_COMPLETED, INVOCATION_COMPLETED |
| `transfer.json` | LLM agent `coordinator` ("Routes requests.") with sub-agent LLM agent `helper` ("Helps with anything.") | coordinator model: call `transfer_to_agent {"agent_name":"helper"}` id `call-transfer-1`; helper model: text "Helper here, how can I help?" | 15 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, LLM_REQUEST, LLM_RESPONSE, TOOL_STARTING, TOOL_COMPLETED, AGENT_TRANSFER, AGENT_STARTING, LLM_REQUEST, LLM_RESPONSE, AGENT_RESPONSE, AGENT_COMPLETED, AGENT_COMPLETED, INVOCATION_COMPLETED |
| `model_error.json` | LLM agent `failing_agent` | model: error `model failure 404` | 7 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, LLM_REQUEST, LLM_ERROR, AGENT_ERROR, INVOCATION_ERROR |
| `confirmation.json` | LLM agent `bank_agent` | model: call `transfer_funds {"amount":100}` id `call-funds-1`. Function tool `transfer_funds` ("Transfers funds.", args `{amount int}`, result `{status string}`, returns `{"status":"done"}`) with `RequireConfirmation: true`; the run pauses on ADK's `adk_request_confirmation` call (one turn) | 11 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, LLM_REQUEST, LLM_RESPONSE, TOOL_STARTING, TOOL_COMPLETED, HITL_CONFIRMATION_REQUEST, TOOL_PAUSED, AGENT_COMPLETED, INVOCATION_COMPLETED |
| `session_signals.json` | custom agent `signal_agent` ("Emits session signals.") yielding the 8 events below; `CustomMetadataAllowlist ["adk_agent_state","end_of_agent"]` | — | 19 | USER_MESSAGE_RECEIVED, INVOCATION_STARTING, AGENT_STARTING, EVENT_COMPACTION, STATE_DELTA, AGENT_STATE_CHECKPOINT, HITL_CREDENTIAL_REQUEST, TOOL_PAUSED, HITL_CONFIRMATION_REQUEST, HITL_INPUT_REQUEST, HITL_CREDENTIAL_REQUEST_COMPLETED, HITL_CONFIRMATION_REQUEST_COMPLETED, HITL_INPUT_REQUEST_COMPLETED, A2A_INTERACTION, NODE_ERROR, NODE_OUTPUT, AGENT_RESPONSE, AGENT_COMPLETED, INVOCATION_COMPLETED |

Every scenario is one turn; the user messages are "What is the weather in Paris?", "Divide 1 by
0.", "I need help.", "Hello?", "Send 100 to Bob." and "Emit the signals.". Each fixture's `turns`
holds them. Tool argument and result structs are serialized with the JSON names shown (lower-case).

The `session_signals` events are each `session.NewEvent(ctx, invocationID)` with `Author
"signal_agent"` and the invocation's branch (empty), and then:

1. `Actions.Compaction = {StartTimestamp 2026-01-02T03:04:05Z, EndTimestamp 2026-01-02T03:04:06Z,
   CompactedContent: text "Summary of earlier turns." role "model"}`.
2. `CustomMetadata = {"adk_agent_state": {"step": 1}, "end_of_agent": false}`, `Actions.StateDelta =
   {"progress": "half", "temp:scratch": "x"}`.
3. Content role "model", one part `FunctionCall{ID "cred-1", Name "adk_request_credential", Args
   {"auth_scheme": "oauth2"}}`; `LongRunningToolIDs ["cred-1"]`.
4. Content role "model", parts `FunctionCall{ID "confirm-1", Name "adk_request_confirmation", Args
   {"hint": "Approve?"}}` and `FunctionCall{ID "input-1", Name "adk_request_input", Args {"message":
   "Your name?"}}` (not long-running).
5. Content role "user", parts `FunctionResponse{ID "cred-1", Name "adk_request_credential", Response
   {"token": "granted"}}`, `FunctionResponse{ID "confirm-1", Name "adk_request_confirmation",
   Response {"confirmed": true}}`, `FunctionResponse{ID "input-1", Name "adk_request_input", Response
   {"name": "Ada"}}`.
6. `CustomMetadata = {"a2a:request": {"method": "message/send"}, "a2a:response": {"status":
   "completed"}}`.
7. `NodeInfo = {Path "signal_agent/step@1"}`, `ErrorCode "STEP_FAILED"`, `ErrorMessage "step
   failed"`, `Output {"result": 42}`.
8. Content `genai.NewContentFromText("All signals sent.", "model")`.

Together the six files hold every row type constant of `bigquery_agent_analytics_plugin.go:54-94`
(28; the legacy `"EVENT"` alias is a filter name, never a row type). No scenario uses a workflow
agent, `ParallelAgent` or a parallel worker (their attribution is 7b-3b).

### File format

Document file, 2-space indented, keys sorted, trailing newline:

```
{"about": <prose>, "app_name": "test-app", "config": {"CustomMetadataAllowlist": [..] | null,
 "DatasetID": "test_dataset", "ProjectID": "test-project", "TableName": "test-table"},
 "rows": [<row>, ...], "scenario": <file stem>, "session_id": "session-1",
 "turns": [<user text>, ...], "user_id": "user-1"}
```

`rows` are in the order the fake write server received them (= the order the plugin appended
them). A row is the decoded Arrow row as a JSON object: string columns are strings, `is_truncated`
a boolean, `content_parts` an array of objects (`part_index` an integer), and the JSON-typed columns
`content`, `attributes`, `latency_ms` (and `content_parts[].object_ref.details`) are parsed JSON
values, numbers kept as written by Go (`json.Number`), not their JSON text. `part_attributes` is a
STRING column and stays text. `timestamp` is normalized (below). Columns that are NULL are absent.

### Normalization

Applied to each row, after the conversion above, by `zzNormalizeRows` (Go) and identically by the
golden test (Rust) to its own rows. One numbering state per scenario file: for each kind, the
distinct real values are numbered from 1 in order of first appearance, walking the rows in order
and, inside a row, depth-first with object keys in byte-wise sorted order and array elements in
order. Rules, by object key (at any depth unless stated):

| key | replacement |
|---|---|
| `timestamp` (top-level column only) | `"<timestamp>"` |
| `adk_version` | `"<adk_version>"` |
| any key inside the top-level `latency_ms` object whose value is a number | `"<ms>"` |
| `event_id` | `"event-N"` (the row's `event_id` column and `attributes.adk.event_id` are the same value, so they get the same N) |
| `source_event_id` | `"source-event-N"` (session event IDs) |
| `invocation_id` | `"invocation-N"` |
| `span_id`, `parent_span_id` | `"span-N"` (one numbering for both, so parent links stay checkable) |
| `trace_id` | `"trace-N"` |

Only non-empty string values of the ID keys are replaced. Besides, every string value anywhere
(not an object key) that matches `^adk-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`
(a function call ID ADK generated, such as the `adk_request_confirmation` call) becomes
`"adk-call-N"`. Kept as is: `session_id` (`"session-1"`, fixed by the run), `user_id`, scripted
function call IDs (`call-*`, `cred-1`, ...), and everything else, including the compaction
`start_timestamp`/`end_timestamp` epoch seconds (fixed by the scenario).

Numbers: Go writes `float64` values without a fractional part as integers (the compaction epoch
seconds `1767323045`); compare numbers by value (`f64`), not by JSON spelling.
