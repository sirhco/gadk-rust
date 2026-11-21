// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Production runs as eval cases: [`run_to_case`] turns a [`ProductionRun`] and its scores into
//! a `gadk-eval` [`EvalCase`].
//!
//! A case's scorers follow the run's scores: a run whose latest scores are positive (a thumbs
//! up, or a number of at least [`ExportOptions::min_good`]) and none negative becomes a
//! regression case with the production reply as its `reference` (plus a `tools` trajectory);
//! every other run becomes an `llm_judge` case graded against a rubric.

use chrono::{DateTime, SecondsFormat, Utc};
use gadk_eval::{EvalCase, ScorerSpec, ToolExpect};
use serde_json::{Map, Value};

use crate::source::ProductionRun;

/// The most characters of the production reply quoted in a case's comments.
const REPLY_COMMENT_MAX: usize = 2_000;

/// One score of a run: the latest `FEEDBACK` row of one name from one source.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RunScore {
    /// The score name.
    pub name: String,
    /// The value: a JSON boolean, number or string.
    pub value: Value,
    /// `bool`, `number` or `category`.
    pub data_type: String,
    /// Who recorded it: `app` or `console`.
    pub source: String,
    /// The author (an end user or a console user).
    pub author: String,
    /// The comment, if any.
    pub comment: Option<String>,
    /// When it was recorded.
    pub at: DateTime<Utc>,
}

impl RunScore {
    /// A score without a comment.
    pub fn new(
        name: impl Into<String>,
        value: Value,
        data_type: impl Into<String>,
        source: impl Into<String>,
        author: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            name: name.into(),
            value,
            data_type: data_type.into(),
            source: source.into(),
            author: author.into(),
            comment: None,
            at,
        }
    }

    /// With the comment `comment`.
    #[must_use]
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }
}

/// What an export knows about a run besides its transcript.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct ExportContext {
    /// The latest score per (name, source).
    pub scores: Vec<RunScore>,
    /// The prompt name and version of the run's first model request, when logged.
    pub prompt: Option<(String, i64)>,
}

impl ExportContext {
    /// A context with `scores` and no prompt.
    pub fn new(scores: Vec<RunScore>) -> Self {
        Self {
            scores,
            prompt: None,
        }
    }

    /// With the prompt `name` at `version`.
    #[must_use]
    pub fn with_prompt(mut self, name: impl Into<String>, version: i64) -> Self {
        self.prompt = Some((name.into(), version));
        self
    }
}

/// How runs become cases.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExportOptions {
    /// The rubric of `llm_judge` cases; `None` skips them ("needs a rubric").
    pub rubric: Option<String>,
    /// The lowest `number` score that counts as positive (default 4).
    pub min_good: f64,
    /// Whether a case starts with the user messages of the session's earlier runs (default
    /// true).
    pub history: bool,
    /// Whether `tools` scorers keep each call's arguments (default false: names only).
    pub tool_args: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            rubric: None,
            min_good: 4.0,
            history: true,
            tool_args: false,
        }
    }
}

impl ExportOptions {
    /// With the rubric `rubric` for `llm_judge` cases.
    #[must_use]
    pub fn with_rubric(mut self, rubric: impl Into<String>) -> Self {
        self.rubric = Some(rubric.into());
        self
    }

    /// With `min_good` as the lowest positive `number` score.
    #[must_use]
    pub fn with_min_good(mut self, min_good: f64) -> Self {
        self.min_good = min_good;
        self
    }

    /// With or without the session history.
    #[must_use]
    pub fn with_history(mut self, history: bool) -> Self {
        self.history = history;
        self
    }

    /// With or without tool arguments in `tools` scorers.
    #[must_use]
    pub fn with_tool_args(mut self, tool_args: bool) -> Self {
        self.tool_args = tool_args;
        self
    }
}

/// A run converted to a case.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExportedCase {
    /// The case: id `inv-<invocation id>`, no state, the history and run user messages as
    /// turns.
    pub case: EvalCase,
    /// The comment lines to write above the case (where it came from, its scores and the
    /// production reply).
    pub comments: Vec<String>,
    /// Whether the run was positive: the case has a `reference` scorer rather than an
    /// `llm_judge`.
    pub positive: bool,
}

