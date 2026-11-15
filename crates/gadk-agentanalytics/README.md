# gadk-agentanalytics

A port of ADK-Go's `plugin/agentanalytics`, the BigQuery Agent Analytics plugin: the plugin, its
session-event signal rows and its trace manager (sub-project 7b-3a) with Go's goroutine
attribution (7b-3b), on the foundations (7b-1)
and the transports (7b-2) — the BigQuery Storage Write client and stream writer, the Arrow batch
processor and the GCS content offloader. Re-exported by the `gadk` façade as
`gadk::agentanalytics` (feature `agentanalytics`, not in `default`).

```rust,ignore
use gadk_agentanalytics::{Config, Context, new_bigquery_agent_analytics_plugin_with_config};

let config = Config {
    project_id: "my-project".into(),
    dataset_id: "agent_analytics".into(),
    ..Config::default()
};
let plugin = new_bigquery_agent_analytics_plugin_with_config(&Context::background(), config).await?;
// Add `plugin` to the runner's plugins (first, before plugins that replace events or
// responses); the runner's plugin manager closes it, draining the queued rows.
```

- `plugin` — `new_bigquery_agent_analytics_plugin` (Go `NewBigQueryAgentAnalyticsPlugin`:
  project, dataset, table and the default config), `…_with_config` (a whole `Config`; a disabled
  config gives Go's disabled plugin, which only closes) and `…_with_clients` (a caller's
  `BigQueryClient` for table setup and views and a `WriteClient` for the rows) return a
  `gadk_agent::Plugin` with Go's 12 callbacks and close. Construction validates the config with
  Go's texts, creates or upgrades the table and starts the analytics views in the background.
  `as_bigquery_agent_analytics_plugin` recovers a constructed plugin's helpers
  (`BigQueryAgentAnalyticsPlugin::drop_stats`, `sync_views`) from the plugin's state slot.
  Every callback builds its rows synchronously, before any `.await`, and never fails a run: a
  panic while building a row is recovered and counted as a dropped row. Close stops the views,
  drains the batch processor on a task of its own (a plugin-manager close timeout cannot lose
  rows) and closes the owned offloader and the write client, once. Rows: identity, trace and span
  IDs, `attributes` with the ADK envelope and the OpenTelemetry section of the enclosing ADK
  span, content inline or offloaded to GCS, custom and session metadata, the state snapshot; the
  event allow / deny lists and denied columns filter them.
- The crate-internal `events` (Go `events.go`: tool origins from `Tool::origin_kind` /
  `unwrap` / `origin_agent(s)`, A2A detection, and the `STATE_DELTA`, `AGENT_TRANSFER`,
  `EVENT_COMPACTION`, `AGENT_STATE_CHECKPOINT`, HITL / pause, `A2A_INTERACTION`, `NODE_ERROR`,
  `NODE_OUTPUT`, `AGENT_RESPONSE` and user-message signal rows) and `trace` (Go
  `trace_manager.go`: invocation, agent, model and tool spans keyed by
  `gadk_agent::ActivationId`, the completed-run LRU with eviction and late callbacks, errors,
  retirement, settling, task mode, and Go's goroutine attribution of workflow nodes, parallel
  branches and worker items (7b-3b): the trace manager reads the callback's
  `gadk_agent::fiber` — id, spawn origin and attempt site, set by gadk-engine where Go starts
  those goroutines — and an ADK context's cancel cause (`gadk_agent::CancelCause`) when it
  needs it).

- `config` — `Config` with every field and default of Go's `Config` (`Config::default()` is
  `DefaultConfig()`, `Config::zero()` Go's `Config{}`), `ClientOptions` (Go's
  `[]option.ClientOption`: credentials, endpoint, HTTP client, headers), `RetryConfig`,
  `Logger` / `StdLogger` (Go `log.Default()`), `ContentFormatter`, `DropStats`, and the
  `GcsContentOffloader` trait 7b-2 implements.
- `schema` — `events_schema()` (Go `EventsSchema()`), denylist projections, `arrow_schema()` and
  `serialized_arrow_schema()` (an Arrow IPC stream: schema message and end-of-stream marker),
  and the additive `upgrade_table_schema`.
- `schema_upgrade` — `ensure_table_and_schema` / `maybe_upgrade_schema` over the
  `TableMetadataOps` trait: create with DAY partitioning, clustering and the schema-version
  label; on conflict re-read and upgrade; etag-guarded additive updates with Go's retry.
