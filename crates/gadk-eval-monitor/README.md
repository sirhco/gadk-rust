# gadk-eval-monitor

Production eval monitors for gadk: grade real agent runs with
[`gadk-eval`](../gadk-eval/README.md)'s scorers. A monitor reads recent invocations from the
BigQuery agent-analytics table (written by `gadk-agentanalytics`) or from the agent's spans in
Cloud Trace ([Cloud Trace source](#cloud-trace-source)), scores each one against a YAML
monitor spec, writes a JSON report to a local directory or `gs://`, and fails below a pass-rate
threshold, so it runs from CI or a scheduler.

The command-line tool is the `gadk-eval` binary of this crate (the `gadk-eval` crate is the
library it builds on):

```sh
cargo install gadk-eval-monitor

gadk-eval monitor monitors/support.yaml --out gs://my-bucket/monitors
gadk-eval report gs://my-bucket/monitors
gadk-eval report gs://my-bucket/monitors support-bot-prod-20261117T080000Z.json
```

## Monitor spec

```yaml
name: support-bot-prod            # [A-Za-z0-9_-]{1,64}; names the reports
source:                           # exactly one of:
  bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
  # cloud_trace: { project: my-proj }   (see "Cloud Trace source")
window: 24h                       # <n>m | <n>h | <n>d: runs started in [now - window, now)
agent: support_bot                # optional: only runs whose root agent is this
limit: 100                        # 1-1000, default 100: the newest runs in the window
min_pass_rate: 0.9                # optional, 0.0-1.0: below it the command exits 1
judge:                            # required if and only if a scorer is llm_judge
  model: gemini-2.5-flash         # Gemini on Vertex AI
  project: my-proj                # default: the source project
  location: us-central1           # default us-central1
  timeout: 120s                   # <n>s | <n>m, default 120s
scorers:                          # gadk-eval scorer syntax
  - not_contains: "I cannot help"
  - regex: { pattern: "(?i)order \\d+", turn: 1 }
  - tools: { calls: [lookup_order], ordered: true }
  - llm_judge: { rubric: "Polite, accurate, no invented policy", min_score: 4 }
```

Unknown fields and invalid values are rejected with the field named. `reference` and `equals`
need a reference answer, which production runs do not have, so they are rejected. `turn` is not
bounded when the spec loads; a run with fewer turns gets an error result for that scorer
(`run has no turn N`).

A run passes when all of its scores pass. The pass rate is passing runs divided by runs; with no
runs it is undefined, which fails a `min_pass_rate`.

## How runs are rebuilt

The analytics table logs a summary of each event's content, not the content itself. For each
invocation that started in the window (from its `INVOCATION_STARTING` rows; a resumed
invocation is listed once), the monitor reads its `USER_MESSAGE_RECEIVED`, `AGENT_RESPONSE`,
`TOOL_STARTING`, `AGENT_ERROR` and `INVOCATION_ERROR` rows:

- each `USER_MESSAGE_RECEIVED` starts a turn; its `content.text_summary` is the user message. A
  message holding only `Function response: <name>` pieces (a resumed invocation) continues the
  current turn;
- a turn's reply is the text of its `AGENT_RESPONSE` rows, in order, joined with newlines.
  `content.response` joins one piece per part with `" | "` (`text: '<text>'`, `call: <name>`,
  `resp: <name>`, `Executable code (…)`, `Code execution result (…)`, `other`); only text
  pieces are kept. The summary is split on `" | "` only where a piece starts (one of those
  prefixes, or exactly `other`), so **a reply text that itself contains `" | text: '"`,
  `" | call: "`, `" | resp: "`, `" | Executable code ("` or `" | Code execution result ("` is
  split there** (a known limitation);
- each `TOOL_STARTING` adds `content.tool` with `content.args` to the turn's tool calls;
- the first `AGENT_ERROR` / `INVOCATION_ERROR` `error_traceback` is the run error: every scorer
  of that run is an error and the run does not pass.

Rows with equal timestamps (parallel tool calls) are ordered by their random `event_id`:
deterministic, not causal (tables created before the `event_id` column was added leave such ties
in no particular order). Rows are read from 10 minutes before the window start (the user
message is logged just before the invocation starts). A resumed invocation is listed by its
first start in the window; if it started before the window and resumed inside it, it is listed
at the resume, and its rows from more than 10 minutes before the window start are missing (a
known limitation; such runs usually show up as incomplete).

A run is marked **content incomplete**, with a note per problem, when a row's content is NULL,
has another shape (a custom `content_formatter`) or was truncated (`is_truncated`); when content
was offloaded (`content_parts` not `INLINE`, or with a `uri` or `object_ref`); when there is no
user message; when a turn has no response and the run has no error; or when rows come before the
first user message (they are ignored). For an incomplete run, `contains`, `not_contains`, `regex`
and `llm_judge` report an error (`content incomplete: <first note>`) instead of grading a partial
transcript. `tools` scorers still run unless a note is about a `TOOL_STARTING` row (its content
was NULL, another shape, truncated or offloaded, or it came before the first user message): its
call could not be rebuilt, so `tools` scorers report an error (`tool calls incomplete: <note>`)
instead of grading a partial call list.

**Denied payload columns.** The analytics plugin's `payload_column_denylist` leaves `content`,
`content_parts` or `attributes` out of the table altogether. The monitor reads the table schema
first and selects a missing column as NULL: without `content` every run is incomplete and every
scorer reports an error (a run's `tools` scorers grade only when it logged no tool calls);
without `content_parts` offloading cannot be detected; without
`attributes` the run's agent is the `agent` column (the emitting agent) and a spec with `agent`
fails, since the root-agent filter needs `attributes`. Runs are listed from their
`INVOCATION_STARTING` rows, so if the plugin's event denylist (or allowlist) drops that event
type, no runs are listed.

**Redaction and truncation.** Text over the plugin's `max_content_len` is cut and sets
`is_truncated`, so those runs are incomplete. With sensitive-key redaction on, free text (user
messages, response text, error tracebacks) that loses content to redaction (embedded JSON with
sensitive keys, sensitive URIs or credential-like patterns replaced by placeholders such as
`[REDACTED_SENSITIVE_TEXT]`) also sets `is_truncated`. Redacting the values of sensitive keys in
structured payloads (for example a tool's `args`, which then read `[REDACTED]`) does **not** set
`is_truncated`: those runs are graded on the redacted values, so `tools` scorers matching such
`args` and `llm_judge` rubrics should tolerate redaction markers. Heavy truncation, redaction of
free text or offloading lowers the number of gradable runs.

## Cloud Trace source

Agents that export their spans to Cloud Trace (with `gadk-telemetry`) but do not log to an
analytics table can be monitored from their traces:

```yaml
source:
  cloud_trace: { project: my-proj }   # [A-Za-z0-9_-]+: the project the traces are in
```

The agent must record message content on its spans: run it with
`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY` (or `SPAN_AND_EVENT`). With
`EVENT_ONLY`, another true value or no value, the content goes to log records or nowhere, and
every run is content incomplete.

The monitor lists the traces in the window that have an `invoke_agent` span (with `agent`, a span
whose `gen_ai.agent.name` is that agent), newest first, 100 a page, and rebuilds each trace's
runs:

- one run per distinct `gcp.vertex.agent.invocation_id` on `invoke_agent` spans. The
  invocation's top-level spans are its `invoke_agent` spans with no `invoke_agent` ancestor of
  the same invocation (several when a workflow agent runs sub-agents side by side under its
  `invoke_workflow` span); the earliest is the run's root and gives the run's agent
  (`gen_ai.agent.name`), session (`gen_ai.conversation.id`) and start time. The run's spans are
  every top-level span and its descendants, stopping at an `invoke_agent` span of another
  invocation (which is a run of its own). Runs whose root started
  outside the window, or whose root agent is not `agent`, are dropped;
- one turn per run: `user` is the text of the last message of the run's first
  `generate_content` span's `gen_ai.input.messages` when that message is the user's (earlier
  messages are history); `reply` is the text parts (not `reasoning`) of every
  `gen_ai.output.messages` of the run's `generate_content` spans, in start order, joined with
  newlines;
- each `execute_tool` span is a tool call, in start order (ties by span id), except the
  `execute_tool (merged)` span that groups parallel calls. The name is `gen_ai.tool.name`; the
  arguments are `gen_ai.tool.call.arguments`, or `gcp.vertex.agent.tool_call_args` in the legacy
  schema (JSON `null` reads as `{}`). With content capture on, the default schema leaves empty
  arguments off the span, so a call without an arguments attribute has `{}`;
- the run error is `error.type: <value>` from the root's `error.type`, else from the earliest
  sub-agent span that has one.

A run is marked **content incomplete**, with a note per problem, when it has no
`generate_content` span; when the first one has no `gen_ai.input.messages`; when a
`generate_content` span without `error.type` has no `gen_ai.output.messages`; when a message
attribute is not JSON or not a message list; when the last input message is not the user's (a
resumed invocation, continuing after a tool confirmation); when the user text is empty; or when
the reply is empty and the run has no error. Its **tool calls are incomplete** when a call span
has no `gen_ai.tool.name`, its arguments are not a JSON object, or it has no arguments attribute
in a run that shows no content capture. Scorers treat incomplete runs as described under [How
runs are rebuilt](#how-runs-are-rebuilt).

Limits:

- Cloud Trace v1 returns span labels, not span status, so a run error is only `error.type` (no
  message), and the legacy telemetry schema records no `error.type`: its failed runs have no
  error, and a failed model call there marks the run content incomplete.
- A message attribute whose JSON is over 60 KiB is left off the span by the telemetry, so long
  conversations show up as incomplete.
- Parallel tool calls start together, so their order may differ from the order the model chose.
- A listing reads at most 50 pages (5,000 traces) and stops early once `limit` runs are found.
  `COMPLETE` pages carry every span, so they can be large.
- Access: Cloud Trace User (`roles/cloudtrace.user`) on the project.

## Command line and exit codes

| Command | Does |
|---|---|
| `gadk-eval monitor <spec.yaml> [--out <url>] [--limit N]` | Runs the monitor, prints its summary and, with `--out`, stores the report. `--limit` (1-1000) overrides the spec. |
| `gadk-eval report <url> [<name>] [--limit N]` | Lists the newest stored reports (name, spec, time, runs, pass rate, runs with errors; default 20), or prints one report's summary. |

`<url>` is a directory, `file://<dir>` or `gs://<bucket>[/<prefix>]`; other schemes (such as
`s3://`) are rejected. Reports are named
`<spec>-<YYYYMMDDTHHMMSSZ>.json`, with `-2`, `-3`, … added when the name is taken.

| Exit | `monitor` | `report` |
|---|---|---|
| 0 | success | success |
| 1 | `min_pass_rate` is set and either the pass rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0) | — |
| 2 | the spec cannot be loaded, the runs cannot be listed, the judge cannot be built, the report cannot be stored, or a usage error | a store error, an unknown or invalid report name, or a usage error |

A judge call that fails or times out is an error result for that scorer, not exit 2.

## Library

```rust
// Inside an async fn returning Result<_, gadk_eval_monitor::MonitorError>.
use gadk_eval_monitor::{BigQueryTranscriptSource, Monitor, MonitorSpec, ReportStore, vertex_judge};

let spec = MonitorSpec::load("monitors/support.yaml")?;
let source = BigQueryTranscriptSource::new("my-proj", "agent_analytics", "agent_events")?;
let judge = match &spec.judge {
    Some(j) => Some(vertex_judge(j).await?),
    None => None,
};
let report = Monitor::new().run(&spec, &source, judge).await?;
print!("{}", report.summary());
ReportStore::open("gs://my-bucket/monitors")?.put(&report).await?;
```

Implement `TranscriptSource` to grade runs from elsewhere; `gadk_eval::score_transcript` grades
a single `RunTranscript` with the same scorers.

## Access (IAM)

Google Cloud access uses Application Default Credentials; specs and reports hold no
credentials. Reports do hold production content (user messages, replies, tool call arguments and
error tracebacks), so restrict who can read the `gs://` bucket or local directory they are
written to as you would the analytics table.

- BigQuery Data Viewer on the analytics dataset (it also allows reading the table schema) and
  BigQuery Job User on the project that runs the queries (the source project).
- Cloud Trace User on the trace project (only with a `cloud_trace` source).
- Vertex AI User on the judge project (only with `llm_judge` scorers).
- Storage Object Admin on the bucket for `gs://` output (`report` needs only Storage Object
  Viewer).

## Cost

Each monitor run reads the table schema (free) and issues two BigQuery queries: the invocations
that started in the window, then the rows of at most `limit` of them. Both filter on `timestamp`, so a table partitioned by
`timestamp` (the analytics plugin's default) scans only the window's partitions; keep the window
short and the event types filtered. Judge cost grows with `limit` times the number of
`llm_judge` scorers; runs are graded 8 at a time.

## Live test

```sh
GADK_EVAL_MONITOR_LIVE_PROJECT=<project> GADK_EVAL_MONITOR_LIVE_DATASET=<dataset> \
  cargo test -p gadk-eval-monitor --test bigquery_live -- --ignored --nocapture
```

The Cloud Trace source has its own read-only live test:

```sh
GADK_EVAL_MONITOR_LIVE_TRACE_PROJECT=<project> \
  cargo test -p gadk-eval-monitor --test cloudtrace_live -- --ignored --nocapture
```