impl ExportedCase {
    /// `case` with the comment lines `comments`; `positive` when it grades against the
    /// production reply (`reference`) rather than a rubric (`llm_judge`).
    pub fn new(case: EvalCase, comments: Vec<String>, positive: bool) -> Self {
        Self {
            case,
            comments,
            positive,
        }
    }
}

/// A filter on a run's latest score of one name (across sources).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ScoreFilter {
    /// `name=value`: the score's value, as text (`true`, `4`, `helpful`), equals `value`.
    Equals {
        /// The score name.
        name: String,
        /// The value text.
        value: String,
    },
    /// `name<n`, `name<=n`, `name>n`, `name>=n`: a `number` score compared with `value`.
    Compare {
        /// The score name.
        name: String,
        /// The comparison.
        op: CompareOp,
        /// The number compared with.
        value: f64,
    },
}

/// A comparison of a [`ScoreFilter::Compare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompareOp {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl ScoreFilter {
    /// Parses `name=value`, `name<n`, `name<=n`, `name>n` or `name>=n`, where `name` is
    /// `[A-Za-z0-9_.-]+`, `value` is non-empty and `n` is a finite number.
    ///
    /// # Errors
    ///
    /// A message naming the expected forms.
    pub fn parse(s: &str) -> Result<ScoreFilter, String> {
        let bad = || {
            format!(
                "invalid score filter {s:?} (expected name=value, name<n, name<=n, name>n or \
                 name>=n, with name [A-Za-z0-9_.-]+)"
            )
        };
        let len = s
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            .count();
        let (name, rest) = s.split_at(len);
        if name.is_empty() {
            return Err(bad());
        }
        let name = name.to_owned();
        let compare = [
            ("<=", CompareOp::Le),
            (">=", CompareOp::Ge),
            ("<", CompareOp::Lt),
            (">", CompareOp::Gt),
        ]
        .into_iter()
        .find_map(|(prefix, op)| rest.strip_prefix(prefix).map(|v| (op, v)));
        match compare {
            Some((op, value)) => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(|value| ScoreFilter::Compare { name, op, value })
                .ok_or_else(bad),
            None => match rest.strip_prefix('=') {
                Some(value) if !value.is_empty() => Ok(ScoreFilter::Equals {
                    name,
                    value: value.to_owned(),
                }),
                _ => Err(bad()),
            },
        }
    }
}

/// An invocation of a trace and when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TraceInvocation {
    /// The trace id.
    pub trace_id: String,
    /// The invocation id.
    pub invocation_id: String,
    /// When the invocation started.
    pub started_at: DateTime<Utc>,
}

impl TraceInvocation {
    /// Invocation `invocation_id` of trace `trace_id`, started at `started_at`.
    pub fn new(
        trace_id: impl Into<String>,
        invocation_id: impl Into<String>,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            trace_id: trace_id.into(),
            invocation_id: invocation_id.into(),
            started_at,
        }
    }
}