- `views` — `Config::view_definitions()` (the 25 per-event-type views, Go's SQL byte for byte),
  `BigQueryViewRunner` (Go `bqViewRunner.RunViewDDL`) and `sync_analytics_views`.
- `bigquery` — a BigQuery REST admin client (`BigQueryClient`, `TableOps`) sending the requests
  Go's `cloud.google.com/go/bigquery` v1.79.0 client sends (tables get / insert / patch, query
  jobs and their polling, Go's retry policy and error texts), over `gadk_model::rest` with
  Application Default Credentials. Feature `testutil` adds `bigquery::fake` (in-memory table
  ops, view runner, offloader and logger).
- `format_content_parts` and `smart_truncate` (Go `FormatContentParts`, `SmartTruncate`): the
  content formatter, whose sanitizer is a serde `Serializer` reproducing Go's reflection walk
  (depth and node budgets, Go's JSON text), with Go's sensitive-key, URI and credential
  redaction. The crate-internal parts collector, content summaries and redaction entry points
  serve the batch processor and the plugin's rows.
- `row` — `Row` (a `BTreeMap<String, RowValue>` with a closed `RowValue` enum, Go's
  `map[string]any` rows) and `QueuedRow` (a row with its typed side-channel data: content-parts
  summary builders and pending uploads).
- `storagepb` — the generated prost/tonic code of `google.cloud.bigquery.storage.v1` (googleapis
  protos vendored under `proto/`, regenerated by `scripts/gen-storagepb/run.sh`; the server side
  only with feature `testutil`).
- `write` — `gapic::WriteClient` (Go's gapic `BigQueryWriteClient`: CreateWriteStream, AppendRows,
  FinalizeWriteStream with gax retries, per-RPC metadata and timeouts, over tonic with TLS and
  Application Default Credentials), `arrow_batch` (Go's record batch encoder, size estimates and
  splits) and `stream_writer` (AppendRows requests, the trace id; the crate-internal stream
  writer with rotation, exactly-once offsets and retired-stream finalization).
- `batch_processor` — `BatchProcessor` (Go `BatchProcessor`): bounded queue with drop counters,
  ticker and size-triggered flushes, run-end flushes with a deferred batch, appends split by
  estimated and serialized size, close with drain and finalization.
- `offload` — `gcs::GcsOffloader` (Go's GCS offloader over the `cloud.google.com/go/storage`
  writer's HTTP upload path: multipart below 16 MiB, resumable 16 MiB chunks, CRC32C, retries, a
  412 counted as success) and the crate-internal upload orchestration (`resolveUploadsForBatch`:
  concurrent uploads under a batch timeout, upload cooldown, deferred uploads, object paths).
- `context::Context` is Go's `context.Context` (cancellation, deadlines, timeout causes,
  `AfterFunc`, grpc-go outgoing metadata); `Error` keeps Go's error texts and `%w` chains
  (`ApiError` is `gadk_model::rest::googleapi`'s).
- Feature `testutil`: `bigquery::fake` (in-memory table ops, view runner, offloader and logger),
  `testutil::fake_write_server` (an in-process tonic BigQuery Write server over an in-memory
  duplex connector), `testutil::arrow_rows` (decodes the rows of an AppendRows request) and the
  probe's row specs (`testutil::row_from_spec`).

Upgrade note: 7b-3a adds the public field `gadk_agent::PluginConfig::state` (the state slot
`as_bigquery_agent_analytics_plugin` reads), so a full `PluginConfig { .. }` literal without
`..PluginConfig::default()` now needs `state: None`.

Behavior, SQL, schemas, request bytes and error texts match ADK-Go except for the accepted
divergences in `docs/parity/7b-1.md`, `docs/parity/7b-2.md` and `docs/parity/7b-3a.md`, each
annotated in the source with a `// Divergence` comment naming its ledger.

## Tests and goldens

- Unit tests per module and ports of `json_formatter_test.go`, `schema_upgrade_test.go` and
  `views_test.go` (55 Go tests) in `src/parity_tests/` (ledger `docs/parity/7b-1.md`, checked by
  `python3 scripts/parity-check --ledger 7b-1`), and of `batch_processor_test.go` and
  `gcs_offloader_test.go` (171 Go tests, all ported — 42 of them in 7b-3a; ledger
  `docs/parity/7b-2.md`, `--ledger 7b-2`), and of `bigquery_agent_analytics_plugin_test.go`,
  `events_test.go`, `late_callbacks_test.go` and `trace_manager_test.go` (338 Go tests, all
  ported — the 99 trace-manager tests that need goroutine attribution in 7b-3b, in the
  `attr_*` files; ledger `docs/parity/7b-3a.md`, `--ledger 7b-3a`).
- `tests/golden_*.rs` compare the config projection, the BigQuery and Arrow schemas (decoded IPC
  streams), the view SQL and the formatter and redaction corpus with ADK-Go's outputs;
  `tests/rest_wire.rs`, `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs` replay the REST
  exchanges Go's client had with a fake server (shared replay in `tests/support/wire.rs`);
  `tests/golden_record_batch.rs` and `tests/golden_append_rows.rs` compare Arrow record batches,
  size estimates and AppendRows requests, `tests/gapic_write.rs` the gapic client's metadata,
  retries and timeouts over the fake write server, and `tests/rest_gcs_offloader.rs` replays the
  GCS uploads (multipart, resumable, 412, retries, errors); `tests/golden_plugin_rows.rs` runs
  the probe's scripted runner scenarios through a gadk `Runner` with the plugin and compares
  every row with the rows ADK-Go's plugin wrote.
- The fixtures in `tests/fixtures/` are written by the Go probe `scripts/gen-go-agentanalytics`
  (an overlay test in a temporary copy of ADK-Go's `plugin/agentanalytics`; its `SCENARIO.md`
  documents the corpus and formats). Regenerate with `scripts/gen-go-agentanalytics/run.sh`
  and update the counts in `scripts/check-fixtures.sh` if fixtures were added;
  `ADK_GO_DIR=… scripts/check-fixtures.sh` asserts the counts and, when `go` is on `PATH`, reruns
  the probe (the output must be identical); it also regenerates `storagepb` into a temporary
  directory and diffs it (skipped when cargo cannot fetch the generator's crates).

## License

Apache-2.0.
