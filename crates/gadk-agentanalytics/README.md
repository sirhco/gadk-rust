# gadk-agentanalytics

The foundations of a port of ADK-Go's `plugin/agentanalytics` (the BigQuery Agent Analytics
plugin), sub-project 7b-1: everything that needs no streaming transport and no plugin. The
Storage Write stream, Arrow batch processor and GCS offloader (7b-2) and the plugin itself
(7b-3) build on it. Re-exported by the `gadk` façade as `gadk::agentanalytics` (feature
`agentanalytics`, not in `default`).

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
  are for 7b-2 / 7b-3.
- `context::Context` is Go's `context.Context` (cancellation and deadlines); `Error` keeps Go's
  error texts and `%w` chains.

Behavior, SQL, schemas, request bytes and error texts match ADK-Go except for the accepted
divergences in `docs/parity/7b-1.md`, each annotated `// Divergence (ledger 7b-1)` in the source.

## Tests and goldens

- Unit tests per module and ports of `json_formatter_test.go`, `schema_upgrade_test.go` and
  `views_test.go` (55 Go tests) in `src/parity_tests/` (ledger `docs/parity/7b-1.md`, checked by
  `python3 scripts/parity-check --ledger 7b-1`).
- `tests/golden_*.rs` compare the config projection, the BigQuery and Arrow schemas (decoded IPC
  streams), the view SQL and the formatter and redaction corpus with ADK-Go's outputs;
  `tests/rest_wire.rs`, `tests/rest_schema_upgrade.rs` and `tests/rest_views.rs` replay the REST
  exchanges Go's client had with a fake server (shared replay in `tests/support/wire.rs`).
- The fixtures in `tests/fixtures/` are written by the Go probe `scripts/gen-go-agentanalytics`
  (an overlay test in a temporary copy of ADK-Go's `plugin/agentanalytics`; its `SCENARIO.md`
  documents the corpus and formats). Regenerate with `scripts/gen-go-agentanalytics/run.sh`
  and update the counts in `scripts/check-fixtures.sh` if fixtures were added;
  `ADK_GO_DIR=… scripts/check-fixtures.sh` asserts the counts and, when `go` is on `PATH`, reruns
  the probe (the output must be identical).

## License

Apache-2.0.
