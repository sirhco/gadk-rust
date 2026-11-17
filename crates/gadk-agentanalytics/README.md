# gadk-agentanalytics

The BigQuery Agent Analytics plugin: the plugin, its session-event signal rows and its trace
manager with attribution of workflow nodes, parallel branches and worker items to fibers, on top
of the foundations and the transports — the BigQuery Storage Write client and stream writer, the
Arrow batch processor and the GCS content offloader. Re-exported by the `gadk` façade as
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

- `plugin` — `new_bigquery_agent_analytics_plugin` (project, dataset, table and the default
  config), `…_with_config` (a whole `Config`; a disabled config gives a plugin that only closes)
  and `…_with_clients` (a caller's `BigQueryClient` for table setup and views and a
  `WriteClient` for the rows) return a `gadk_agent::Plugin` with 12 callbacks and close.
  Construction validates the config, creates or upgrades the table and starts the analytics
  views in the background.
  `as_bigquery_agent_analytics_plugin` recovers a constructed plugin's helpers
  (`BigQueryAgentAnalyticsPlugin::drop_stats`, `sync_views`) from the plugin's state slot.
  Every callback builds its rows synchronously, before any `.await`, and never fails a run: a
  panic while building a row is recovered and counted as a dropped row. Close stops the views,
  drains the batch processor on a task of its own (a plugin-manager close timeout cannot lose
  rows) and closes the owned offloader and the write client, once. Rows: identity, trace and span
  IDs, `attributes` with the ADK envelope and the OpenTelemetry section of the enclosing ADK
  span, content inline or offloaded to GCS, custom and session metadata, the state snapshot; the
  event allow / deny lists and denied columns filter them.
- The crate-internal `events` (tool origins from `Tool::origin_kind` / `unwrap` /
  `origin_agent(s)`, A2A detection, and the `STATE_DELTA`, `AGENT_TRANSFER`,
  `EVENT_COMPACTION`, `AGENT_STATE_CHECKPOINT`, HITL / pause, `A2A_INTERACTION`, `NODE_ERROR`,
  `NODE_OUTPUT`, `AGENT_RESPONSE` and user-message signal rows) and `trace` (invocation, agent,
  model and tool spans keyed by `gadk_agent::ActivationId`, the completed-run LRU with eviction
  and late callbacks, errors, retirement, settling, task mode, and the attribution of workflow
  nodes, parallel branches and worker items to fibers: the trace manager reads the callback's
  `gadk_agent::fiber` — id, spawn origin and attempt site, set by gadk-engine where it spawns
  those fibers — and an ADK context's cancel cause (`gadk_agent::CancelCause`) when it needs
  it).

- `config` — `Config` with every field and default (`Config::default()` is the standard
  configuration, `Config::zero()` the all-zero one), `ClientOptions` (credentials, endpoint,
  HTTP client, headers), `RetryConfig`, `Logger` / `StdLogger` (the standard logger),
  `ContentFormatter`, `DropStats`, and the `GcsContentOffloader` trait the `offload` module
  implements.
- `schema` — `events_schema()`, denylist projections, `arrow_schema()` and
  `serialized_arrow_schema()` (an Arrow IPC stream: schema message and end-of-stream marker),
  and the additive `upgrade_table_schema`.
- `schema_upgrade` — `ensure_table_and_schema` / `maybe_upgrade_schema` over the
  `TableMetadataOps` trait: create with DAY partitioning, clustering and the schema-version
  label; on conflict re-read and upgrade; etag-guarded additive updates with retry.
