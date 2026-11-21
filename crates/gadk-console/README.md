# gadk-console

`gadk-console`, a local web console for gadk agents. It shows the traces your agents exported
to Cloud Trace, with each span's Cloud Logging entries attached, and it browses the
`gadk-prompts` registry: versions, diffs and labels. With `--reports`, it also shows the reports
of `gadk-eval-monitor` monitors. With a price file (`--prices`) it prices token counts, and with the BigQuery trace source it shows usage and cost by model, agent, prompt version and trace. It runs on your machine, listens on 127.0.0.1 only, and reads
Google Cloud with your Application Default Credentials.

```sh
cargo install gadk-console

gcloud auth application-default login
gadk-console --project my-project --store 'firestore://my-project/(default)'
# http://127.0.0.1:8765/
```

The console prints its URL and serves until you stop it (Ctrl-C).

| Flag | Default | Meaning |
|---|---|---|
| `--project <p>` | required | GCP project whose traces and logs are read |
| `--store <url>` | none | Prompt store: `file://<dir>` or `firestore://<project>/<database>[/<root>]`. Without it the prompt pages say "no store configured" and the label routes return 404. |
| `--reports <url>` | none | Monitor report store: `file://<dir>`, a directory, or `gs://<bucket>[/<prefix>]` (the `--out` of `gadk-eval monitor`, or the `<url>` of `gadk-eval report`). Without it the Evals pages answer 404 "no report store configured". |
| `--port <n>` | `8765` | Port on 127.0.0.1; `0` picks a free one (the printed URL shows it) |
| `--log-id <id>` | `gadk` | Cloud Logging log id the agents write (`with_cloud_log_name` in `gadk-telemetry`) |
| `--prices <file>` | none | Price file (YAML, USD per 1,000,000 tokens per model). Without it, cost cells show `—`; token counts still show. See [Usage and cost](#usage-and-cost). |

Exit codes: 2 for a usage error (clap's message), 1 when startup fails (a bad store URL, a bad
report store URL, a bad price file, missing credentials for a `gs://` report store, or a port already in use), with
a one-line `gadk-console: …` message.

## Prerequisites

- Agents export with `gadk-telemetry`'s `with_otel_to_cloud(true)`: spans go to Cloud Trace and
  log records to Cloud Logging under the log id `gadk`, each entry carrying its span id. See
  the [`gadk-telemetry` README](../gadk-telemetry/README.md).
- Application Default Credentials for an identity with, on the project:
  - `roles/cloudtrace.user` (read traces);
  - `roles/logging.viewer` (read log entries);
  - for a Firestore store, Firestore access on the store's project: `roles/datastore.viewer` to
    browse, `roles/datastore.user` to change labels;
  - for a `gs://` report store, `roles/storage.objectViewer` on the bucket.
- The Cloud Trace and Cloud Logging APIs enabled on the project.

Traces are read through the Cloud Trace v1 client in
[`gadk-cloudtrace`](../gadk-cloudtrace/README.md) (the same client `gadk-eval-monitor`'s
`cloud_trace` source uses); log entries come from the Cloud Logging API.

Credentials are built on the first GCP call, so the console starts without them. A `gs://` report
store is the exception: its credentials are looked up at startup. A missing role or disabled API
shows as an error banner with a hint ("needs roles/cloudtrace.user",
"needs roles/logging.viewer", "enable <service>").

## Pages

| Page | Shows |
|---|---|
| `/traces` | Traces, newest first, 50 per page. Filters: time range (`1h`, `6h`, `24h`, `7d`), root span name prefix, agent (`gen_ai.agent.name`), session (`gen_ai.conversation.id`). |
| `/traces/{id}` | The span waterfall (tree order, indented by depth). Selecting a span shows its labels and the log entries whose `spanId` matches it; entries matching no span are listed as "unattached". A `render_prompt …` span with `prompt.name` and `prompt.version` links to that prompt version. A trace that ended less than 2 minutes ago notes that logs may still be arriving. Model calls (spans whose `gen_ai.operation.name` is `generate_content`) are listed with their tokens and cost, plus totals. |
| `/prompts` | Every prompt in the store: latest version, labels and when the latest version was created. |
| `/prompts/{name}` | All versions (newest first) with their labels; `?v=<n>` shows a version's template and config, `?a=<n>&b=<m>` diffs two versions. Forms set, move and remove labels, conditional on the label not having changed since the page was rendered (a stale page gets 409). "Production metrics" (BigQuery source, `?range=`, default `7d`): calls, mean tokens and cost, p50/p95 latency and, with `--reports`, eval pass rate per version. |
| `/evals` | The newest 100 monitor reports: a card per monitor (latest pass rate, errors, and a pass-rate trend over its listed reports), then a table of reports. `?spec=<name>` shows only that monitor's newest 100. Reports are read once and kept in memory (up to 256), so the first load of a `gs://` store can take a while. |
| `/evals/{report}` | One report: window, source, runs, pass rate and errors, then one row per run with a chip per scorer. Expanding a run shows each score's outcome, judge score and rationale, the run error, notes on incomplete transcripts, and the transcript with tool calls; runs that recorded a trace id link to `/traces/{id}`. |
| `/usage` | BigQuery source only. `?range=` (`1h`, `6h`, `24h` default, `7d`): totals, then LLM calls, tokens and cost by model, by agent and by prompt version, and the 20 traces with the most tokens. |

## Evals

The Evals pages are read-only views of the JSON reports `gadk-eval monitor --out <url>` writes
(see the [`gadk-eval-monitor` README](../gadk-eval-monitor/README.md)); start the console with the
same URL as `--reports`. Nothing on these pages runs a monitor or changes the store.

## Security model

The console is for one developer on their own machine:

- It binds 127.0.0.1 only.
- Requests whose `Host` is not `127.0.0.1:<port>` or `localhost:<port>` get 421 (DNS-rebinding
  defence).
- Every POST needs an `Origin` of `http://127.0.0.1:<port>` or `http://localhost:<port>` and a
  per-process random CSRF token from the page, or it gets 403.
- With `--port 80`, the port-less `Host` (`127.0.0.1`, `localhost`) and `Origin`
  (`http://127.0.0.1`, `http://localhost`) that browsers send for the default port are accepted too.
- Every response carries
  `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; form-action 'self'`,
  `X-Content-Type-Options: nosniff` and `Cache-Control: no-store` (so pages holding trace
  payloads are not kept in the browser cache), and all trace and log text is HTML-escaped.
- There is no login. Anyone who can reach the port on your machine (other local users, or
  processes you run) can read your traces, logs and monitor reports, which may include prompts, completions and
  production user messages, and change prompt labels with your credentials. Don't run it on a shared host.

## BigQuery traces

`--traces bigquery://<project>.<dataset>.<table>` reads traces from the table that the
`gadk-agentanalytics` plugin writes (`agent_events`) instead of Cloud Trace and Cloud Logging:

    gadk-console --project my-billing-project \
      --traces bigquery://my-data-project.agent_analytics.agent_events

- Queries run in `--project` (billed there) and need `roles/bigquery.jobUser` there and
  `roles/bigquery.dataViewer` on the table. The project may be domain-scoped
  (`example.com:proj`).
- Every query stops at `--bigquery-max-bytes` bytes billed (default 1,073,741,824 = 1 GiB); a
  query over the cap fails with a hint to raise it. List queries read the chosen window plus the
  day before it (a 1h view scans about 25 hours of partitions); opening a trace reads that
  trace's rows across the whole table.
- Rows that share a `trace_id` are one trace and rows that share a `span_id` are one span. Span
  names: `invocation` (user message and invocation rows), `invoke_agent <agent>`,
  `call_llm <model>`, `execute_tool <tool>`, else the event type in lower case. A span starts at
  its first row and ends at its last row, or later when `latency_ms.total_ms` says so.
- Each row is also a log entry of its span (`event.name` = event type, `body` = `content`,
  `attributes` = `attributes` plus `error_message`); rows with an `ERROR` status or an `_ERROR`
  event type are severity `ERROR`. Rows without a span id are listed under "Unattached".
- A trace shows at most 1,000 rows. Paging uses offsets, so rows written while you page can
  shift the list.

## Usage and cost

`--prices <file>` reads model prices, in USD per 1,000,000 tokens, from a YAML list. There are no
built-in prices.

    - model: gemini-3.8-flash      # an exact model name
      input: 0.30
      output: 2.50
      cached_input: 0.075          # optional; defaults to input
    - model: gemini-3.8-*          # a trailing * matches by prefix
      input: 1.25
      output: 10.00
      reasoning: 10.00             # optional; defaults to output

An exact name beats a prefix, and a longer prefix beats a shorter one. A bad file (unreadable,
not a list, an unknown key, an empty or repeated model, a negative price) stops startup with
`price file <path>: <detail>`.

Each model call is four token counts:

| Count | Cloud Trace span label | BigQuery `LLM_RESPONSE` row |
|---|---|---|
| input | `gen_ai.usage.input_tokens` | `content.usage.prompt` + `attributes.usage_metadata.tool_use_prompt_token_count` |
| cached | `gen_ai.usage.cache_read.input_tokens` | `attributes.usage_metadata.cached_content_token_count` |
| output | `gen_ai.usage.output_tokens` minus reasoning | `content.usage.completion` |
| reasoning | `gen_ai.usage.reasoning.output_tokens` | `attributes.usage_metadata.thoughts_token_count` |

Missing values count as 0. The model is `gen_ai.request.model` (Cloud Trace), or the
`attributes.model` of the span's `LLM_REQUEST` row, else the response's
`attributes.model_version`, else `(unknown)` (BigQuery). Cost is
`(input − cached) × input + cached × cached_input + output × output + reasoning × reasoning`,
prices per 1,000,000 tokens. A model without a price has no cost (`—`); a total marked `*`
leaves out calls to models without a price.

- `/traces/{id}` lists every model call with its tokens and cost, plus totals. Compaction spans
  also carry usage labels but are not counted. With the BigQuery source the LLM spans get the
  same labels from the rows the page already reads.
- `/usage` and the production metrics on `/prompts/{name}` need `--traces bigquery://…`; with
  the Cloud Trace source they show a note and run no query. They read the `LLM_RESPONSE` rows
  (and their spans' `LLM_REQUEST` rows) whose `timestamp` is in the range, each query under
  `--bigquery-max-bytes`. Calls group by prompt version through `attributes.prompt`, which the
  `gadk-agentanalytics` plugin writes when the agent's instruction comes from `gadk-prompts`;
  calls without it, including rows written before it existed, are `(none)`.
- The eval pass rate per version reads the newest 50 monitor reports, keeps those whose window
  overlaps the range, takes their runs that started in the range (at most 5,000 invocations,
  newest first; the page notes when capped), and maps each invocation to the prompt versions its
  `LLM_REQUEST` rows recorded (rows from one day before to one hour after the range). A run that
  used several versions counts for each.

## Limits

- Trace lists: 50 traces per page (a "Next page" link follows the API's page token).
- Log entries: at most 1,000 per trace; the page says when the list is truncated.
- Log bodies and attributes longer than 4,096 characters are collapsed, with an "expand"
  control.
- The report cache keeps up to 256 full reports (transcripts included) in memory, so memory use
  grows with transcript size.

## Live test

`tests/console_live.rs` is an ignored round trip against a real project: it exports a
`render_prompt` span and a log record through `with_otel_to_cloud(true)`, then reads them back
through the console's `CloudTraceSource` (waiting up to 180 s for both to become readable).

```sh
GADK_CONSOLE_PROJECT=<project> cargo test -p gadk-console --test console_live -- --ignored
```

It needs ADC that may write traces and logs (`roles/cloudtrace.agent`,
`roles/logging.logWriter`) as well as read them.
