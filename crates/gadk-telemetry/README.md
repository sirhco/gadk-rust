# gadk-telemetry

OpenTelemetry tracer, logger and meter providers for gadk, with OTLP/HTTP export and Google
Cloud export (`with_otel_to_cloud(true)`).

## Google Cloud export

With `with_otel_to_cloud(true)`:

- spans go to Cloud Trace and metrics to Cloud Monitoring (`telemetry.googleapis.com`);
- log records go to Cloud Logging (`logging.googleapis.com/v2/entries:write`), unless you
  supply your own logger provider with `with_logger_provider`.

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