- `views` — `Config::view_definitions()` (the 25 per-event-type views),
  `BigQueryViewRunner` (runs a view's DDL) and `sync_analytics_views`.
- `bigquery` — a BigQuery REST admin client (`BigQueryClient`, `TableOps`) sending the requests
  of the Google Cloud BigQuery client library (tables get / insert / patch, query jobs and their
  polling, its retry policy and error texts), over `gadk_model::rest` with
  Application Default Credentials. Feature `testutil` adds `bigquery::fake` (in-memory table
  ops, view runner, offloader and logger).
- `format_content_parts` and `smart_truncate`: the content formatter, whose sanitizer is a serde
  `Serializer` that walks a value with depth and node budgets and writes JSON text, with
  sensitive-key, URI and credential redaction. The crate-internal parts collector, content
  summaries and redaction entry points serve the batch processor and the plugin's rows.
- `row` — `Row` (a `BTreeMap<String, RowValue>` with a closed `RowValue` enum) and `QueuedRow` (a
  row with its typed side-channel data: content-parts summary builders and pending uploads).
- `storagepb` — the generated prost/tonic code of `google.cloud.bigquery.storage.v1` (googleapis
  protos vendored under `proto/`, regenerated by `scripts/gen-storagepb/run.sh`; the server side
  only with feature `testutil`).
- `write` — `gapic::WriteClient` (the BigQuery Write client: CreateWriteStream, AppendRows,
  FinalizeWriteStream with retries, per-RPC metadata and timeouts, over tonic with TLS and
  Application Default Credentials), `arrow_batch` (the Arrow record batch encoder, size estimates
  and splits) and `stream_writer` (AppendRows requests, the trace id; the crate-internal stream
  writer with rotation, exactly-once offsets and retired-stream finalization).
- `batch_processor` — `BatchProcessor`: bounded queue with drop counters, ticker and size-triggered
  flushes, run-end flushes with a deferred batch, appends split by estimated and serialized size,
  close with drain and finalization.
- `offload` — `gcs::GcsOffloader` (the GCS offloader, following the Cloud Storage client library's
  HTTP upload path: multipart below 16 MiB, resumable 16 MiB chunks, CRC32C, retries, a 412 counted
  as success) and the crate-internal upload orchestration (resolving a batch's uploads: concurrent
  uploads under a batch timeout, upload cooldown, deferred uploads, object paths).
- `context::Context` carries cancellation, deadlines, timeout causes, after-cancel
  callbacks and gRPC outgoing metadata; `Error` keeps the error texts and source chains
  (`ApiError` is `gadk_model::rest::googleapi`'s).
- Feature `testutil`: `bigquery::fake` (in-memory table ops, view runner, offloader and logger),
  `testutil::fake_write_server` (an in-process tonic BigQuery Write server over an in-memory
  duplex connector), `testutil::arrow_rows` (decodes the rows of an AppendRows request) and the
  probe's row specs (`testutil::row_from_spec`).

Upgrade note: the public field `gadk_agent::PluginConfig::state` (the state slot
`as_bigquery_agent_analytics_plugin` reads) is new, so a full `PluginConfig { .. }` literal
without `..PluginConfig::default()` now needs `state: None`.

Upgrade note (engine-wide; all public API changes are additive): early stop now unwinds instead
of aborting. When a consumer drops a `Runner::run` stream, work the run
already started (tool calls, ParallelAgent branches, workflow node tasks) is cancelled plainly
and finished before the after-run callbacks, so a tool that ignores cancellation runs to
completion and delays after-run until it returns. The live runner detaches its session task, so
an in-flight live tool call also finishes. New in `gadk-agent`: `fiber`, `CancelCause`,
`InvocationContext::{cancel_with_cause, cause}`, `Context::context_path`, `CancelGuard::release`,
`SharedCauseError` and the `teardown` registry (`Teardowns`,
`InvocationContext::{teardowns, register_teardown, spawn_teardown}`, `ContextBuilder::teardowns`);
in `gadk-engine`: `llm::run_llm_agent_as_node` and `testutil::live`.

Behavior, SQL, schemas, request bytes and error texts follow ADK's BigQuery Agent Analytics
plugin, so tables and rows written by this crate match those written by ADK. The few accepted
differences are annotated in the source with plain `//` notes.

## Tests and goldens

- Unit tests per module, plus conformance tests in `src/parity_tests/` for the formatter, schema
  upgrade, views, batch processor, GCS offloader, plugin, events, late callbacks and trace
  manager (the trace-manager tests that need fiber attribution are in the `attr_*` files).
- `tests/golden_*.rs` compare the config projection, the BigQuery and Arrow schemas (decoded IPC
  streams), the view SQL and the formatter and redaction corpus with recorded ADK outputs;
  `tests/rest_wire.rs`, `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs` replay the
  recorded REST exchanges against a fake server (shared replay in `tests/support/wire.rs`);
  `tests/golden_record_batch.rs` and `tests/golden_append_rows.rs` compare Arrow record batches,
  size estimates and AppendRows requests, `tests/gapic_write.rs` the gapic client's metadata,
  retries and timeouts over the fake write server, and `tests/rest_gcs_offloader.rs` replays the
  GCS uploads (multipart, resumable, 412, retries, errors); `tests/golden_plugin_rows.rs` runs
  scripted runner scenarios through a gadk `Runner` with the plugin and compares every row with
  the rows recorded from ADK's plugin.
- The fixtures in `tests/fixtures/` were recorded from ADK's plugin; they are committed and not
  regenerated.

## License

Apache-2.0.