/// `run` as a case. `history` holds the session's earlier runs, oldest first (used only with
/// [`ExportOptions::history`]); `ctx` holds the run's scores and prompt.
///
/// The case id is `inv-<invocation id>` and its turns are the user messages of the complete
/// history runs, then the run's own. Its scorers follow the latest score of each name across
/// sources: positive (some `bool` is true or some `number` is at least `min_good`) with no
/// negative and a non-empty final reply gives a `reference` scorer with that reply (min score 3)
/// plus, when the run made tool calls and they were all rebuilt, an ordered, non-exact `tools`
/// scorer; anything else gives one `llm_judge` with `opts.rubric`. The comments name the trace,
/// session, invocation, start, agent, prompt version, every score and the reply (cut to 2,000
/// characters); incomplete history runs are named there instead of becoming turns.
///
/// # Errors
///
/// The reason the run is skipped: its notes when its content is incomplete, or `needs a rubric`
/// for a judge case without one.
pub fn run_to_case(
    run: &ProductionRun,
    history: &[ProductionRun],
    ctx: &ExportContext,
    opts: &ExportOptions,
) -> Result<ExportedCase, String> {
    if !run.content_complete || run.transcript.turns.is_empty() {
        return Err(if run.notes.is_empty() {
            "the run has no user message".to_owned()
        } else {
            format!("incomplete content: {}", run.notes.join("; "))
        });
    }
    let mut comments = vec![
        format!("trace {}", run.trace_id.as_deref().unwrap_or("none")),
        format!("session {}", run.session_id),
        format!("invocation {}", run.invocation_id),
        format!(
            "started {}",
            run.started_at.to_rfc3339_opts(SecondsFormat::Secs, true)
        ),
        format!("agent {}", run.agent),
    ];
    if let Some((name, version)) = &ctx.prompt {
        comments.push(format!("prompt {name}@{version}"));
    }
    let mut scores: Vec<&RunScore> = ctx.scores.iter().collect();
    scores.sort_by(|a, b| (&a.name, &a.source).cmp(&(&b.name, &b.source)));
    for s in &scores {
        let mut line = format!(
            "score {} = {} (source {}, author {})",
            s.name,
            value_text(&s.value),
            s.source,
            s.author
        );
        if let Some(comment) = &s.comment {
            line.push_str(&format!(": {comment}"));
        }
        comments.push(line);
    }
    let mut turns = Vec::new();
    if opts.history {
        for h in history {
            if h.content_complete {
                turns.extend(h.transcript.turns.iter().map(|t| t.user.clone()));
            } else {
                comments.push(format!(
                    "history run {} left out: {}",
                    h.invocation_id,
                    h.notes.join("; ")
                ));
            }
        }
    }
    turns.extend(run.transcript.turns.iter().map(|t| t.user.clone()));
    let reply = run.transcript.reply(None);
    comments.push(format!("reply {}", truncate(reply, REPLY_COMMENT_MAX)));
    let positive = is_positive(&ctx.scores, opts.min_good) && !reply.trim().is_empty();
    let expect = if positive {
        let mut expect = vec![ScorerSpec::Reference {
            answer: reply.to_owned(),
            min_score: 3,
            turn: None,
        }];
        let calls = run.transcript.all_calls();
        if !calls.is_empty() && run.tools_complete {
            expect.push(ScorerSpec::Tools {
                calls: calls
                    .iter()
                    .map(|c| ToolExpect {
                        name: c.name.clone(),
                        args: opts.tool_args.then(|| c.args.clone()),
                    })
                    .collect(),
                ordered: true,
                exact: false,
            });
        }
        expect
    } else {
        let rubric = opts
            .rubric
            .clone()
            .filter(|r| !r.trim().is_empty())
            .ok_or_else(|| "needs a rubric".to_owned())?;
        vec![ScorerSpec::LlmJudge {
            rubric,
            min_score: 3,
        }]
    };
    let case = EvalCase {
        id: format!("inv-{}", run.invocation_id),
        state: Map::new(),
        turns,
        expect,
    };
    Ok(ExportedCase::new(case, comments, positive))
}

/// Whether the latest score of each name (across sources) has a positive one and no negative
/// one: a `bool` true or a `number` at least `min_good` is positive, a `bool` false or a smaller
/// `number` negative; categories are neither.
fn is_positive(scores: &[RunScore], min_good: f64) -> bool {
    let mut latest: Vec<&RunScore> = Vec::new();
    for s in scores {
        match latest.iter_mut().find(|l| l.name == s.name) {
            Some(l) if (s.at, &s.source) > (l.at, &l.source) => *l = s,
            Some(_) => {}
            None => latest.push(s),
        }
    }
    let (mut positive, mut negative) = (false, false);
    for s in latest {
        let verdict = match s.data_type.as_str() {
            "bool" => bool_value(&s.value),
            "number" => number_value(&s.value).map(|n| n >= min_good),
            _ => None,
        };
        match verdict {
            Some(true) => positive = true,
            Some(false) => negative = true,
            None => {}
        }
    }
    positive && !negative
}

/// A JSON boolean, or the text `true` / `false`.
fn bool_value(v: &Value) -> Option<bool> {
    v.as_bool().or_else(|| match v.as_str() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    })
}

