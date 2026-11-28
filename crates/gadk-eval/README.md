# gadk-eval

Offline evaluation of gadk agents. An eval set (YAML) holds test cases: user turns plus the
scorers that grade the agent's replies. `Evaluator::run` runs every case against an agent built
once per *variant* (typically one per prompt-registry `Selector`), grades every run, and returns
an `EvalReport`: a text summary for the terminal and the full JSON for files and CI.

Use it before promoting a prompt label: run the set against `prod` and the candidate version
and compare.

## Example

```rust
// Inside an async fn returning Result; `judge`, `registry` and `build_agent` are yours.
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_prompts::{Label, Selector};

let set = EvalSet::load("evals/support.yaml")?;
let report = Evaluator::new(judge)            // any gadk model: Gemini, Claude, OpenAI, …
    .concurrency(4)                          // runs at once (default 4)
    .runs_per_case(1)                        // repeats per case (default 1)
    .run(
        &set,
        &[
            Variant::prompt("prod", Selector::Label(Label::production())),
            Variant::prompt("v7", Selector::Version(7)),
        ],
        |v| build_agent(&registry, v.selector().cloned().unwrap_or(Selector::Latest)),
    )
    .await?;
print!("{}", report.summary());
report.write_json("eval-out.json")?;
assert!(report.all_passed());
```

`build` is called once per variant. A variant whose `build` fails gets error results; the other
variants still run.

A `Variant` is a name plus two optional settings for your `build` closure: a prompt-registry
`Selector` (`Variant::prompt(name, selector)`, read with `selector()`) and a model name
(`with_model`, read with `model()`), for example
`Variant::prompt("v7", Selector::Version(7)).with_model("gemini-4-hydrogen")`. `Variant::new`
and `Variant::prompt` set no model. The evaluator itself reads neither setting; `build` decides
what they mean, and `gadk-eval-monitor` experiments record both in their reports.

## Eval set format

```yaml
name: support-bot
judge_rubric: "Polite, accurate, no invented policy."      # default rubric for llm_judge
cases:
  - id: refund-window                                      # unique
    state: { tier: gold }                                  # optional initial session state
    turns: ["I want a refund", "Order 123, bought 40 days ago"]
    expect:
      - contains: "30 days"                                # = {text: "30 days"}
      - contains: { text: "sorry", ignore_case: true, turn: 2 }
      - not_contains: "guarantee"                          # same options as contains
      - regex: "(?i)store credit"                          # = {pattern: …}; option turn
      - equals: { text: "OK", turn: 1 }                    # = {text: …}; option turn
      - tools: [lookup_order]                              # = {calls: [lookup_order]}
      - tools: { calls: [lookup_order, { name: issue_credit, args: { order: "123" } }],
                 ordered: true, exact: false }
      - reference: "Refunds only within 30 days; offer store credit."  # options min_score, turn
      - llm_judge: { rubric: "Offers store credit", min_score: 4 }     # or llm_judge: "<rubric>"
      - llm_judge: {}                                      # uses judge_rubric
```

Each case runs in a fresh in-memory session; its turns are sent in order in that one session.
`turn` is 1-based and defaults to the last turn. Loading fails with the case named on any
unknown key, bad regex, out-of-range `turn` or `min_score`, duplicate id, or missing rubric.

## Writing eval sets

`EvalSet::to_yaml()` writes a set as YAML that `EvalSet::from_yaml` reads back unchanged: every
string double-quoted (its JSON form, with U+007F, U+0080 to U+009F, U+FFFE and U+FFFF escaped
as `\uXXXX`), `state` and tool `args` as JSON flow mappings, each scorer as a one-line mapping,
and an `llm_judge` whose rubric is the set's `judge_rubric` as `llm_judge: {}` (or
`{min_score: n}`). `to_yaml_with_comments(&|case| …)` writes the lines it returns as `# `
comments above each case; line breaks and other control characters in them become spaces.

`append_cases(text, &cases, &comments)` returns an eval-set file's text with new cases appended
after the last item of its `cases` list, at that list's item indentation (zero-indented lists
included), with `\n` line endings. The existing text, comments and formatting included, is kept
byte for byte. The file must parse, have `cases:` as its last top-level key with nothing after
its colon (a block list, not `cases: [ … ]`) and no `...` document-end marker. A new id that is
already in the set, or anything the function cannot place, is an `EvalError::Load` and nothing
is returned; the result is parsed again, and returned only when its ids are the old ones
followed by the new ones.

## Scorers and pass rules

| Scorer | Passes when |
|---|---|
| `contains` / `not_contains` | the reply contains (does not contain) `text`; case-sensitive unless `ignore_case` |
| `regex` | `pattern` (Rust `regex` syntax) matches anywhere in the reply |
| `equals` | the reply equals `text`, both trimmed |
| `tools` | every listed call was made, across all turns (extra calls allowed); `ordered`: in that order, others may sit between; `exact`: exactly that sequence. `args` must be a subset of the call's args with equal values |
| `reference` | the judge says the reply agrees with `answer` and scores it ≥ `min_score` (default 3) |
| `llm_judge` | the judge says the conversation meets `rubric` and scores it ≥ `min_score` (default 3) |

