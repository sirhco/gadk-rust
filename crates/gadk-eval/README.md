# gadk-eval

Offline evaluation of gadk agents. An eval set (YAML) holds test cases: user turns plus the
scorers that grade the agent's replies. `Evaluator::run` runs every case against an agent built
once per *variant* (typically one per prompt-registry `Selector`), grades every run, and returns
an `EvalReport`: a text summary for the terminal and the full JSON for files and CI.

Use it before promoting a prompt label: run the set against `prod` and the candidate version
and compare.

## Example

```rust
use std::sync::Arc;
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_prompts::{Label, PromptRegistry, Selector};

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

## Errors versus failures

A result is `pass`, `fail` or `error`. Errors are counted apart from failures:

- a run whose `build` failed, whose agent returned an error (or an error event), or that took
  longer than `run_timeout` (default 120 s): every scorer of that run is an error;
- a judge model error (not retried), or a judge reply still unparseable after the retry.

`EvalReport::all_passed()` is true only with no failures and no errors.

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

## Live test

```sh
GADK_EVAL_LIVE_PROJECT=<project> cargo test -p gadk-eval --test eval_live -- --ignored --nocapture
```

Gemini on Vertex AI (Application Default Credentials) is both agent and judge;
`GADK_EVAL_LIVE_LOCATION` (default `us-central1`) and `GADK_EVAL_LIVE_MODEL` (default
`gemini-2.5-flash`) override the defaults.
