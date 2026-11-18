# gadk-console

`gadk-console`, a local web console for gadk agents. It shows the traces your agents exported
to Cloud Trace, with each span's Cloud Logging entries attached, and it browses the
`gadk-prompts` registry: versions, diffs and labels. With `--reports`, it also shows the reports
of `gadk-eval-monitor` monitors. It runs on your machine, listens on 127.0.0.1 only, and reads
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
| `--reports <url>` | none | Monitor report store: `file://<dir>`, a directory, or `gs://<bucket>[/<prefix>]` (the `--store` of `gadk-eval monitor`). Without it the Evals pages answer 404 "no report store configured". |
| `--port <n>` | `8765` | Port on 127.0.0.1; `0` picks a free one (the printed URL shows it) |
| `--log-id <id>` | `gadk` | Cloud Logging log id the agents write (`with_cloud_log_name` in `gadk-telemetry`) |

Exit codes: 2 for a usage error (clap's message), 1 when startup fails (a bad store URL, a bad
report store URL, missing credentials for a `gs://` report store, or a port already in use), with
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

Credentials are built on the first GCP call, so the console starts without them. A `gs://` report
store is the exception: its credentials are looked up at startup. A missing role or disabled API
shows as an error banner with a hint ("needs roles/cloudtrace.user",
"needs roles/logging.viewer", "enable <service>").

## Pages

| Page | Shows |
|---|---|
| `/traces` | Traces, newest first, 50 per page. Filters: time range (`1h`, `6h`, `24h`, `7d`), root span name prefix, agent (`gen_ai.agent.name`), session (`gen_ai.conversation.id`). |
| `/traces/{id}` | The span waterfall (tree order, indented by depth). Selecting a span shows its labels and the log entries whose `spanId` matches it; entries matching no span are listed as "unattached". A `render_prompt …` span with `prompt.name` and `prompt.version` links to that prompt version. A trace that ended less than 2 minutes ago notes that logs may still be arriving. |
| `/prompts` | Every prompt in the store: latest version, labels and when the latest version was created. |
| `/prompts/{name}` | All versions (newest first) with their labels; `?v=<n>` shows a version's template and config, `?a=<n>&b=<m>` diffs two versions. Forms set, move and remove labels, conditional on the label not having changed since the page was rendered (a stale page gets 409). |
| `/evals` | The newest 100 monitor reports: a card per monitor (latest pass rate, errors, and a pass-rate trend over its listed reports), then a table of reports. `?spec=<name>` shows only that monitor's newest 100. Reports are read once and kept in memory (up to 256), so the first load of a `gs://` store can take a while. |
| `/evals/{report}` | One report: window, source, runs, pass rate and errors, then one row per run with a chip per scorer. Expanding a run shows each score's outcome, judge score and rationale, the run error, notes on incomplete transcripts, and the transcript with tool calls; runs that recorded a trace id link to `/traces/{id}`. |

## Evals

The Evals pages are read-only views of the JSON reports `gadk-eval monitor --store <url>` writes
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
  and all trace and log text is HTML-escaped.
- There is no login. Anyone who can reach the port on your machine (other local users, or
  processes you run) can read your traces, logs and monitor reports, which may include prompts, completions and
  production user messages, and change prompt labels with your credentials. Don't run it on a shared host.

## Limits

- Trace lists: 50 traces per page (a "Next page" link follows the API's page token).
- Log entries: at most 1,000 per trace; the page says when the list is truncated.
- Log bodies and attributes longer than 4,096 characters are collapsed, with an "expand"
  control.

## Live test

`tests/console_live.rs` is an ignored round trip against a real project: it exports a
`render_prompt` span and a log record through `with_otel_to_cloud(true)`, then reads them back
through the console's `CloudTraceSource` (waiting up to 180 s for both to become readable).

```sh
GADK_CONSOLE_PROJECT=<project> cargo test -p gadk-console --test console_live -- --ignored
```

It needs ADC that may write traces and logs (`roles/cloudtrace.agent`,
`roles/logging.logWriter`) as well as read them.