The judge is called at temperature 0 and must reply with JSON
`{"pass": bool, "score": 1-5, "rationale": "…"}` (bare, in a ```json block, or embedded in
text). An unparseable reply is retried once.

## Judge backends

The `llm_judge` and `reference` scorers are graded by a `JudgeBackend`. `Evaluator::new(model)`
and `score_transcript(model, timeout, …)` wrap the model in `GeminiJudge`, the judge described
above; any other backend plugs in without a judge model:

```rust
// Inside an async fn; `backend` is an `Arc<dyn JudgeBackend>`, `set` an `EvalSet` and `t` a
// `RunTranscript`.
use gadk_eval::{Evaluator, score_transcript_with};

// Every judge scorer of every run is graded by `backend`; no judge model is built.
let evaluator = Evaluator::with_backend(backend.clone()).runs_per_case(3);
// The same, on an evaluator built with a judge model.
let evaluator = Evaluator::new(model).with_judge_backend(backend.clone());
// One transcript, graded as `Evaluator::run` grades a run.
let scores = score_transcript_with(backend.as_ref(), &set.cases[0].expect, &t).await;
```

A backend implements one method, `grade(&JudgeRequest) -> BoxFuture<JudgeOutcome>`
(`Box::pin(async move { … })`). The request carries the scorer kind
(`JudgeKind::Rubric { rubric }` or `JudgeKind::Reference { question, answer }`), the
conversation as `render_conversation` prints it (`User:`, `Agent tool calls:` and `Agent:`
lines) and the graded turn's reply. A `JudgeOutcome::Verdict { pass, score, rationale }`
passes when `pass` is set and `score` (1-5) is at least the scorer's `min_score`; a
`JudgeOutcome::Error(text)` is an error result with that text. With `Evaluator::with_backend`,
the run timeout does not bound the backend's calls; the backend bounds its own.
`gadk-eval-monitor`'s `EvalServiceJudge` is a backend that asks the Vertex AI Gen AI evaluation
service.

`judge_prompt(&req)` returns exactly the prompt `GeminiJudge` sends for a request, and
`parse_judge_reply(text)` reads a reply to it as `GeminiJudge` does: a `JudgeOutcome::Verdict`,
or `JudgeOutcome::Error("unparseable judge reply: <at most the first 1024 bytes>")`. Together they let
the same prompt go through another channel, such as the batch judging of
`gadk-eval-monitor`; unlike `GeminiJudge`, nothing is retried there.

## Errors versus failures

A result is `pass`, `fail` or `error`. Errors are counted apart from failures:

- a run whose `build` failed, whose agent returned an error (or an error event), or that took
  longer than `run_timeout` (default 120 s): every scorer of that run is an error;
- a judge model error or a judge call longer than `run_timeout` (neither retried), or a judge
  reply still unparseable after the retry.

`EvalReport::all_passed()` is true only with at least one score, no failures and no errors
(including a run or build error in a case with no scorers).

## Report

`summary()`:

```text
eval set: support-bot

variant  scorer     pass rate   mean score  errors
prod     contains   1/2 (50%)   -           0
prod     llm_judge  1/2 (50%)   3.00        0
prod     overall    2/4 (50%)   3.00        0
```

Pass rate is passes / (passes + failures); mean score averages the judge scores. `write_json`
writes every variant, case and run with its transcript (turns, replies, tool calls) and scores.

## Run metrics

`Evaluator::run_measured(&set, &variants, build)` runs exactly like `run` and also measures each
run. It returns a `MeasuredReport`: `report` is the `EvalReport` that `run` returns for the same
inputs, and `metrics[v][i]` is the `RunMetrics` of `report.variants[v].runs[i]`.

- `duration_ms`: from just before the run's runner and session are created until the run
  completes, fails or times out (a timed-out run lasts at least `run_timeout`).
- `models`: `TokenCounts` per model, keyed by the responses' `model_version` (`unknown` when
  empty) and summed over every model call of the run: `input` = `prompt_token_count` +
  `tool_use_prompt_token_count`, `cached` = `cached_content_token_count`, `output` =
  `candidates_token_count`, `reasoning` = `thoughts_token_count`. Negative counts are 0, only
  final (non-partial) responses count, and a response without usage metadata adds nothing.
  `RunMetrics::total()` sums the models.

A variant whose `build` failed gets zero metrics (`RunMetrics::default()`) for each of its runs.
`gadk-eval-monitor`'s experiments store these metrics and compare variants with them.

`RunMetrics` and `TokenCounts` deserialize with any missing field defaulting (0, or no
models), so stored metrics stay readable when a field is absent.

## Live test

```sh
GADK_EVAL_LIVE_PROJECT=<project> cargo test -p gadk-eval --test eval_live -- --ignored --nocapture
```

Gemini on Vertex AI (Application Default Credentials) is both agent and judge;
`GADK_EVAL_LIVE_LOCATION` (default `us`, the US multi-region) and `GADK_EVAL_LIVE_MODEL` (default
`gemini-3.8-flash`) override the defaults.
