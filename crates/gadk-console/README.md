# gadk-console

`gadk-console`, a local web console for gadk agents. It shows the traces your agents exported to
Cloud Trace, with each span's Cloud Logging entries attached, and it browses the `gadk-prompts`
registry: versions, diffs and labels. With `--reports`, it also shows the reports of
`gadk-eval-monitor` monitors, compares the variants of its experiments and shows the decisions of
its rollout guards. With a price file (`--prices`) it prices token counts. With the BigQuery trace
source it also shows usage and cost by model, agent, prompt version and trace, lists and adds
scores, walks a review queue of the traces not yet scored, browses sessions and users (each
session's turns as a conversation, with tokens, cost, errors and thumbs), serves the same data as
a read-only JSON API under `/api/v1`, and, with `--datasets`, adds a trace's runs to an eval set.
It runs on your machine, listens on 127.0.0.1 only, and reads
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
| `--store <url>` | none | Prompt store: `file://<dir>` or `firestore://<project>/<database>[/<root>]`. Without it the prompt pages say "no store configured" and the label and rollout routes return 404. |
| `--reports <url>` | none | Monitor report store: `file://<dir>`, a directory, or `gs://<bucket>[/<prefix>]` (the `--out` of `gadk-eval monitor`, or the `<url>` of `gadk-eval report`). Experiments are read from its `experiments/` and guard reports from its `guards/` (the `--out` of `gadk-eval guard`). Without it the Evals, Experiments and Guards pages answer 404 "no report store configured". |
| `--port <n>` | `8765` | Port on 127.0.0.1; `0` picks a free one (the printed URL shows it) |
| `--log-id <id>` | `gadk` | Cloud Logging log id the agents write (`with_cloud_log_name` in `gadk-telemetry`) |
| `--prices <file>` | none | Price file (YAML, USD per 1,000,000 tokens per model). Without it, cost cells show `—`; token counts still show. See [Usage and cost](#usage-and-cost). |
| `--author <name>` | `$USER`, else `$USERNAME`, else `console` | Author of the scores you add on `/traces/{id}` (BigQuery source). See [Scores](#scores). |
| `--datasets <dir>` | none | Directory of eval-set YAML files that `/traces/{id}` adds runs to (BigQuery trace source only; ignored with Cloud Trace). It must exist. See [Datasets](#datasets). |
| `--rubrics <file>` | none | YAML file of review rubrics: named sets of criteria, each scored on a small integer scale (1-5 by default), on `/traces/{id}` and in `/review` queues (BigQuery trace source only; with Cloud Trace the file is still checked at startup but unused). See [Rubrics](#rubrics). |
| `--api-origin <origin>` | none | Origin of a browser app allowed to read the JSON API, exactly `scheme://host[:port]` as a browser sends it (`http://localhost:5173`); repeat it for more origins. Without it the API sends no CORS headers. See [JSON API](#json-api). |

Exit codes: 2 for a usage error (clap's message), 1 when startup fails (a bad store URL, a bad
report store URL, a bad price file, a bad rubric file, a `--datasets` directory that does not
exist, an `--api-origin` that is not an exact origin, missing credentials for a `gs://` report store or for `--datasets`, or a port already in
use), with a one-line `gadk-console: …` message.

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
| `/prompts/{name}` | All versions (newest first) with their labels and rollouts (a `production 10%` chip on the rollout version); `?v=<n>` shows a version's template and config, `?a=<n>&b=<m>` diffs two versions. Forms set, move and remove labels, conditional on the label not having changed since the page was rendered (a stale page gets 409). The Labels table's Rollout column shows each label's rollout (`v4 at 10%`, or `—`); a label without one has a Start rollout form (version and percent), a label with one has forms to change the percent, promote (move the label to the rollout version, which ends the rollout, in one atomic write; 409 if the label or its rollout changed since the page was rendered) and abort. "Production metrics" (BigQuery source, `?range=`, default `7d`): calls, mean tokens and cost, p50/p95 latency and, with `--reports`, eval pass rate per version, then a Feedback table of the scores per version. With `--reports`, a "guard history" link under the Labels table opens `/guards?prompt=<name>`. |
| `/evals` | The newest 100 monitor reports: a card per monitor (latest pass rate, errors, and a pass-rate trend over its listed reports), then a table of reports. `?spec=<name>` shows only that monitor's newest 100. Reports are read once and kept in memory (up to 256, and at most 20,000 runs in all), so the first load of a `gs://` store can take a while. |
| `/evals/{report}` | One report: window, source, runs, pass rate and errors, then one row per run with a chip per scorer. Expanding a run shows each score's outcome, judge score and rationale, the run error, notes on incomplete transcripts, and the transcript with tool calls; runs that recorded a trace id link to `/traces/{id}`. |
| `/experiments` | The newest 100 experiments in the `--reports` store (its `experiments/`), newest first: name (linking to the comparison), eval set, when it was generated, and each variant's name and pass rate. Experiments are read once and kept in memory (up to 256, and at most 20,000 runs in all). |
| `/experiments/{report}` | One experiment, with a column per variant (the first is the baseline): prompt, model (only when a variant records one), runs, pass rate and its difference from the baseline in percentage points, mean judge score, errors, p50 and p95 duration, input, output, cached and reasoning tokens, and cost. Then the cases where the variants disagree, each with every variant's verdict and, expanded, every run's last reply, run error and score rationales; `?cases=all` lists every case. See [Experiments](#experiments). |
| `/guards` | The newest 100 guard reports in the `--reports` store (its `guards/`): a card per rollout (prompt, label, baseline and rollout version, linking to the prompt) with its latest verdict, action and percent, how many reports it has and how many applied their action, and its timeline; then a table of the reports, newest first. `?prompt=<name>` shows only that prompt's rollouts. See [Guards](#guards). |
| `/guards/{report}` | One guard report: the rollout and its percent, window, its stats when the spec set `sequential` or `confidence` (for example `sequential, 95% across 2 interval checks`), source and when it was made, then one row per check (baseline and rollout values with their sample sizes, harm, the interval of the harm, and a status badge with its reason), the verdict, the note and the action, marked `applied` or `dry run`. |
| `/review` | BigQuery source only. A review queue: `?score=<name>` (required) with optional `prompt`, `version`, `flagged=1` and `range` (`1h`, `6h`, `24h`, `7d` default) lists the recent traces with no console score of that name, newest first, at most 100, with a "Start reviewing" link and a Judge column (the newest `gadk-eval monitor --write-scores` judge score per name). Without `score` it shows the form alone. See [Review](#review). |
| `/review/next` | BigQuery source only. `?<queue>&after=<time>&after_id=<trace id>` redirects (303) to the queue's next trace after that one, or back to `/review?<queue>&done=1` when none is left. |
| `/sessions` | BigQuery source only. The sessions with activity in the window, newest last activity first, at most 100 per page, with their user, agent, turns, tokens, cost, errors and thumbs. `?range=` (`1h`, `6h`, `24h` default, `7d`), `user`, `agent`. See [Sessions and users](#sessions-and-users). |
| `/sessions/{id}` | BigQuery source only. One session's turns as a conversation, oldest first, each with its metrics. `?range=` (default `7d`). |
| `/users` | BigQuery source only. The users with activity in the window, at most 100, with their sessions, turns, tokens, cost, errors and thumbs. `?range=` (default `7d`), `agent`. |
| `/usage` | BigQuery source only. `?range=` (`1h`, `6h`, `24h` default, `7d`, `30d`): totals, a chart of cost (or tokens) over time, then LLM calls, tokens, cost and a trend sparkline by model, by agent and by prompt version, and the 20 traces with the most tokens. See [Usage and cost](#usage-and-cost). |
| `/usage.csv`, `/sessions.csv`, `/users.csv` | BigQuery source only. CSV downloads of the `/usage` tables (`?table=model`, `agent`, `prompt` or `trend`) and of every session or user of the window, up to 5,000. See [CSV export](#csv-export). |
| `/api/v1/…` | BigQuery source only, except `/api/v1/openapi.json`. Read-only JSON of the usage, sessions, users, prompt-version, trace-score and review views, for custom dashboards. See [JSON API](#json-api). |

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
- The JSON API under `/api` sends no CORS headers unless you list origins with `--api-origin`. A
  listed origin, matched exactly, gets `Access-Control-Allow-Origin` with its own value (never
  `*`, never `Access-Control-Allow-Credentials`) on `GET` responses and preflights; every other
  origin's browser scripts still cannot read the responses. The `Host` check applies to the API
  too, and the API has no write routes. List only apps you trust: a listed origin's pages can
  read everything the API serves.
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

`/usage` also shows how usage changes across its range. Under `Over time`, a chart plots the
cost per bucket (without `--prices`, the input + output + reasoning tokens per bucket), labeled
with the largest and the latest bucket's value and the first and last buckets' start times in
UTC. With a price file, calls to models without a price count as 0, and when the range has any,
a note under the chart says so. The Totals, By model, By agent and By prompt tables end with a `Trend` column: a
sparkline of the same metric per bucket for each row, scaled to the row's own largest bucket
(the row's size is in its other columns). The buckets are aligned to the start of the range:

| Range | Bucket | Buckets |
|---|---|---|
| `1h` | 5 minutes | 12 |
| `6h` | 15 minutes | 24 |
| `24h` (default) | 1 hour | 24 |
| `7d` | 6 hours | 28 |
| `30d` | 1 day | 30 |

The trends come from one more query over the same rows, grouped by bucket as well as by model,
agent and prompt version, under the same `--bigquery-max-bytes` cap, and it returns at most
20,000 groups. Past that, the chart and the `Trend` cells give way to `Too many groups to chart
for this range.`, and the tables still show. `30d` is offered on `/usage` only; the other pages
keep their ranges.

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
  trimming; with `--rubrics`, a score name or a rubric, see [Rubrics](#rubrics)), optionally a
  prompt name and version (only traces whose `LLM_REQUEST` rows used that prompt, at that
  version), "Thumbs-down only" (only traces with an app thumbs-down score) and a range (`1h`,
  `6h`, `24h` or `7d`, the default). A trace is in the window when it has an
  `LLM_RESPONSE` row in it, and pending while no console score with exactly that name is on it
  (by `trace_id`, or by one of its invocations).
- The page lists at most 100 pending traces, newest first by their first LLM response: the time,
  the trace, its prompt versions (`name@version`) and its app thumbs-down count. With more it
  says `100+ pending (showing 100)`.
- The Judge column shows the judge scores that `gadk-eval monitor --write-scores` wrote on the
  trace (`FEEDBACK` rows with source `judge`), the newest per name, joined by ` · `: each is
  `<short name> <score> ✓` (pass) or `<short name> <score> ✗` (fail), with its full name
  (`<monitor spec>/<scorer kind>`) as a tooltip; `—` when there are none. Judge scores never take
  a trace out of the queue: only a console score of the queue's name does.
- "Start reviewing" (or a trace link) opens the trace in review mode. A bar above the trace shows
  `Reviewing <score>`, `Skip →` (the next trace) and `Back to queue`, and the score form's name is
  pre-filled. Adding a score opens the next trace; when none is left the queue page says
  `Queue empty — all caught up`.
- In review mode the Scores section shows a Judge section after the scores table and before the
  forms, so you can see why the judge passed or failed the run before scoring it: the newest
  judge score per name with its score, `pass` or `fail` and the judge's rationale. It is left out
  when the trace has no judge scores, and outside review mode. Judge rows past the newest 200
  scores the page loads are not shown.
- The queue is the URL: nothing is stored, nothing is locked. Two reviewers on the same queue may
  score the same trace; both scores are kept. A trace drops out of the queue once its first model
  response is older than the range.
- The pending query reads the window's rows and the `FEEDBACK` rows from a day before it, under
  `--bigquery-max-bytes`. The score name and filters are query parameters.

## Rubrics

A rubric scores a trace on several criteria in one submit. `--rubrics <file>` loads the rubrics
from a YAML file at startup; they need `--traces bigquery://…` like every score. This "rubric"
is unrelated to the optional judge rubric of the "Add to dataset" form (see
[Datasets](#datasets)), which is text for an LLM judge stored in an eval set.

```yaml
- name: support                      # [A-Za-z0-9_-]{1,64}, unique in the file
  description: Answers to support questions   # optional
  criteria:                          # 1-10
    - name: accuracy                 # [A-Za-z0-9_-]{1,64}, unique in the rubric
      description: Is the answer right?       # optional
    - name: tone
      scale: [0, 2]                  # optional [min, max], default [1, 5]
- name: safety
  criteria:
    - name: harmless
```

- The file is a list of 1-20 rubrics, each a map of exactly `name`, `description` (optional, a
  non-empty string) and `criteria`. A criterion is a map of exactly `name`, `description`
  (optional) and `scale` (optional): two integers with `0 <= min < max <= 100` and at most 11
  steps. `<rubric>/<criterion>` must fit in 100 bytes. Any other key or value stops startup
  with `rubric file <path>: <detail>`, for example
  `` rubric file rubrics.yaml: rubrics[1].criteria[2].scale must be [min, max] with 0 <= min < max <= 100 and at most 11 steps ``
  (indices count from 1).
- Each criterion is saved as an ordinary console `number` score named `<rubric>/<criterion>`,
  such as `support/accuracy` with the value `4.0`. The Scores list, the per-version Feedback
  table on `/prompts/{name}` and rollout guards read them like any other number score, and a
  guard can watch one criterion:

  ```yaml
  checks:
    - feedback: { name: support/accuracy, source: console, max_decrease: 0.1 }
  ```

- On `/traces/{id}` the Scores section ends with one collapsed "Score with rubric <name>" form
  per rubric, in file order. A form shows the rubric's description, then each criterion with its
  description and one radio button per value of its scale, all required, and one optional
  comment shared by the criteria. "Save scores" asks for confirmation; the console checks its
  token, the rubric and every criterion (the first missing or out-of-scale one is a 400), looks
  up the trace's identity like the score form, and inserts one `FEEDBACK` row per criterion with
  a single parameterized `INSERT` (all rows or none; billed at least 10 MiB, like a score).
  Without `--rubrics`, `POST /traces/{id}/rubric` answers 404.
- A rubric queue is `/review?rubric=<name>`: the form's Rubric select (with `--rubrics`, the
  score name is then optional; choose one or the other, not both). A trace stays pending until
  it has a console score for every criterion of the rubric, with any value, so a trace scored on
  some criteria only stays in the queue. The queue page is titled `Review: rubric <name>`, the
  review bar reads `Reviewing rubric <name>`, and the rubric's form shows above the score form
  (whose name is then left blank). Saving the rubric opens the next trace.
- Re-submitting a rubric adds new rows; summaries use the latest score per invocation, name and
  source.

## Sessions and users

The Sessions tab groups the agent-analytics rows by session and by user. It needs
`--traces bigquery://…`; with the Cloud Trace source `/sessions`, `/sessions/{id}` and `/users`
answer 404 with a note.

- A session's activity rows are its rows in the window other than `FEEDBACK` rows. Its user and root
  agent come from its earliest `INVOCATION_STARTING` row in the window (the user shows `(none)` when
  it has none, and the agent cell is left empty). Turns are its distinct invocations; errors are its
  `LLM_ERROR` and `TOOL_ERROR` rows and rows with status `ERROR`; tokens are input + output +
  reasoning of its `LLM_RESPONSE` rows, and the cost prices them per model (`—` without prices,
  `$x*` when some models have none). 👍 and 👎 count the latest boolean score per invocation, name and
  source. Every count covers the window only, so a session that started before it shows only its
  later turns.
- `/sessions` lists at most 100 sessions per page, newest last activity first, with a `Next →`
  link to the following ones. `range` is `1h`, `6h`, `24h` (the default) or `7d`; `user` and
  `agent` (at most 128 bytes each, trimmed) keep only the sessions of that user or root agent.
  Each session links to its page.
- `/sessions/{id}` (`range` defaults to `7d`) shows the session's user, agent, first and last
  activity and totals, a link to its traces, and then its turns, oldest first (at most 200): the
  user's message and the agent's reply as chat bubbles (a run error follows the reply), and a
  line with the start time, latency, tokens and cost, the tools called, errors, the latest
  scores (`name=value`) and a link to the trace. The text comes from the same transcripts as
  `gadk-eval export`: a turn with no transcript run (for example one whose run started before
  the window) shows `(no content logged)`, and a transcript that could not be fully rebuilt says
  `content incomplete` with the reasons.
  If the transcripts cannot be read, the page keeps the metrics and says why in a banner.
- `/users` (`range` defaults to `7d`, optional `agent`) lists at most 100 users, most recent first,
  with their sessions, turns, tokens, cost, errors, thumbs and last activity; each links to its
  sessions. Sessions without a user count as `(none)`.
- The queries read only the window (at most 7 days) under `--bigquery-max-bytes`, with every
  filter and id as a query parameter. With the BigQuery source the console builds the transcript
  reader at startup, which loads Application Default Credentials; without `--datasets` a failure
  there does not stop the console, and the session page shows it.

## CSV export

`/usage`, `/sessions` and `/users` offer their tables as CSV files for spreadsheets and scripts.
With the BigQuery source each table has a "Download CSV" link that carries the page's range and
filters: on `/usage` under the `Over time`, `By model`, `By agent` and `By prompt` headings, and on
`/sessions` and `/users` above the table. A page without rows has no link. The downloads are
read-only `GET` routes that run the pages' own queries, under `--bigquery-max-bytes`:

- `/usage.csv?range=<range>&table=<table>`: `range` as on `/usage` (unknown or missing is `24h`).
  `table` is required and is `model`, `agent`, `prompt` or `trend`; anything else is a 400 (`table:
  must be model, agent, prompt or trend`).
  - `model`, `agent` and `prompt` read only the breakdown query and sum it exactly as the page's
    tables do, in the same order and with the same labels (an empty agent is `(unknown)`, a
    missing prompt `(none)`). Columns: `<table>,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial`.
  - `trend` reads only the trend query: one row per bucket, model, agent and prompt version, in
    query order. Columns: `bucket_start,model,agent,prompt,llm_calls,input,cached,output,reasoning,cost_usd,cost_partial`.
    `bucket_start` is the bucket's start time (see the bucket table above); an empty agent or a
    missing prompt is an empty cell.
- `/sessions.csv?range=&user=&agent=`: the parameters, defaults and 400s of `/sessions`. `after`
  and `after_id` are not read, so the file always starts at the newest session. Columns:
  `session_id,user,agent,first_activity,last_activity,turns,tokens,cost_usd,cost_partial,errors,thumbs_up,thumbs_down`,
  newest last activity first; `user` and `agent` are empty when the session has none.
- `/users.csv?range=&agent=`: the parameters, defaults (`7d`) and 400s of `/users`. Columns:
  `user,sessions,turns,errors,thumbs_up,thumbs_down,last_activity,tokens,cost_usd,cost_partial`, in
  the page's order. `user` is the raw id, empty for sessions without one (the page shows `(none)`).

The files are RFC 4180: a header row, then data rows, every line ending in CRLF, UTF-8 without a
BOM. A field holding `,`, `"`, CR or LF is wrapped in `"`, with each `"` doubled. Counts are plain
decimal, times are UTC RFC 3339 to the millisecond (`2026-11-24T01:00:00.000Z`), and `tokens` is
input + output + reasoning, as on the pages.

**Cost columns.** `cost_usd` is the cost of the priced calls in USD to 6 decimals, and is empty
when no call had a price (always, without `--prices`). `cost_partial` is `true` when some calls had
a price and some did not (the page's `$x*`), and `false` otherwise, so a column of `cost_usd` still
sums in a spreadsheet. The usage, session and user rows use the same sums as the pages. A trend row
is one model's calls, so its `cost_partial` is always `false` and its `cost_usd` is empty when the
model has no price.

**Formula guard.** Text that comes from the data (models, agents, prompts, session ids and users)
and starts with `=`, `+`, `-`, `@`, TAB or CR gets a leading `'`, so spreadsheet tools show it as
text instead of running it as a formula. Such a cell differs from the stored id by that `'`.

**Caps and truncation.**

- `/sessions.csv` and `/users.csv` list at most 5,000 sessions or users, reading at most 50,000
  session (or user) × model rows. `/sessions` and `/users` keep their 100-row pages.
- `/usage.csv` keeps the page's caps: 5,000 breakdown groups and 20,000 trend groups.
- Every query reads one BigQuery reply page, and BigQuery caps one reply at about 10 MB, so a long
  window can stop well below 50,000 rows. The file is then marked truncated, and the last session
  or user listed may miss some of its tokens and cost. The pages behave the same way.

When a cap cuts the rows, the response carries `X-Gadk-Truncated: true` and the file name ends in
`-truncated.csv`; the body has no comment rows.

**Responses.** A download is a 200 with `Content-Type: text/csv; charset=utf-8` and
`Content-Disposition: attachment; filename="<name>"`, where the name is
`gadk-usage-<table>-<range>-<stamp>.csv`, `gadk-sessions-<range>-<stamp>.csv` or
`gadk-users-<range>-<stamp>.csv`, and the stamp is the window end in UTC (`20261124T0100Z`).
Without the BigQuery source a download is a 404 page with the page's note, a bad parameter is a
400 page, and a failed query is a 502 page with the source's hint.

## JSON API

With the BigQuery source, the console serves the data of its usage, sessions, users,
prompt-version, trace-score and review views as JSON, for custom dashboards and frontends. The
routes are read-only `GET`s under `/api/v1`. They run the pages' own queries with the pages'
parameters, defaults, caps and 400 texts, under `--bigquery-max-bytes`, and their shapes are the
[`gadk-analytics`](../gadk-analytics/README.md) result types (timestamps as RFC 3339). The OpenAPI
document at `/api/v1/openapi.json` is served without the BigQuery source too.

| Route | Parameters | `data` |
|---|---|---|
| `/api/v1/usage/breakdown` | `range` as on `/usage` (`1h`, `6h`, `24h`, `7d`, `30d`; unknown or missing is `24h`) | `{"cells": [...], "truncated"}`: calls and tokens per model, agent and prompt version, each cell priced |
| `/api/v1/usage/trend` | `range` as above | `{"start", "bucket_seconds", "cells": [...]}`: the window start, the bucket width and one priced cell per bucket, model, agent and prompt version (`bucket` counts buckets from `start`); each cell is `{"bucket", "cell": {"agent", "prompt", "calls"}, "cost_usd", "cost_partial"}` |
| `/api/v1/usage/top-traces` | `range` as above | the 20 traces with the most tokens, each with its calls per model, priced |
| `/api/v1/sessions` | `range` (default `24h`), `user`, `agent`, `after` and `after_id` (the cursor), as on `/sessions` | at most 100 sessions, newest last activity first, each priced |
| `/api/v1/sessions/{id}/turns` | `range` (default `7d`), as on `/sessions/{id}` | `{"session", "turns": [...]}`: the session's row and its first 200 turns, oldest first, each priced |
| `/api/v1/users` | `range` (default `7d`), `agent`, as on `/users` | at most 100 users, each priced |
| `/api/v1/prompts/{name}/versions` | `range` as on `/prompts/{name}` (`1h`, `6h`, `24h`, `7d`; unknown or missing is `7d`) | `{"metrics": {"stats", "models", "truncated"}, "feedback"}`: per-version calls, mean tokens and latency with their cost, the calls per version and model with theirs, and the scores per version |
| `/api/v1/traces/{id}/scores` | none | `{"rows", "truncated"}`: the trace's scores and its invocations', newest first, as in the trace page's Scores section |
| `/api/v1/review/pending` | `score` or `rubric` (one is required), `prompt`, `version`, `flagged=1`, `range`, as on `/review` | the queue's pending traces, newest first, at most 100, each with `judge`: `[{"name", "score", "pass"}]`, its newest judge score per name (empty when none) |
| `/api/v1/openapi.json` | none | not an envelope: the [OpenAPI](#openapi) document of these routes, also without the BigQuery source |

A name in a path is percent-encoded (`/api/v1/prompts/team%2Fgreet/versions`). The prompt route
reads only BigQuery, not the prompt store: an unknown name gives empty rows, and it has no eval
pass rate.

Every successful response is 200 with `Content-Type: application/json` and the envelope
`{"data": …, "truncated": <bool>, "next": <string or null>}`:

- `truncated` is true when the answer is cut: a query hit its row cap (for users, also more
  than 100 users), a session has more than 200 turns, a review queue has more than 100 traces,
  or the prompt metrics or feedback were truncated. More sessions than fit on one page is not
  truncation: it is `next`.
- `next` is set only on `/api/v1/sessions` when there are more sessions: the relative URL of the
  next page, with the same `range`, `user` and `agent` and the cursor of the last row. It is
  `null` everywhere else.

These rows always carry `cost_usd` and `cost_partial` next to their own fields:
usage cells, trend cells, top traces, sessions, the session row and its turns, users, and the
prompt `stats` and `models` rows. `cost_usd` is the sum of the priced calls in USD, unrounded,
and `null` when no call had a price (always, without `--prices`); `cost_partial` is true when some
calls had a price and some did not.

```sh
curl -s 'http://127.0.0.1:8765/api/v1/usage/breakdown?range=24h'
```

```json
{
  "data": {
    "cells": [
      {
        "agent": "triage",
        "prompt": "triage@2",
        "calls": {
          "model": "gemini-3.8-flash",
          "calls": 3,
          "usage": { "input": 3000, "cached": 1000, "output": 600, "reasoning": 0 }
        },
        "cost_usd": 0.002175,
        "cost_partial": false
      }
    ],
    "truncated": false
  },
  "truncated": false,
  "next": null
}
```

Errors are JSON too, `{"error": {"kind", "message", "hint"}}` with `Content-Type:
application/json`:

| Status | `kind` | When |
|---|---|---|
| 400 | `bad_request` | A bad parameter, with the page's text (`user is too long`, `invalid cursor`, `invalid session id`, `score and rubric: choose one`, `score: required`, …), or a request the router cannot read (`invalid request: …`). |
| 404 | `needs_bigquery` | The console is not using the BigQuery source; the message is the page's note. This comes before any other check. |
| 404 | `not_found` | An unknown or invalid trace id (`trace not found`), a session without activity in the range (`no activity for session <id> in the last <range>`), or an unknown `/api` path requested with `GET` or `HEAD` (`no such API route`). |
| 405 | `method_not_allowed` | A method other than `GET`, `HEAD` or `OPTIONS` (`the API is read-only`), with an `Allow: GET, HEAD, OPTIONS` header. A `POST` from another origin gets the console's HTML 403 first. |
| 502 | the source's kind (`bigquery`, `transport`, …) | A failed query; `message` and `hint` are the error banner's text and hint. |

A wrong `Host` still gets the HTML 421 of the [Security model](#security-model).

### OpenAPI

`GET /api/v1/openapi.json` answers 200 with an OpenAPI 3.1 description of the routes above:
their parameters, the envelope and `data` schema of each, and their errors. Its `info.version`
is the console's version. It answers with or without the BigQuery source, under the same `Host`
check, security headers and `--api-origin` CORS as the other routes.

```sh
curl -s http://127.0.0.1:8765/api/v1/openapi.json > openapi.json
```

For Rust, [`gadk-console-client`](../gadk-console-client/README.md) is a typed async client
generated from this document; the console's API tests check that it reads every response they
produce. Any OpenAPI 3.1 generator can make a typed client from it too. For example,
[openapi-typescript](https://openapi-ts.dev/) writes TypeScript types with
`npx openapi-typescript openapi.json -o api.d.ts`; the console neither ships nor needs one. Each
schema lists every field the console always writes as required and allows other fields, so a
field added later does not break a client.

### Browser apps on another origin

A browser app served from another origin (a Vite dev server on `http://localhost:5173`, say)
can read the API only when the console lists that origin:

    gadk-console --project my-billing-project \
      --traces bigquery://my-data-project.agent_analytics.agent_events \
      --api-origin http://localhost:5173

- `--api-origin` takes an exact origin as a browser sends it: lowercase `http://` or `https://`,
  a lowercase host name, IPv4 address or bracketed IPv6 address, and a port unless it is the
  scheme's default; no path, no trailing slash. Repeat the flag for more origins. Anything else
  stops startup with `--api-origin: invalid origin <origin> (expected scheme://host[:port])`.
- With at least one origin listed, every `/api` response carries `Vary: Origin`, and a request
  whose `Origin` equals a listed origin byte for byte also gets
  `Access-Control-Allow-Origin: <that origin>`.
- `OPTIONS` on an `/api` path answers 204. For a listed origin it adds
  `Access-Control-Allow-Origin`, `Access-Control-Allow-Methods: GET` and
  `Access-Control-Max-Age: 600`.
- The pages and the CSV downloads never send CORS headers, and the console still answers only
  on 127.0.0.1 with its own `Host`, so the app calls `http://127.0.0.1:<port>/api/v1/…` (or
  `localhost`) directly, without a proxy that rewrites `Host`.

The [console example dashboard](../../examples/console-dashboard/README.md) is such an app:
three static files, with no build step, that read the usage, sessions, users and review routes
from `http://localhost:5173`.

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

## Library access

The BigQuery queries behind `/usage`, `/sessions`, `/users`, the per-version prompt metrics,
the trace scores and `/review` are the [`gadk-analytics`](../gadk-analytics/README.md) crate.
Depend on it to run the same queries from your own code, with the same SQL, caps and byte cap,
and typed results that serialize to JSON. The console's pages and its [JSON API](#json-api) are
consumers of it.

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
