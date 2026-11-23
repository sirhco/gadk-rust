# gadk-console

`gadk-console`, a local web console for gadk agents. It shows the traces your agents exported to
Cloud Trace, with each span's Cloud Logging entries attached, and it browses the `gadk-prompts`
registry: versions, diffs and labels. With `--reports`, it also shows the reports of
`gadk-eval-monitor` monitors, compares the variants of its experiments and shows the decisions of
its rollout guards. With a price file (`--prices`) it prices token counts. With the BigQuery trace
source it also shows usage and cost by model, agent, prompt version and trace, lists and adds
scores, walks a review queue of the traces not yet scored, and, with `--datasets`, adds a trace's
runs to an eval set. It runs on your machine, listens on 127.0.0.1 only, and reads Google Cloud with
your Application Default Credentials.

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
| `--store <url>` | none | Prompt store: `file://<dir>` or `firestore://<project>/<database>[/<root>]`. Without it the prompt pages say "no store configured" and the label and rollout routes return 404. |
| `--reports <url>` | none | Monitor report store: `file://<dir>`, a directory, or `gs://<bucket>[/<prefix>]` (the `--out` of `gadk-eval monitor`, or the `<url>` of `gadk-eval report`). Experiments are read from its `experiments/` and guard reports from its `guards/` (the `--out` of `gadk-eval guard`). Without it the Evals, Experiments and Guards pages answer 404 "no report store configured". |
| `--port <n>` | `8765` | Port on 127.0.0.1; `0` picks a free one (the printed URL shows it) |
| `--log-id <id>` | `gadk` | Cloud Logging log id the agents write (`with_cloud_log_name` in `gadk-telemetry`) |
| `--prices <file>` | none | Price file (YAML, USD per 1,000,000 tokens per model). Without it, cost cells show `—`; token counts still show. See [Usage and cost](#usage-and-cost). |
| `--author <name>` | `$USER`, else `$USERNAME`, else `console` | Author of the scores you add on `/traces/{id}` (BigQuery source). See [Scores](#scores). |
| `--datasets <dir>` | none | Directory of eval-set YAML files that `/traces/{id}` adds runs to (BigQuery trace source only; ignored with Cloud Trace). It must exist. See [Datasets](#datasets). |

Exit codes: 2 for a usage error (clap's message), 1 when startup fails (a bad store URL, a bad
report store URL, a bad price file, a `--datasets` directory that does not exist, missing
credentials for a `gs://` report store or for `--datasets`, or a port already in use), with a
one-line `gadk-console: …` message.

## Prerequisites

- Agents export with `gadk-telemetry`'s `with_otel_to_cloud(true)`: spans go to Cloud Trace and
  log records to Cloud Logging under the log id `gadk`, each entry carrying its span id. See
  the [`gadk-telemetry` README](../gadk-telemetry/README.md).
- Application Default Credentials for an identity with, on the project:
  - `roles/cloudtrace.user` (read traces);
  - `roles/logging.viewer` (read log entries);
  - for a Firestore store, Firestore access on the store's project: `roles/datastore.viewer` to
    browse, `roles/datastore.user` to change labels and rollouts;
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
| `/traces/{id}` | The span waterfall (tree order, indented by depth). Selecting a span shows its labels and the log entries whose `spanId` matches it; entries matching no span are listed as "unattached". A `render_prompt …` span with `prompt.name` and `prompt.version` links to that prompt version. A trace that ended less than 2 minutes ago notes that logs may still be arriving. Model calls (spans whose `gen_ai.operation.name` is `generate_content`) are listed with their tokens and cost, plus totals. With the BigQuery source a Scores section lists the trace's scores and a form adds one ([Scores](#scores)). With `--datasets`, an "Add to dataset" form adds its runs to an eval set ([Datasets](#datasets)). Opened from a review queue, it shows a review bar and the score form continues to the queue's next trace ([Review](#review)). |
| `/prompts` | Every prompt in the store: latest version, labels and when the latest version was created. |
| `/prompts/{name}` | All versions (newest first) with their labels and rollouts (a `production 10%` chip on the rollout version); `?v=<n>` shows a version's template and config, `?a=<n>&b=<m>` diffs two versions. Forms set, move and remove labels, conditional on the label not having changed since the page was rendered (a stale page gets 409). The Labels table's Rollout column shows each label's rollout (`v4 at 10%`, or `—`); a label without one has a Start rollout form (version and percent), a label with one has forms to change the percent, promote (move the label to the rollout version, which ends the rollout; 409 if the label moved since the page was rendered) and abort. "Production metrics" (BigQuery source, `?range=`, default `7d`): calls, mean tokens and cost, p50/p95 latency and, with `--reports`, eval pass rate per version, then a Feedback table of the scores per version. With `--reports`, a "guard history" link under the Labels table opens `/guards?prompt=<name>`. |
| `/evals` | The newest 100 monitor reports: a card per monitor (latest pass rate, errors, and a pass-rate trend over its listed reports), then a table of reports. `?spec=<name>` shows only that monitor's newest 100. Reports are read once and kept in memory (up to 256, and at most 20,000 runs in all), so the first load of a `gs://` store can take a while. |
| `/evals/{report}` | One report: window, source, runs, pass rate and errors, then one row per run with a chip per scorer. Expanding a run shows each score's outcome, judge score and rationale, the run error, notes on incomplete transcripts, and the transcript with tool calls; runs that recorded a trace id link to `/traces/{id}`. |
| `/experiments` | The newest 100 experiments in the `--reports` store (its `experiments/`), newest first: name (linking to the comparison), eval set, when it was generated, and each variant's name and pass rate. Experiments are read once and kept in memory (up to 256, and at most 20,000 runs in all). |
| `/experiments/{report}` | One experiment, with a column per variant (the first is the baseline): prompt, model (only when a variant records one), runs, pass rate and its difference from the baseline in percentage points, mean judge score, errors, p50 and p95 duration, input, output, cached and reasoning tokens, and cost. Then the cases where the variants disagree, each with every variant's verdict and, expanded, every run's last reply, run error and score rationales; `?cases=all` lists every case. See [Experiments](#experiments). |
| `/guards` | The newest 100 guard reports in the `--reports` store (its `guards/`): a card per rollout (prompt, label, baseline and rollout version, linking to the prompt) with its latest verdict, action and percent, how many reports it has and how many applied their action, and its timeline; then a table of the reports, newest first. `?prompt=<name>` shows only that prompt's rollouts. See [Guards](#guards). |
| `/guards/{report}` | One guard report: the rollout and its percent, window, source and when it was made, then one row per check (baseline and rollout values with their sample sizes, harm, the interval of the harm, and a status badge with its reason), the verdict, the note and the action, marked `applied` or `dry run`. |
| `/review` | BigQuery source only. A review queue: `?score=<name>` (required) with optional `prompt`, `version`, `flagged=1` and `range` (`1h`, `6h`, `24h`, `7d` default) lists the recent traces with no console score of that name, newest first, at most 100, with a "Start reviewing" link. Without `score` it shows the form alone. See [Review](#review). |
| `/review/next` | BigQuery source only. `?<queue>&after=<time>&after_id=<trace id>` redirects (303) to the queue's next trace after that one, or back to `/review?<queue>&done=1` when none is left. |
| `/usage` | BigQuery source only. `?range=` (`1h`, `6h`, `24h` default, `7d`): totals, then LLM calls, tokens and cost by model, by agent and by prompt version, and the 20 traces with the most tokens. |

## Evals

The Evals pages are read-only views of the JSON reports `gadk-eval monitor --out <url>` writes
(see the [`gadk-eval-monitor` README](../gadk-eval-monitor/README.md)); start the console with the
same URL as `--reports`. Nothing on these pages runs a monitor or changes the store.

## Experiments

The Experiments pages are read-only views of the experiment reports that
`ReportStore::put_experiment` (and `gadk-eval experiment --out <url>`) writes under
`experiments/` in the `--reports` store (see
[Experiments](../gadk-eval-monitor/README.md#experiments) in the `gadk-eval-monitor` README).
Nothing on these pages runs an experiment or changes the store.

- The Prompt row shows each variant's prompt selector (`v2`, a label, `latest`) and, when any
  variant records a model (`Variant::with_model`, or every variant of `gadk-eval experiment`),
  a Model row follows with each variant's model; `—` marks a variant without one.
- A run passes when it has no run error and every score passes, errors when it has a run error
  or an `error` score, and fails otherwise. The pass rate is passed / (passed + failed) runs,
  `—` when there are none; it counts whole runs, unlike the per-score rate of the Evals pages.
  The first variant is the baseline: the other columns show their pass-rate difference from it
  in percentage points (`+5.0 pp`, `−12.5 pp`).
- A case's verdict for a variant is `error` when any of its runs errored, `pass` when all of
  them passed, else `fail`. A case is a disagreement when its verdicts differ across variants.
- Duration percentiles (nearest rank) cover the runs without a run error. Token totals add up
  every model call of every run.
- Cost needs `--prices` and uses the formula of [Usage and cost](#usage-and-cost) on each model
  of each run. The model is the response's `model_version`, which can carry a dated suffix
  (`gemini-3.8-flash-001`), so price models by prefix (`model: gemini-3.8-flash*`). A total
  marked `*` leaves out models without a price; without `--prices`, or when no model has a
  price, cost shows `—`.
- An invalid report name answers 400, an unknown one 404, and a stored file that does not parse
  502 (on its page and on `/experiments`).

## Guards

The Guards pages are read-only views of the reports `gadk-eval guard --out <url>` writes under
`guards/` (see [Guards](../gadk-eval-monitor/README.md#guards) in the `gadk-eval-monitor`
README); start the console with the same URL as `--reports`. Nothing on these pages runs a guard
or applies a decision.

- A rollout is the reports that share a prompt, label, baseline version and rollout version.
  Cards are ordered by each rollout's newest report. A rollout that was aborted and later
  restarted with the same versions shares one card, and its timeline shows both runs.
- The timeline starts at the oldest report's percent and adds one step per report that applied
  its action, oldest first: `50% → ramped 75% → promoted v2`, or `… → aborted`. Dry runs add no
  step.
- Values print as `gadk-eval guard` prints them: rates as percents with their harm in
  percentage points (`+0.20pt`), other values with a relative harm (`+12.0%`), and `—` where
  there is no value.
- `?prompt=` filters the newest 100 guard reports, so a prompt whose reports are all older shows
  "No guard runs yet".
- An invalid report name answers 400, an unknown one 404, and a stored file that does not parse
  502 (on its page and on `/guards`).
- Guard reports are read once and kept in memory (up to 256).

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
built-in prices. The same file works for `gadk-eval guard --prices` (crate `gadk-eval-monitor`,
whose parser the console uses), so one price file serves both; see the [eval-monitor
Guards](../gadk-eval-monitor/README.md#guards) section.

```yaml
- model: gemini-3.8-flash      # an exact model name
  input: 0.30
  output: 2.50
  cached_input: 0.075          # optional; defaults to input
- model: gemini-3.8-*          # a trailing * matches by prefix
  input: 1.25
  output: 10.00
  reasoning: 10.00             # optional; defaults to output
```

An exact name beats a prefix, and a longer prefix beats a shorter one. A bad file (unreadable,
not a list, an unknown key, an empty or repeated model, a negative price) stops startup with
`price file <path>: <detail>`.

Each model call is four token counts:

| Count | Cloud Trace span label | BigQuery `LLM_RESPONSE` row |
|---|---|---|
| input | `gen_ai.usage.input_tokens` | `attributes.usage_metadata.prompt_token_count` + `attributes.usage_metadata.tool_use_prompt_token_count` |
| cached | `gen_ai.usage.cache_read.input_tokens` | `attributes.usage_metadata.cached_content_token_count` |
| output | `gen_ai.usage.output_tokens` minus reasoning | `attributes.usage_metadata.candidates_token_count` |
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

## Scores

Scores are the `FEEDBACK` rows of the agent-analytics table: end-user feedback that apps record
with `gadk-agentanalytics` (`record_score`, source `app`) and annotations added here (source
`console`). They need `--traces bigquery://…`; with the Cloud Trace source the Scores section
shows a note and no form.

- `/traces/{id}` lists the trace's scores, newest first, at most 200: rows whose `trace_id` is
  the trace or whose `invocation_id` is one of its spans' invocations, written from a day before
  the trace started until now. The trace list and the trace itself leave `FEEDBACK` rows out.
- The form under the list adds a score: a name, a value (thumbs up or down, a number or a
  category) and an optional comment. The page asks for confirmation; the console checks its
  token, looks up the trace's first row with an invocation id and inserts one row for that
  invocation with a parameterized `INSERT`. The lookup reads only the last 30 days, so a trace
  older than that cannot be scored (404). The author is `--author`, else `$USER`, else
  `$USERNAME`, else `console`.
- Adding scores needs `roles/bigquery.dataEditor` on the table (and `roles/bigquery.jobUser` in
  `--project`). BigQuery bills every DML statement at least 10 MiB, so the insert runs with a
  byte cap of at least 10,485,760 bytes even when `--bigquery-max-bytes` is lower (about
  $0.00006 per score at on-demand prices).
- Scores are append-only: there is no editing or deleting. Summaries use the latest score per
  invocation, name and source.
- `/prompts/{name}` adds a Feedback table under the production metrics: per version, score
  name, source and type, the count and the share of thumbs up (`bool`), the mean (`number`) or
  the most common value (`category`; a tie goes to the lowest value). Scores count from the
  start of the range with no end, since they arrive after the calls, and map to versions
  through their invocations' `LLM_REQUEST` rows like the eval pass rate.

## Review

A review queue walks you through the recent traces that have no console score of a given name
yet. It needs `--traces bigquery://…`; with the Cloud Trace source `/review` and `/review/next`
answer 404 with a note.

- `/review` defines the queue with its form: a score name (required, 1-100 bytes after
  trimming), optionally a prompt name and version (only traces whose `LLM_REQUEST` rows used that
  prompt, at that version), "Thumbs-down only" (only traces with an app thumbs-down score) and a
  range (`1h`, `6h`, `24h` or `7d`, the default). A trace is in the window when it has an
  `LLM_RESPONSE` row in it, and pending while no console score with exactly that name is on it
  (by `trace_id`, or by one of its invocations).
- The page lists at most 100 pending traces, newest first by their first LLM response: the time,
  the trace, its prompt versions (`name@version`) and its app thumbs-down count. With more it
  says `100+ pending (showing 100)`.
- "Start reviewing" (or a trace link) opens the trace in review mode. A bar above the trace shows
  `Reviewing <score>`, `Skip →` (the next trace) and `Back to queue`, and the score form's name is
  pre-filled. Adding a score opens the next trace; when none is left the queue page says
  `Queue empty — all caught up`.
- The queue is the URL: nothing is stored, nothing is locked. Two reviewers on the same queue may
  score the same trace; both scores are kept. A trace drops out of the queue once its first model
  response is older than the range.
- The pending query reads the window's rows and the `FEEDBACK` rows from a day before it, under
  `--bigquery-max-bytes`. The score name and filters are query parameters.

## Datasets

With `--datasets <dir>` and `--traces bigquery://…`, `/traces/{id}` shows an "Add to dataset"
form below the scores: a dataset name (1-64 of `A-Z`, `a-z`, `0-9`, `_` and `-`; the sets
already in `<dir>` are suggested) and an optional judge rubric. The page asks for confirmation;
the console checks its token and the request's origin, reads the trace's invocations from the
last 30 days, their runs, session history and scores, and adds one case per run to
`<dir>/<dataset>.yaml`, as `gadk-eval export` does (see the gadk-eval-monitor README). A new
file gets the dataset as its name and the form's rubric as its `judge_rubric`; an existing one
is appended to with its text kept byte for byte, and its `judge_rubric` grades judge cases when
the form has no rubric. Runs already in the file are skipped. The trace page then says how many
cases were added.

- A bad dataset name, a file that cannot be appended to (checked before any query), or no case
  added (every reason is listed, such as `needs a rubric` or `already in <dataset>.yaml`) is a
  400; an unknown trace a 404; a BigQuery error a 502.
- The queries run in `--project` under `--bigquery-max-bytes`. The table's project must be
  `[A-Za-z0-9_-]+` (a domain-scoped project fails at startup).
- Adds from this console are serialized, and every write goes through a temporary file next to
  the target: a new file is linked into place (never overwriting one that appeared meanwhile),
  an append renames over the old file, keeping its permissions. The lock does not cover other writers: a concurrent
  `gadk-eval export --append` to the same file, or a hand edit, can be lost (the last write
  wins), so do not run both on one file at once.
- The files hold production text; nothing is uploaded.

## Limits

- Trace lists: 50 traces per page (a "Next page" link follows the API's page token).
- Log entries: at most 1,000 per trace; the page says when the list is truncated.
- Log bodies and attributes longer than 4,096 characters are collapsed, with an "expand"
  control.
- The report cache keeps up to 256 full reports (transcripts included) in memory and at most
  20,000 runs in all, evicting the oldest reports first; the newest report always stays, so one
  larger report still renders (it is read again after eviction). Memory use grows with
  transcript size.
- The experiment cache keeps up to 256 parsed experiments (every run's transcript included) in
  memory and at most 20,000 case runs over all variants, with the same eviction; split very
  large eval sets into several experiments.

## Live test

`tests/console_live.rs` is an ignored round trip against a real project: it exports a
`render_prompt` span and a log record through `with_otel_to_cloud(true)`, then reads them back
through the console's `CloudTraceSource` (waiting up to 180 s for both to become readable).

```sh
GADK_CONSOLE_PROJECT=<project> cargo test -p gadk-console --test console_live -- --ignored
```

It needs ADC that may write traces and logs (`roles/cloudtrace.agent`,
`roles/logging.logWriter`) as well as read them.
