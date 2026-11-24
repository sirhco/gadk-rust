# gadk-eval-monitor

Production eval monitors for gadk: grade real agent runs with
[`gadk-eval`](../gadk-eval/README.md)'s scorers. A monitor reads recent invocations from the
BigQuery agent-analytics table (written by `gadk-agentanalytics`) or from the agent's spans in
Cloud Trace ([Cloud Trace source](#cloud-trace-source)), scores each one against a YAML
monitor spec, writes a JSON report to a local directory or `gs://`, and fails below a pass-rate
threshold, so it runs from CI or a scheduler.

It also runs experiments: one eval set against several variants of an agent, stored next to the
It also runs experiments: one eval set against several variants of an agent, stored next to the
monitor reports and compared in `gadk-console` ([Experiments](#experiments)), and guards prompt
rollouts: it compares a rollout's two prompt versions on production signals and ramps, promotes
or aborts the rollout ([Guards](#guards)).

The command-line tool is the `gadk-eval` binary of this crate (the `gadk-eval` crate is the
library it builds on):

```sh
cargo install gadk-eval-monitor

gadk-eval monitor monitors/support.yaml --out gs://my-bucket/monitors
gadk-eval report gs://my-bucket/monitors
gadk-eval report gs://my-bucket/monitors support-bot-prod-20261117T080000Z.json
gadk-eval export --source bigquery://my-proj.agent_analytics.agent_events \
  --out evals/regressions.yaml --since 1d --score 'thumbs=true' --rubric "Answers correctly."
gadk-eval experiment experiments/answer-style.yaml --out gs://my-bucket/monitors
gadk-eval guard guards/support.yaml --prices prices.yaml --apply --out gs://my-bucket/monitors
gadk-eval guards gs://my-bucket/guards/ --prices gs://my-bucket/prices.yaml --apply \
  --out gs://my-bucket/monitors
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
  model: gemini-3.8-flash         # Gemini on Vertex AI
  project: my-proj                # default: the source project
  location: us                    # default us (the US multi-region)
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
- A resumed invocation (the same invocation id continuing in a new trace) is one run per trace
  here, where the BigQuery source gives one run.
- A listing reads at most 50 pages (5,000 traces) and stops early once `limit` runs are found.
  `COMPLETE` pages carry every span, so they can be large.
- Access: Cloud Trace User (`roles/cloudtrace.user`) on the project.

## Command line and exit codes

| Command | Does |
|---|---|
| `gadk-eval monitor <spec.yaml> [--out <url>] [--limit N]` | Runs the monitor, prints its summary and, with `--out`, stores the report. `--limit` (1-1000) overrides the spec. |
| `gadk-eval report <url> [<name>] [--limit N]` | Lists the newest stored reports (name, spec, time, runs, pass rate, runs with errors; default 20), or prints one report's summary. |
| `gadk-eval export --source bigquery://<project>.<dataset>.<table> --out <file> [options]` | Writes production runs as eval-set cases to a new file, or `--append`s them; see [Exporting runs as eval cases](#exporting-runs-as-eval-cases). |
| `gadk-eval experiment <spec.yaml> [--out <url>]` | Runs an experiment from an experiment spec (one Gemini agent on Vertex AI per variant), stores it under `experiments/` of `<url>` when `--out` is given, then prints its summary and, with `--out`, `stored report <name>`; see [Experiment spec](#experiment-spec). |
| `gadk-eval guard <spec.yaml> [--apply] [--prices <file>] [--out <url>] [--max-bytes <n>]` | Compares both sides of a prompt rollout, prints the report and, with `--out`, stores it under `guards/` of `<url>` and prints `stored report <name>`; with `--apply` it ramps, promotes or aborts the rollout. Exits 0 (advance), 1 (abort), 3 (wait), 4 (no rollout) or 2 (error); see [Guards](#guards). |
| `gadk-eval guards <source>... [--apply] [--prices <file\|gs://bucket/object>] [--out <url>] [--max-bytes <n>]` | Runs every guard spec in its sources (files, directories, `gs://` objects and prefixes) one at a time, each under a `== <name> ==` line, then prints a summary line; see [Running many guards](#running-many-guards). |

`<url>` is a directory, `file://<dir>` or `gs://<bucket>[/<prefix>]`; other schemes (such as
`s3://`) are rejected. Reports are named
`<spec>-<YYYYMMDDTHHMMSSZ>.json`, with `-2`, `-3`, … added when the name is taken.

| Exit | `monitor` | `report` | `export` | `experiment` | `guards` |
|---|---|---|---|---|---|
| 0 | success | success | at least one case was added | the experiment ran (whatever its case results) and, with `--out`, was stored | every guard reached an outcome (advance, abort, wait or no rollout) |
| 1 | `min_pass_rate` is set and either the pass rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0) | — | no case was added (nothing is written) | — | — |
| 2 | the spec cannot be loaded, the runs cannot be listed, the judge cannot be built, the report cannot be stored, or a usage error | a store error, an unknown or invalid report name, or a usage error | the file exists (without `--append`) or cannot be read, appended to or written, a query fails, or a usage error | the spec is invalid, the eval set cannot be read or is invalid, the prompt store or a variant's prompt version cannot be loaded, a model or the judge cannot be built, the experiment fails, the report cannot be stored, or a usage error | a guard failed, a check before the run failed (a source, a spec, the price file or `--out`), or a usage error |

A judge call that fails or times out is an error result for that scorer, not exit 2.

## Exporting runs as eval cases

`gadk-eval export` turns runs logged in the agent-analytics table into
[`gadk-eval`](../gadk-eval/README.md) eval-set cases, so production failures and good answers
become offline regression cases:

```sh
# The runs of one trace, into a new file.
gadk-eval export --source bigquery://my-proj.agent_analytics.agent_events \
  --out evals/regressions.yaml --trace 0123456789abcdef0123456789abcdef \
  --rubric "Answers the question correctly and politely."

# The last day's runs whose latest `rating` is at least 4, added to the same file.
gadk-eval export --source bigquery://my-proj.agent_analytics.agent_events \
  --out evals/regressions.yaml --append --since 1d --score 'rating>=4' --limit 20
```

| Option | Meaning |
|---|---|
| `--source bigquery://<project>.<dataset>.<table>` | The agent-analytics table: project `[A-Za-z0-9_-]+` (no domain-scoped project), dataset and table `[A-Za-z0-9_]+`. Queries run in that project. |
| `--out <file>` | The eval-set file. Without `--append` it is created and never overwritten: named `--name` (not blank; default: the file stem), with `judge_rubric` = `--rubric`. |
| `--append` | Add to the set at `--out`; runs whose case id is already in it are skipped. Not with `--name`. |
| `--trace <id>` | The runs of this trace (32 hex digits) that started in the last 30 days; repeatable. Not with `--since`, `--agent`, `--score` or `--limit`. |
| `--since <n>h\|<n>d` | Runs that started in this window, 1h to 90d (default `7d`). |
| `--agent <name>` | Only runs of this root agent. |
| `--score <filter>` | `name=value`, `name<n`, `name<=n`, `name>n` or `name>=n` on the latest score of that name from any source; repeatable, every filter must hold. `=` compares the value as text (`true`, `4`, `helpful`); the comparisons need a `number` score. |
| `--limit <n>` | The newest runs to read, 1-1000 (default 50). |
| `--rubric <text>` | The rubric (not blank) of `llm_judge` cases (default: the set's `judge_rubric`). |
| `--min-good <x>` | The lowest `number` score that counts as positive (default 4). |
| `--no-history` | A case starts with the run's own user message, without the session's earlier ones. |
| `--tool-args` | `tools` scorers keep each call's arguments (default: names only, since arguments often hold ids and times). |

Each run becomes a case with id `inv-<invocation id>` and no `state` (session state is not
logged):

- Turns: the user messages of up to 50 earlier runs of the same session that started at most 30
  days before it, oldest first, then the run's own. An earlier run whose content could not be
  rebuilt is left out and named in a comment.
- Scorers follow the latest score of each name across sources. A run with a positive score (a
  `bool` true, or a `number` of at least `--min-good`), no negative one (a `bool` false or a
  smaller `number`) and a non-empty reply gets `reference` (the production reply, `min_score` 3)
  and, when it called tools and every call was rebuilt, `tools` (`ordered: true`, `exact:
  false`). Every other run (negative, unscored, only `category` scores, or an empty reply) gets
  one `llm_judge` with the rubric, and is skipped with `needs a rubric` when there is none.
- Comments above the case name its trace (or `none`), session, invocation, start time, agent,
  prompt version (`name@version`, when logged), every latest score per name and source (value,
  source, author, comment) and the production reply (its first 2,000 characters).

A run whose own content is incomplete is skipped with its notes. The command prints `added
inv-<id> (reference|judge) trace <id|none>` or `skipped inv-<id>: <reason>` per run (before the
file is written, so they show even when the write fails), then `<n> cases added to <file>` after
the write. An append keeps the file's text byte for byte and needs `cases:` as its last
top-level key (see the gadk-eval README); the file is checked before any query. Every write goes
through a temporary file next to the target: a new file is linked into place and never
overwrites one, an append renames over the old file and keeps its permissions. Two writers
appending to one file at once (two exports, or an export and `gadk-console`'s "Add to dataset")
can lose one write: the last one wins.

The files hold production text (user messages, replies, and tool arguments with
`--tool-args`): keep them as private as the table. Nothing is uploaded. Cases are a starting
point to review and edit: a user message with several parts is its logged summary, and the reply
of a run that transferred to a sub-agent includes the sub-agent's text.

## Experiments

An experiment runs one [`gadk-eval`](../gadk-eval/README.md) eval set against two or more
variants of an agent (prompt versions, models, or anything your `build` closure changes) and
keeps the result for a side-by-side comparison on `gadk-console`'s Experiments pages.

```rust
// Inside an async fn returning Result<_, Box<dyn std::error::Error>>; `judge`, `registry` and
// `build_agent` are yours, as in the gadk-eval README.
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_eval_monitor::{ReportStore, run_experiment};
use gadk_prompts::{Label, Selector};

let set = EvalSet::load("evals/support.yaml")?;
let variants = [
    Variant::prompt("prod", Selector::Label(Label::production())),
    Variant::prompt("v8", Selector::Version(8)),
];
let report = run_experiment("support-v8", &Evaluator::new(judge), &set, &variants, |v| {
    build_agent(&registry, v.selector().cloned().unwrap_or(Selector::Latest))
})
.await?;
print!("{}", report.summary());
let stored = ReportStore::open("gs://my-bucket/monitors")?
    .put_experiment(&report)
    .await?;
println!("stored {stored}");
```

`run_experiment(name, &evaluator, &set, &variants, build)` calls `Evaluator::run_measured` and
returns an `ExperimentReport`: the name, the set's name, `generated_at` (when the runs
finished), each variant's name, prompt selector as text (`v8`, `production`, `latest`;
none for `Variant::new`) and model (`Variant::with_model`; none when unset), the `EvalReport`
and each run's `RunMetrics` (duration and tokens per model).
The name must be `[A-Za-z0-9_-]{1,64}`: an invalid name fails before `build` is called, and it
and empty or repeated variant names are a `MonitorError::Spec` whose message starts
`experiment: `. `summary()` prints one row per variant: runs, pass rate, mean judge score,
errors, p50 and p95 duration, and input and output tokens, with a `model` column after
`variant` when any variant has a model (`—` for one without); `variant_stats()` returns the
same numbers and `case_verdicts()` each case's verdict per variant. `to_json` and `from_json`
round trip a report.

To compare models, give each variant one and read it in `build`:

```rust
let variants = [
    Variant::prompt("flash", Selector::Version(8)).with_model("gemini-3.8-flash"),
    Variant::prompt("hydrogen", Selector::Version(8)).with_model("gemini-4-hydrogen"),
];
// In `build`: `v.model()` names the model to build the agent with.
```

A run passes when it has no run error and every score passes, errors when it has a run error or
an `error` score, and fails otherwise. The pass rate is passed / (passed + failed) runs (`-` when
there are none): it counts whole runs, unlike the per-score rate of `EvalReport::summary` and
the monitor reports. A case's verdict for a variant is `error` when any of its runs errored,
`pass` when all passed, else `fail`; the variants disagree on a case when its verdicts differ.
Duration percentiles (nearest rank) cover only runs without a run error. Mean judge score
averages the scores of non-error results that have one.

`ReportStore::put_experiment` stores the report under `experiments/` in the same store as the
monitor reports, as `<name>-<YYYYMMDDTHHMMSSZ>.json` (`-2`, `-3`, … when taken, never
overwriting; a local `experiments` directory is created when missing) and returns that stored
name. `experiment_names(limit)` lists stored names newest first and `find_experiment(name)`
reads one (`None` when absent). Monitor report listings (`names`, `list`, `gadk-eval report`)
never include experiments.

A report's `model` keys are written only when set, so a report without models has the same
shape as one written by 0.5.1. Reading tolerates missing fields: a stored experiment without
`variants` or `metrics`, or with run metrics missing token counts, loads with them empty or 0.

```text
gs://my-bucket/monitors/
  support-bot-prod-20261117T080000Z.json      a monitor report
  experiments/
    support-v8-20261121T101500Z.json          an experiment report
    support-v8-20261121T101500Z-2.json        the same experiment name, same second
```

`examples/experiment.rs` runs a two-variant experiment end to end: it creates versions 1 and 2
of the prompt `exp-assistant` in a file prompt store, runs a three-case set (`contains` and
`llm_judge` scorers) on `gemini-3.8-flash` (Vertex AI, location `us`) as agent and judge (each
variant records the model, so the summary and the console show it), stores the report as
`exp-e2e-<timestamp>.json`, and prints its summary and stored name:

```sh
GOOGLE_CLOUD_PROJECT=my-proj GADK_EXPERIMENT_PROMPTS=/tmp/exp-prompts \
  GADK_EXPERIMENT_OUT=/tmp/exp-reports cargo run -p gadk-eval-monitor --example experiment
gadk-console --project my-proj --reports /tmp/exp-reports --prices prices.yaml
# http://127.0.0.1:8765/experiments
```

An experiment calls the agent's model for every turn of `cases × runs_per_case × variants`
runs, plus one judge call per `reference` and `llm_judge` scorer per run. Each report holds every
transcript, so split very large sets into several experiments.

### Experiment spec

`gadk-eval experiment <spec.yaml> [--out <url>]` runs an experiment without Rust code: one
Gemini LLM agent (no tools or sub-agents) on Vertex AI, whose model and prompt version change
per variant. For tools, sub-agents or other models, call `run_experiment` from Rust.

```yaml
name: answer-style                 # [A-Za-z0-9_-]{1,64}; names the stored report
set: sets/smoke.yaml               # eval-set file, relative to this spec file's directory
project: my-proj                   # Vertex AI project of the agents (and default of the judge)
location: us                       # default us
runs: 1                            # runs per case, 1-20, default 1
prompts: file://prompts            # prompt store: file://<dir> (a relative dir is relative to
                                   #   this file's directory) or firestore://<project>/<database>[/<root>];
                                   #   required with agent.prompt, not allowed without it
model: gemini-3.8-flash            # default model of variants without one
judge:                             # optional; keys model, project, location only
  model: gemini-3.8-flash          #   project and location default to the ones above
agent:
  name: assistant                  # [A-Za-z_][A-Za-z0-9_]{0,63}, not `user`
  description: Answers questions.  # optional
  prompt: exp-assistant            # exactly one of `prompt` (a prompt name) or `instruction` (text)
variants:                          # 2-10, unique names [A-Za-z0-9_-]{1,64}
  - name: flash-v1
    model: gemini-3.8-flash        # optional when the top-level `model` is set
    prompt: { version: 1 }         # only with agent.prompt: { version: <n> } or { label: <l> };
                                   #   without it the variant uses the latest version
  - name: hydrogen-v1
    model: gemini-4-hydrogen
    prompt: { version: 1 }
```

Without `judge`, the first variant's model judges, in `project` and `location`. Every
variant records its model (and, with `agent.prompt`, its prompt selector) in the report, so
`summary()` and the console show a model column.

The command loads the spec and the eval set, opens the prompt store and checks that every
variant's prompt version exists, opens the `--out` store, builds the judge and one model per
distinct variant model, runs the experiment, stores the report (with `--out`), and then prints
the summary and `stored report <name>`. Storing comes before printing, so a failed write to
stdout still leaves the report stored. Any failure before or during the run exits 2 with one
`gadk-eval: <error>` line on stderr and runs nothing more; spec errors read
`gadk-eval: monitor spec: experiment spec: <detail>` and name the offending key, and model build
errors `gadk-eval: judge: building <model> on Vertex AI: <detail>`. The exit code does not
depend on the case results: compare the variants in the summary or in `gadk-console`.

`ExperimentSpec::load(path)` and `ExperimentSpec::from_yaml(text)` are the library side: they
return the validated spec with defaults filled in (`ExperimentAgent`, whose `instruction` is an
`AgentInstruction::Text` or `AgentInstruction::Prompt`, and `gadk_eval::Variant`s that each
carry their model). A custom `cli::Backends` implements `model(project, location, name)` to
supply the agents' models; the default refuses with `models are not supported by these
backends`.

## Guards

A guard watches a prompt rollout (see the [gadk-prompts Rollouts
section](../gadk-prompts/README.md#rollouts)): it compares the label's version (the baseline, B)
with the rollout version (R) on production signals in the BigQuery agent-analytics table — error
rate, feedback scores, eval scorers, cost per call and p95 latency — and decides whether to
advance the rollout (the next ramp step, or promote R), abort it, or wait for more data. Run on a
schedule with `--apply`, it turns a manual canary into a guarded, ramped one.

```sh
# Dry run: print what it would do, write nothing.
gadk-eval guard guards/support.yaml --prices prices.yaml
# Carry the decision out in the prompt store and keep the report.
gadk-eval guard guards/support.yaml --prices prices.yaml --apply --out gs://my-bucket/monitors
```

| Flag | Default | Meaning |
|---|---|---|
| `--apply` | off | Write the decision to the prompt store: ramp, promote or abort. Without it nothing is written and the report says what would be done. |
| `--prices <file>` | none | Price file for `cost_per_call` (the format of `gadk-console --prices`; see the [console's Usage and cost](../gadk-console/README.md#usage-and-cost)). Without it the cost check is `unknown`. |
| `--out <url>` | none | Store the report under `guards/` of `<url>` as `<name>-<YYYYMMDDTHHMMSSZ>[-<n>].json` and print `stored report <name>`. Monitor and experiment listings ignore `guards/`. Required when the spec sets `min_step_age`: the guard then reads the spec's newest stored report there that applied a ramp (looking at the newest 200 guard reports). |
| `--max-bytes <n>` | 1073741824 | `maximumBytesBilled` of every BigQuery query the guard sends (at least 1). |

The example spec [`examples/guard.yaml`](examples/guard.yaml) uses every check kind.

### Guard spec

```yaml
name: support-answer-guard        # [A-Za-z0-9_-]{1,64}; names the reports
source:                           # BigQuery only
  bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
store: firestore://my-proj        # the prompt store holding the rollout: file://<dir> or
                                  #   firestore://<project>[/<database>[/<root>]]
prompt: support/answer            # the prompt name
label: production                 # the label whose rollout is guarded
window: 24h                       # <n>m | <n>h | <n>d, at most 30d: calls in [now - window, now)
min_calls: 100                    # >= 2, default 100: fewer calls on either side is `insufficient`
min_step_age: 24h                 # optional, <n>m | <n>h | <n>d, at most 30d: an applied ramp
                                  #   step runs at least this long before the next advance;
                                  #   needs --out
ramp: [10, 25, 50]                # optional: percents 1-99, strictly increasing
checks:                           # at least one; only `feedback` may repeat (with another
                                  #   name or source)
  - error_rate: { max_increase: 0.01 }          # absolute, 0-1
  - feedback:
      name: thumbs_up             # the score name
      source: app                 # app | console | any (default any)
      max_decrease: 0.05          # absolute for bool scores, relative for number scores, 0-1
      min_count: 30               # >= 2, default 30: fewer scores on either side is `insufficient`
  - cost_per_call: { max_increase: 0.2 }        # relative, >= 0
  - latency_p95: { max_increase: 0.25 }         # relative, >= 0
  - eval:
      scorers:                    # gadk-eval scorer syntax, as in a monitor spec; no reference
                                  #   or equals
        - llm_judge: { rubric: "Polite and accurate", min_score: 4 }
      max_decrease: 0.05          # absolute pass-rate drop, 0-1
      limit: 100                  # 10-500, default 100: runs scored per version
judge:                            # required if and only if an eval scorer is llm_judge
  model: gemini-3.8-flash         # Gemini on Vertex AI
  project: my-proj                # default: the source project
  location: us                    # default us (the US multi-region)
  timeout: 120s                   # <n>s | <n>m, default 120s
```

Each check is a one-key mapping. A spec error prints as `monitor spec: guard spec: <detail>`,
the detail naming the key.

What each check measures, per version:

- `error_rate`: the guard's calls are the `LLM_REQUEST` rows whose `attributes.prompt` has the
  spec's prompt and label and version B or R, logged in the window. A call failed when an
  `LLM_ERROR` row has its `trace_id` and `span_id`. Error rate = failed calls ÷ calls.
- `feedback`: the latest score per invocation, name and source (as the console counts them)
  for the check's `name`, joined to the calls by `invocation_id`. `source: any` counts the
  latest score of each source, so one invocation can count twice. Bool scores compare the share
  of `true`; number scores compare means, and higher is better. Category scores, or a data type
  that differs between the sides, make the check `unknown`.
- `cost_per_call`: the token counts of the calls' `LLM_RESPONSE` rows, per model, priced with
  `--prices`, divided by the calls. Failed calls have no response tokens but still count in
  the divisor, so a version that fails more looks cheaper per call (the `error_rate` check
  catches that). A model without a price makes the check `unknown`, naming the model.
- `latency_p95`: the approximate 95th percentile of the `LLM_RESPONSE` rows'
  `latency_ms.total_ms`.
- `eval`: up to `limit` invocations per version (newest first), rebuilt as `monitor` rebuilds
  them and scored with the check's scorers. A run with a scorer error is left out of the pass
  rate and counted in the reason.

### Decision rule

Each check gets a status:

- `insufficient` when either side has fewer than `min_calls` calls; also, for `feedback`, fewer
  than `min_count` scores, and for `eval`, fewer than 10 scored runs.
- `unknown` when the values cannot be compared (see above; also a baseline mean, cost or
  latency of 0, or a missing latency).
- Otherwise the guard measures the rollout's harm h (positive = worse) and its tolerance t
  (`max_increase` or `max_decrease`):
  - `error_rate`: h = rate(R) − rate(B), in percentage points.
  - bool `feedback` and `eval`: h = rate(B) − rate(R).
  - number `feedback`: h = (mean(B) − mean(R)) ÷ |mean(B)|, a share of the baseline mean.

  It computes a range [L, U] that, with 95% confidence, still contains h's true value on its
  own side (each bound is one-sided 95%, z = 1.6448536269514722). For rates the range is
  Newcombe's hybrid score interval, built from a Wilson score interval for each side; for
  number means it is Welch's interval (normal quantile, no t distribution) divided by
  |mean(B)|. The check is `worse` when L > t (the rollout is worse by more than the tolerance
  even at its best), `ok` when U ≤ t (no worse than the tolerance even at its worst), and
  `unsure` otherwise (more data needed).
- `cost_per_call` and `latency_p95` use the point value h = (value(R) − value(B)) ÷ value(B):
  `worse` when h > t, else `ok`. They can flap near the tolerance, so give them looser
  tolerances.

The verdict is `abort` when any check is `worse`, else `advance` when every check is `ok`, else
`wait`. `advance` ramps to the first `ramp` step above the current percent, or promotes R when
there is none (no `ramp`, or the percent is at or past its last step).

With `min_step_age`, the guard reads the spec's newest stored report that applied a ramp
(held, waiting and dry-run reports are skipped). When it ramped to q% for the same prompt, label
and rollout version, the rollout is still at q%, and that report was made less than
`min_step_age` ago (a report time in the future counts as now), an `advance` is held back: the
verdict becomes `wait`, the action none, and the report gets a note such as
`note: step at 25% is 3h old (< min_step_age 1d)` after the verdict line. An `abort` is never
held back.

A label without a rollout (none was started, or a guard run already promoted or aborted it) is
nothing to guard: the command prints `guard <name>: <prompt>/<label> has no rollout`, stores no
report and exits 4.

With `--apply`:

- ramp: `set_rollout_if(prompt, label, Some(R at the next step), B, Some(<the rollout read>))`;
- abort: `set_rollout_if(prompt, label, None, B, Some(<the rollout read>))`;
- promote: a fresh read that must still show the label at B with the same rollout, then
  `set_label(prompt, label, R, Some(Some(B)))`, which moves the label and clears the rollout.

These writes are compare-and-set: when the label or rollout changed since the guard read them
(another guard run, someone in the console), nothing is written and the command fails with
`prompt store changed since it was read (v<B>, v<R> at <p>%): …`. A store without
`set_rollout_if` answers `rollouts are not supported by this store` and nothing is written.

The report prints a header, one row per check (each side's value and sample size, the harm,
the status and its reason), the verdict and the action:

```text
guard support-answer-guard: support/answer/production v3 → v4 (10%), window 1d
  check                     baseline            rollout             harm     status
  error_rate                1.20% (n=1000)      1.25% (n=1200)      +0.05pt  ok
  feedback thumbs_up (app)  91.00% (n=300)      85.00% (n=40)       +6.00pt  unsure
  cost_per_call             $0.000123 (n=1000)  $0.000131 (n=1200)  +6.5%    ok
verdict: wait
action: none
```

### Exit codes

| Exit | `guard` |
|---|---|
| 0 | `advance` (applied or not) |
| 1 | `abort` (applied or not) |
| 3 | `wait` (including an `advance` held back by `min_step_age`) |
| 4 | the label has no rollout: nothing to guard, no report stored |
| 2 | the spec or price file is invalid, the spec sets `min_step_age` without `--out`, the prompt store fails or has no such prompt or label (the message names the prompt and label), the store changed since it was read, a query or the judge fails, the report cannot be stored, or a usage error. The report is printed before it is stored, so when storing fails after `--apply`, the decision printed is already written to the prompt store and stays written |

### Running many guards

`gadk-eval guards <source>...` runs every guard spec its sources name, one at a time, in the
order given:

| Source | Specs |
|---|---|
| a file | that file |
| a directory | its regular files (symlinks followed) whose names end in `.yaml` or `.yml`, sorted by name; subdirectories are ignored |
| `gs://<bucket>/<object>` | that object |
| `gs://<bucket>/<prefix>/` or `gs://<bucket>` | the objects directly under the prefix whose names end in `.yaml` or `.yml`, sorted by name |

`--apply`, `--out` and `--max-bytes` mean what they mean for `guard`, and `--prices` also takes
a `gs://<bucket>/<object>`. The price file is read once and `--out` is opened once for every
guard.

Every spec is read and parsed, and every check below is made, before any guard runs; the first
failure exits 2 with nothing run or written:

- a source is missing (`guard spec <source>: not found`), unreadable, or an empty directory or
  prefix (`guard spec <source>: no .yaml or .yml specs`);
- a spec is invalid (`guard spec <origin>: <detail>`, where `<origin>` is the file path or
  `gs://` URL);
- there are more than 200 specs;
- two specs share a `name` (`guards: duplicate guard name <name> in <origin1> and <origin2>`);
- a spec sets `min_step_age` without `--out`;
- the price file cannot be read or parsed;
- `--out` is not a valid store URL.

Each guard's output starts with `== <name> ==`, followed by exactly what `guard` prints for it:
the report and, with `--out`, `stored report <name>`, or the `no rollout` line. A guard that
fails (the prompt store, a query, the judge or the report store) prints `error: <message>`
under its header and the run goes on to the next guard; a decision it already wrote with
`--apply` stays written. The last line counts the outcomes:

```text
guards: 3 run — 1 advance, 0 abort, 1 wait, 0 no rollout, 1 errors
```

| Exit | `guards` |
|---|---|
| 0 | every guard reached an outcome: advance, abort, wait or no rollout |
| 2 | a guard failed (the summary counts it under `errors`), a check before the run failed, stdout could not be written, or a usage error (no summary is printed for these three) |

### Sample sizes and false aborts

Intervals are wide on small samples, so a guard often says `wait` early, and a tight tolerance
may never reach `ok`. With 0 errors in 100 calls on both sides, the upper bound of the error-rate
increase is U ≈ 0.026: a `max_increase` of 0.01 cannot be `ok` until each side has about 270
calls, even with no errors at all. Pick a window, `min_calls` and tolerances your traffic can
meet, and remember that the rollout side only gets its percent of the traffic.

The decision rule has no correction for repeated looks or for several checks: every scheduled
run, and every check in it, is another chance for noise to cross a tolerance, so a guard run
hourly for days with five checks will now and then abort a rollout that was fine. An abort only
removes the rollout (every session goes back to B) and can be undone by starting the rollout
again; use looser tolerances, fewer checks or a longer interval between runs if false aborts
cost you too much.

### Scheduling

Run the guard where it can reach BigQuery and the prompt store, on a schedule shorter than the
window. Without `min_step_age`, the schedule interval is the only soak time a ramp step gets:
an hourly guard whose checks are all `ok` steps through `ramp: [10, 25, 50]` and promotes within
a few hours, each step judged mostly on calls made before it. With `ramp`, set `min_step_age`
to about the window, so each step runs a full window before the next `advance` (this needs
`--out`).

Once the rollout ends (promoted, aborted, or removed by hand) its guard reports no rollout until
a new rollout starts. Schedule `gadk-eval guards` rather than `guard`: advance, abort, wait and
no rollout all exit 0, so exit 2 is the only failure to alert on, and no wrapper script is
needed. Keep the specs and the price file in a bucket: editing or adding a guard is then an
upload, with no image rebuild. The guards run one at a time, so keep a whole run (each query
bounded by `--max-bytes`) well inside the schedule interval. An exit 2 after `--apply` does not
undo decisions already written: check the printed actions before retrying.

With cron, every hour:

```text
0 * * * * gadk-eval guards gs://my-bucket/guards/ --apply --prices gs://my-bucket/prices.yaml --out gs://my-bucket/monitors >> /var/log/gadk-guards.log 2>&1
```

As a Cloud Run job started hourly by Cloud Scheduler (the image only needs `gadk-eval`; the
job's service account needs the roles in [Access (IAM)](#access-iam)):

```sh
gcloud run jobs create guards --region us-central1 \
  --image us-docker.pkg.dev/my-proj/tools/gadk-eval:latest \
  --command gadk-eval \
  --args guards,gs://my-bucket/guards/,--apply,--prices,gs://my-bucket/prices.yaml,--out,gs://my-bucket/monitors \
  --max-retries 0 \
  --service-account guard@my-proj.iam.gserviceaccount.com
gcloud scheduler jobs create http guards-hourly --location us-central1 \
  --schedule "0 * * * *" --http-method POST \
  --uri "https://run.googleapis.com/v2/projects/my-proj/locations/us-central1/jobs/guards:run" \
  --oauth-service-account-email guard@my-proj.iam.gserviceaccount.com
```

`--max-retries 0` keeps a failed run from being repeated at once; the next scheduled run tries
again.

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
a single `RunTranscript` with the same scorers. The `export` module is the library side of
`gadk-eval export`: `ExportSource` (implemented by `BigQueryTranscriptSource`, which can run its
queries in another project with `with_billing_project` and cap them with
`with_max_bytes_billed`), `run_to_case`, `export_runs`, `cases_text`, `write_set_file`,
`ScoreFilter` (with `ScoreFilter::parse` for the `--score` syntax) and `ExportedCase::new`.

`Guard::new().run(&spec, store, source, prices, judge, previous, apply)` is the library side of
`gadk-eval guard`: a `GuardSpec`, a `gadk_prompts::PromptStore`, a `GuardSource`
(`BigQueryGuardSource`, or your own for other data), an optional `PriceTable` and judge, and
the spec's previous `GuardReport` for `min_step_age`. It returns `None` when the label has no
rollout, else a `GuardReport` (`summary()`, `exit_code()`, JSON), which
`ReportStore::put_guard` stores and `guard_names` / `find_guard` read back. A custom
`cli::Backends` implements `guard_source(project, dataset, table, max_bytes)`; the default
refuses with `guards are not supported by these backends`.

## Access (IAM)

Google Cloud access uses Application Default Credentials; specs and reports hold no
credentials. Reports do hold production content (user messages, replies, tool call arguments and
error tracebacks), so restrict who can read the `gs://` bucket or local directory they are
written to as you would the analytics table.

- BigQuery Data Viewer on the analytics dataset (it also allows reading the table schema) and
  BigQuery Job User on the project that runs the queries (the source project).
- Cloud Trace User on the trace project (only with a `cloud_trace` source).
- Vertex AI User on the judge project (only with `llm_judge` scorers).
- Vertex AI User on the experiment spec's `project` (`gadk-eval experiment`: the agents and,
  by default, the judge), and read access to a `firestore://` prompt store when the spec uses
  one.
- `gadk-eval guard`: the BigQuery roles above, read access to the prompt store, and with
  `--apply` write access to it (Cloud Datastore User for a `firestore://` store); Vertex AI
  User on the judge project with `llm_judge` scorers.
- `gadk-eval guards`: what `guard` needs for each spec, and Storage Object Viewer
  (`roles/storage.objectViewer`) on the bucket holding `gs://` specs and the price file.
- Storage Object Admin on the bucket for `gs://` output (`report` needs only Storage Object
  Viewer).

## Cost

Each monitor run reads the table schema (free) and issues two BigQuery queries: the invocations
that started in the window, then the rows of at most `limit` of them. Both filter on `timestamp`, so a table partitioned by
`timestamp` (the analytics plugin's default) scans only the window's partitions; keep the window
short and the event types filtered. Judge cost grows with `limit` times the number of
`llm_judge` scorers; runs are graded 8 at a time.
`export` issues up to eight queries per call (the trace's invocations or the matching ones,
their run headers and rows, the sessions' earlier run headers and rows, the scores and the
prompt versions), each bounded by `timestamp` and by invocation, session or trace ids, with no
byte cap.

Each guard run issues one query for both versions' calls and, only when both sides have
`min_calls` calls, one per `feedback` check and the run queries for each version of an `eval`
check. Each of these reads the table schema (free) first. Every query filters on `timestamp`
from the window's start and is capped at `--max-bytes` (default 1 GiB) billed; a query over
the cap fails (exit 2) instead of running. A label without a rollout issues no query.

With the Cloud Trace source, each monitor run makes up to 50 `COMPLETE` `traces.list` calls of
100 traces each, counted against the Cloud Trace API read quota.

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