/// A JSON number, or text that parses as one.
fn number_value(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

/// A score value as shown in a comment: a string as is, anything else as JSON.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `text` cut to `max` characters, with `…` when cut.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use gadk_eval::{EvalSet, ToolCall};
    use serde_json::json;

    use super::*;
    use crate::testutil::{at, run};

    const RUBRIC: &str = "Answers the question.";

    fn opts() -> ExportOptions {
        ExportOptions::default().with_rubric(RUBRIC)
    }

    fn score(name: &str, value: Value, data_type: &str, secs: i64) -> RunScore {
        RunScore::new(name, value, data_type, "app", "user-1", at(secs))
    }

    fn ctx(scores: Vec<RunScore>) -> ExportContext {
        ExportContext::new(scores)
    }

    /// The run `e-1`: one turn `Where is order 7?` → `It ships today.` with a `lookup_order`
    /// call `{"id": 7}`, started at `at(1_000)` in session `s-1`, trace `t-1`.
    fn order_run() -> ProductionRun {
        let mut r = run(
            "e-1",
            1_000,
            &[("Where is order 7?", "It ships today.", &["lookup_order"])],
        );
        r.session_id = "s-1".into();
        r.trace_id = Some("t-1".into());
        r.transcript.turns[0].calls[0].args = map(json!({"id": 7}));
        r
    }

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    fn kinds(c: &ExportedCase) -> Vec<&'static str> {
        c.case.expect.iter().map(ScorerSpec::kind).collect()
    }

    #[test]
    fn scores_decide_reference_or_judge() {
        let cases = [
            (vec![score("thumbs", json!(true), "bool", 1)], true),
            (vec![score("thumbs", json!(false), "bool", 1)], false),
            (vec![score("rating", json!(5), "number", 1)], true),
            (vec![score("rating", json!(4), "number", 1)], true),
            (vec![score("rating", json!(2), "number", 1)], false),
            (
                vec![score("quality", json!("helpful"), "category", 1)],
                false,
            ),
            (Vec::new(), false),
            (
                vec![
                    score("thumbs", json!(true), "bool", 1),
                    score("rating", json!(2), "number", 1),
                ],
                false,
            ),
            (
                vec![
                    score("thumbs", json!(true), "bool", 1),
                    score("quality", json!("helpful"), "category", 1),
                ],
                true,
            ),
        ];
        for (scores, positive) in cases {
            let label = format!("{scores:?}");
            let c = run_to_case(&order_run(), &[], &ctx(scores), &opts()).unwrap();
            assert_eq!(c.positive, positive, "{label}");
            if positive {
                assert_eq!(kinds(&c), ["reference", "tools"], "{label}");
                assert!(matches!(
                    &c.case.expect[0],
                    ScorerSpec::Reference { answer, min_score: 3, turn: None } if answer == "It ships today."
                ));
            } else {
                assert_eq!(kinds(&c), ["llm_judge"], "{label}");
                assert!(matches!(
                    &c.case.expect[0],
                    ScorerSpec::LlmJudge { rubric, min_score: 3 } if rubric == RUBRIC
                ));
            }
        }
    }

    #[test]
    fn min_good_moves_the_number_threshold() {
        let scores = vec![score("rating", json!(4), "number", 1)];
        let strict = opts().with_min_good(4.5);
        let c = run_to_case(&order_run(), &[], &ctx(scores), &strict).unwrap();
        assert!(!c.positive);
    }

    #[test]
    fn the_latest_score_of_a_name_wins_across_sources() {
        let mut later = score("thumbs", json!(true), "bool", 20);
        later.source = "console".into();
        let scores = vec![score("thumbs", json!(false), "bool", 10), later];
        let c = run_to_case(&order_run(), &[], &ctx(scores.clone()), &opts()).unwrap();
        assert!(c.positive);
        let reversed: Vec<RunScore> = scores.into_iter().rev().collect();
        let c = run_to_case(&order_run(), &[], &ctx(reversed), &opts()).unwrap();
        assert!(c.positive);
    }

    #[test]
    fn a_positive_run_with_an_empty_reply_is_judged() {
        let mut r = order_run();
        r.transcript.turns[0].reply = "  ".into();
        let scores = vec![score("thumbs", json!(true), "bool", 1)];
        let c = run_to_case(&r, &[], &ctx(scores), &opts()).unwrap();
        assert!(!c.positive);
        assert_eq!(kinds(&c), ["llm_judge"]);
    }

    #[test]
    fn a_judge_case_without_a_rubric_is_skipped() {
        let err = run_to_case(
            &order_run(),
            &[],
            &ExportContext::default(),
            &ExportOptions::default(),
        )
        .unwrap_err();
        assert_eq!(err, "needs a rubric");
        let blank = ExportOptions::default().with_rubric(" ");
        assert!(run_to_case(&order_run(), &[], &ExportContext::default(), &blank).is_err());
    }

    #[test]
    fn an_incomplete_run_is_skipped_with_its_notes() {
        let mut r = order_run();
        r.content_complete = false;
        r.notes = vec!["AGENT_RESPONSE content is NULL".into(), "x".into()];
        assert_eq!(
            run_to_case(&r, &[], &ExportContext::default(), &opts()).unwrap_err(),
            "incomplete content: AGENT_RESPONSE content is NULL; x"
        );
    }

    #[test]
    fn history_turns_come_first_and_incomplete_runs_are_named() {
        let first = run("e-0", 900, &[("Hi", "Hello", &[])]);
        let mut broken = run("e-b", 950, &[("lost", "", &[])]);
        broken.content_complete = false;
        broken.notes = vec!["no AGENT_RESPONSE".into()];
        let history = [first, broken];
        let c = run_to_case(&order_run(), &history, &ExportContext::default(), &opts()).unwrap();
        assert_eq!(c.case.id, "inv-e-1");
        assert!(c.case.state.is_empty());
        assert_eq!(c.case.turns, ["Hi", "Where is order 7?"]);
        assert!(
            c.comments
                .contains(&"history run e-b left out: no AGENT_RESPONSE".to_owned()),
            "{:?}",
            c.comments
        );
        let off = opts().with_history(false);
        let c = run_to_case(&order_run(), &history, &ExportContext::default(), &off).unwrap();
        assert_eq!(c.case.turns, ["Where is order 7?"]);
        assert!(!c.comments.iter().any(|l| l.starts_with("history run")));
    }

    #[test]
    fn tools_keep_names_unless_args_are_asked_for() {
        let mut r = order_run();
        r.transcript.turns[0].calls.push(ToolCall {
            name: "issue_credit".into(),
            args: map(json!({"order": "7"})),
        });
        let up = || ctx(vec![score("thumbs", json!(true), "bool", 1)]);
        let c = run_to_case(&r, &[], &up(), &opts()).unwrap();
        match &c.case.expect[1] {
            ScorerSpec::Tools {
                calls,
                ordered: true,
                exact: false,
            } => {
                let names: Vec<(&str, bool)> = calls
                    .iter()
                    .map(|c| (c.name.as_str(), c.args.is_none()))
                    .collect();
                assert_eq!(names, [("lookup_order", true), ("issue_credit", true)]);
            }
            other => panic!("{other:?}"),
        }
        let c = run_to_case(&r, &[], &up(), &opts().with_tool_args(true)).unwrap();
        match &c.case.expect[1] {
            ScorerSpec::Tools { calls, .. } => {
                assert_eq!(
                    calls[0].args.clone().map(Value::Object),
                    Some(json!({"id": 7}))
                );
            }
            other => panic!("{other:?}"),
        }
        r.tools_complete = false;
        assert_eq!(
            kinds(&run_to_case(&r, &[], &up(), &opts()).unwrap()),
            ["reference"]
        );
        let no_calls = run("e-2", 1, &[("q", "a", &[])]);
        assert_eq!(
            kinds(&run_to_case(&no_calls, &[], &up(), &opts()).unwrap()),
            ["reference"]
        );
    }

    #[test]
    fn comments_name_the_run_its_prompt_scores_and_reply() {
        let c = ctx(vec![
            score("thumbs", json!(true), "bool", 1),
            score("quality", json!("helpful"), "category", 2).with_comment("clear"),
        ])
        .with_prompt("e2e-assistant", 1);
        let got = run_to_case(&order_run(), &[], &c, &opts()).unwrap();
        assert_eq!(
            got.comments,
            [
                "trace t-1",
                "session s-1",
                "invocation e-1",
                "started 1970-01-01T00:16:40Z",
                "agent agent",
                "prompt e2e-assistant@1",
                "score quality = helpful (source app, author user-1): clear",
                "score thumbs = true (source app, author user-1)",
                "reply It ships today.",
            ]
        );
        let mut r = order_run();
        r.trace_id = None;
        let got = run_to_case(&r, &[], &ExportContext::default(), &opts()).unwrap();
        assert_eq!(got.comments[0], "trace none");
    }

    #[test]
    fn long_replies_are_cut_and_comments_are_written_on_one_line() {
        let mut r = order_run();
        r.transcript.turns[0].reply = format!("line one\r\nline\ttwo\u{7f} {}", "é".repeat(3_000));
        let c = run_to_case(&r, &[], &ExportContext::default(), &opts()).unwrap();
        let reply = c.comments.last().unwrap();
        assert_eq!(reply.chars().count(), "reply ".len() + 2_000 + 1);
        assert!(reply.ends_with("é…"), "{reply}");
        let set = EvalSet {
            name: "s".into(),
            judge_rubric: None,
            cases: vec![c.case.clone()],
        };
        let yaml = set.to_yaml_with_comments(&|_| c.comments.clone());
        assert!(
            yaml.contains("\n  # reply line one  line two  éé"),
            "{yaml}"
        );
        let back = EvalSet::from_yaml(&yaml).unwrap();
        assert_eq!(back.cases[0].id, "inv-e-1");
    }

    #[test]
    fn score_filters_parse() {
        let ok = [
            (
                "thumbs=true",
                ScoreFilter::Equals {
                    name: "thumbs".into(),
                    value: "true".into(),
                },
            ),
            (
                "quality=very helpful",
                ScoreFilter::Equals {
                    name: "quality".into(),
                    value: "very helpful".into(),
                },
            ),
            (
                "rating>=4",
                ScoreFilter::Compare {
                    name: "rating".into(),
                    op: CompareOp::Ge,
                    value: 4.0,
                },
            ),
            (
                "rating<=2.5",
                ScoreFilter::Compare {
                    name: "rating".into(),
                    op: CompareOp::Le,
                    value: 2.5,
                },
            ),
            (
                "a.b-c_1>-1",
                ScoreFilter::Compare {
                    name: "a.b-c_1".into(),
                    op: CompareOp::Gt,
                    value: -1.0,
                },
            ),
            (
                "rating<3",
                ScoreFilter::Compare {
                    name: "rating".into(),
                    op: CompareOp::Lt,
                    value: 3.0,
                },
            ),
        ];
        for (text, want) in ok {
            assert_eq!(ScoreFilter::parse(text), Ok(want), "{text}");
        }
        for bad in [
            "",
            "=x",
            "rating",
            "rating=",
            "rating>=x",
            "rating>inf",
            "ra ting=1",
            "r!=1",
        ] {
            let err = ScoreFilter::parse(bad).unwrap_err();
            assert!(err.starts_with("invalid score filter"), "{bad}: {err}");
        }
    }

    #[test]
    fn constructors_fill_every_field() {
        let s = RunScore::new("n", json!(1), "number", "console", "chris", at(5)).with_comment("c");
        assert_eq!(
            (
                s.name.as_str(),
                s.data_type.as_str(),
                s.source.as_str(),
                s.author.as_str()
            ),
            ("n", "number", "console", "chris")
        );
        assert_eq!((s.comment.as_deref(), s.at), (Some("c"), at(5)));
        let t = TraceInvocation::new("t", "e", at(1));
        assert_eq!(
            (t.trace_id.as_str(), t.invocation_id.as_str(), t.started_at),
            ("t", "e", at(1))
        );
        let o = ExportOptions::default();
        assert_eq!(
            (o.rubric.clone(), o.min_good, o.history, o.tool_args),
            (None, 4.0, true, false)
        );
        assert_eq!(
            ExportContext::default().with_prompt("p", 2).prompt,
            Some(("p".into(), 2))
        );
        let case = run_to_case(&order_run(), &[], &ExportContext::default(), &opts())
            .unwrap()
            .case;
        let e = ExportedCase::new(case, vec!["c".into()], true);
        assert_eq!(
            (e.case.id.as_str(), e.comments.len(), e.positive),
            ("inv-e-1", 1, true)
        );
    }
}
