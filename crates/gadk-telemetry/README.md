# gadk-telemetry

OpenTelemetry tracer, logger and meter providers for gadk, with OTLP/HTTP export and Google
Cloud export (`with_otel_to_cloud(true)`).

## Google Cloud export

With `with_otel_to_cloud(true)`:

- spans go to Cloud Trace and metrics to Cloud Monitoring (`telemetry.googleapis.com`);
- log records go to Cloud Logging (`logging.googleapis.com/v2/entries:write`), unless you
  supply your own logger provider with `with_logger_provider`.

### Credentials and projects

The export authenticates with the credentials given by `with_google_credentials`, or else with
Application Default Credentials (scope `cloud-platform`), loaded when telemetry starts; failing to
find them is a setup error. The quota project (sent as `x-goog-user-project`) and the resource
project (`gcp.project_id`) come from `with_gcp_quota_project` / `with_gcp_resource_project`, else
the credentials' project ID, else `GOOGLE_CLOUD_PROJECT`; with neither, setup fails. The identity
needs permission to write traces and metrics through the Telemetry API on the project (for example
`roles/telemetry.tracesWriter` and `roles/monitoring.metricWriter`).

The `gadk-launcher` `-otel_to_cloud` flag turns this export on; otherwise a
`with_otel_to_cloud` option in its `Config::telemetry_options` decides.

Log export through the API needs the Cloud Logging API enabled on the project and
`logging.logEntries.create` (for example `roles/logging.logWriter`) on the identity. A failed
export is reported once as a `tracing` warning under the target `gadk_telemetry::cloud_logging`,
then not again until a later export succeeds.

Log records carry prompts, completions and tool payloads only when content capture is on
(`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`). Turning it on sends that content to
Cloud Logging and is billed as Cloud Logging ingestion.

### Options

| Option / env var | Effect |
|---|---|
| `with_cloud_log_name("name")` | Log ID (default `gadk`): entries go to `projects/{project}/logs/{name}` |
| `with_cloud_logging_stdout()` or `GADK_TELEMETRY_LOGS_STDOUT=1` | Write structured JSON lines to stdout instead of calling the API (Cloud Run, GKE and Cloud Functions collect them) |

### Entry shape

- `jsonPayload`: `event.name`, `body` and `attributes` of the record (on stdout these are
  top-level fields).
- On stdout only: `message`, the summary line Cloud Logging shows for the entry:
  `<event.name>: <body>`, or whichever of the two the record has (only a string body counts),
  cut to 200 characters. Records with neither have no `message`.
- `severity` from the OTel severity number; `timestamp` from the record (RFC 3339, nanoseconds).
- `trace` (`projects/{project}/traces/{trace id}`), `spanId` and `traceSampled` when the record
  has a span context, so Cloud Logging shows the entry under its Cloud Trace span. On stdout:
  `logging.googleapis.com/trace`, `logging.googleapis.com/spanId`,
  `logging.googleapis.com/trace_sampled`.
- Resource: `cloud_run_revision` on a Cloud Run service, otherwise `global`.

### Size limits

An entry larger than 256,000 bytes has its longest strings shortened (each ending in
`…[truncated]`) until it fits, and gains `gadk.truncated: true` and `gadk.original_bytes`.
Requests are split to stay under Cloud Logging's request size limit.

### Flushing at exit

Log records are queued and exported in the background, at most 512 per export, at least once a
second. Call `Providers::shutdown().await` before the process exits: it exports what is still
queued, but waits at most 5 seconds (the OpenTelemetry SDK's shutdown timeout), and each
`entries:write` request may take up to 10 seconds. Records still queued when that time runs out
are dropped, and a full queue (2,048 records) drops new ones. If a process logs heavily just
before it exits:

- on Cloud Run, GKE or Cloud Functions, prefer `with_cloud_logging_stdout()`, which writes
  locally and does not wait on the network;
- otherwise, export more often with `OTEL_BLRP_SCHEDULE_DELAY` (milliseconds) so less is left
  for the final flush, and raise `OTEL_BLRP_MAX_QUEUE_SIZE` if bursts overflow the queue.
