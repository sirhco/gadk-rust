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

//! Loading and validating eval sets from YAML.

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::EvalError;
use crate::spec::{EvalCase, EvalSet, ScorerSpec, ToolExpect};

const SCORER_KINDS: &str = "contains, not_contains, regex, equals, tools, reference, llm_judge";

impl EvalSet {
    /// Reads and validates the eval set at `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, EvalError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| EvalError::Io(format!("reading {}: {e}", path.display())))?;
        Self::from_yaml(&text)
    }

    /// Parses and validates an eval set from YAML text.
    pub fn from_yaml(yaml: &str) -> Result<Self, EvalError> {
        let doc: Value = serde_saphyr::from_str(yaml)
            .map_err(|e| EvalError::load(None, format!("invalid YAML: {e}")))?;
        parse_set(&doc)
    }
}

fn parse_set(doc: &Value) -> Result<EvalSet, EvalError> {
    let set_err = |m: String| EvalError::load(None, m);
    let top = doc
        .as_object()
        .ok_or_else(|| set_err("the document must be a mapping".into()))?;
    check_keys(top, &["name", "judge_rubric", "cases"]).map_err(set_err)?;
    let name = match top.get("name") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        _ => return Err(set_err("`name` must be a non-empty string".into())),
    };
    let judge_rubric = match top.get("judge_rubric") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(_) => return Err(set_err("`judge_rubric` must be a non-empty string".into())),
    };
    let cases_v = match top.get("cases") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Err(set_err("`cases` must be a non-empty list".into())),
    };
    let mut seen = HashSet::new();
    let mut cases = Vec::with_capacity(cases_v.len());
    for (i, c) in cases_v.iter().enumerate() {
        let case = parse_case(c, i, judge_rubric.as_deref())?;
        if !seen.insert(case.id.clone()) {
            return Err(EvalError::load(Some(&case.id), "duplicate case id"));
        }
        cases.push(case);
    }
    Ok(EvalSet {
        name,
        judge_rubric,
        cases,
    })
}

fn parse_case(
    v: &Value,
    index: usize,
    default_rubric: Option<&str>,
) -> Result<EvalCase, EvalError> {
    let usable_id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let label = usable_id.map_or_else(|| format!("#{}", index + 1), str::to_owned);
    let err = |m: String| EvalError::load(Some(&label), m);
    let m = v
        .as_object()
        .ok_or_else(|| err("a case must be a mapping".into()))?;
    check_keys(m, &["id", "state", "turns", "expect"]).map_err(err)?;
    let id = usable_id
        .ok_or_else(|| err("`id` must be a non-empty string".into()))?
        .to_owned();
    let state = match m.get("state") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err(err("`state` must be a mapping".into())),
    };
    let turns = match m.get("turns") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|t| t.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| err("every turn must be a string".into()))?,
        _ => return Err(err("`turns` must be a non-empty list of strings".into())),
    };
    let expect_v = match m.get("expect") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Err(err("`expect` must be a non-empty list of scorers".into())),
    };
    let expect = expect_v
        .iter()
        .enumerate()
        .map(|(j, s)| {
            parse_scorer(s, turns.len(), default_rubric)
                .map_err(|m| err(format!("expect[{}]: {m}", j + 1)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EvalCase {
        id,
        state,
        turns,
        expect,
    })
}

fn check_keys(m: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "unknown key `{k}` (expected one of: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

/// A scorer's main string and its option mapping (`None` for the scalar shorthand).
type MainAndOptions<'a> = (String, Option<&'a Map<String, Value>>);

/// A scorer body that is either the scalar shorthand for `main` or a mapping of options.
fn main_and_options<'a>(
    kind: &str,
    body: &'a Value,
    main: &str,
    allowed: &[&str],
) -> Result<MainAndOptions<'a>, String> {
    match body {
        Value::String(s) => Ok((s.clone(), None)),
        Value::Object(o) => {
            check_keys(o, allowed).map_err(|e| format!("`{kind}`: {e}"))?;
            match o.get(main) {
                Some(Value::String(s)) => Ok((s.clone(), Some(o))),
                _ => Err(format!("`{kind}` needs a string `{main}`")),
            }
        }
        _ => Err(format!(
            "`{kind}` must be a string or a mapping with `{main}`"
        )),
    }
}

fn opt_bool(o: Option<&Map<String, Value>>, key: &str) -> Result<bool, String> {
    match o.and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

/// The optional 1-based `turn`, at most `max_turn` when given.
fn opt_turn(
    o: Option<&Map<String, Value>>,
    max_turn: Option<usize>,
) -> Result<Option<usize>, String> {
    match o.and_then(|o| o.get("turn")) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 1 && max_turn.is_none_or(|max| *n <= max));
            match (n, max_turn) {
                (Some(n), _) => Ok(Some(n)),
                (None, Some(max)) => Err(format!("`turn` must be an integer from 1 to {max}")),
                (None, None) => Err("`turn` must be a positive integer".to_owned()),
            }
        }
    }
}

fn min_score(o: Option<&Map<String, Value>>) -> Result<u8, String> {
    match o.and_then(|o| o.get("min_score")) {
        None | Some(Value::Null) => Ok(3),
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=5).contains(n))
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(|| "`min_score` must be an integer from 1 to 5".to_owned()),
    }
}

impl ScorerSpec {
    /// Parses one scorer written in the eval-set syntax (for example
    /// `{"contains": {"text": "30 days", "turn": 2}}`), outside an eval set: `turn` has no upper
    /// bound and `llm_judge` needs its own `rubric`.
    ///
    /// # Errors
    ///
    /// [`EvalError::Load`] with `case: None` and the same message an eval set reports for that
    /// scorer.
    pub fn from_value(v: &Value) -> Result<ScorerSpec, EvalError> {
        parse_scorer_with(v, None, None).map_err(|m| EvalError::load(None, m))
    }
}

/// A scorer of a case with `nturns` turns, falling back to the set's `default_rubric`.
fn parse_scorer(
    v: &Value,
    nturns: usize,
    default_rubric: Option<&str>,
) -> Result<ScorerSpec, String> {
    parse_scorer_with(v, Some(nturns), default_rubric)
}

/// A scorer whose `turn` is at most `max_turn` (any positive turn for `None`); `llm_judge`
/// without a `rubric` uses `default_rubric`.
fn parse_scorer_with(
    v: &Value,
    max_turn: Option<usize>,
    default_rubric: Option<&str>,
) -> Result<ScorerSpec, String> {
    let Some((kind, body)) = v
        .as_object()
        .filter(|m| m.len() == 1)
        .and_then(|m| m.iter().next())
    else {
        return Err("a scorer must be a single-key mapping like `contains: \"text\"`".into());
    };
    match kind.as_str() {
        "contains" | "not_contains" => {
            let (text, o) = main_and_options(kind, body, "text", &["text", "ignore_case", "turn"])?;
            let ignore_case = opt_bool(o, "ignore_case")?;
            let turn = opt_turn(o, max_turn)?;
            Ok(if kind == "contains" {
                ScorerSpec::Contains {
                    text,
                    ignore_case,
                    turn,
                }
            } else {
                ScorerSpec::NotContains {
                    text,
                    ignore_case,
                    turn,
                }
            })
        }
        "regex" => {
            let (pattern, o) = main_and_options(kind, body, "pattern", &["pattern", "turn"])?;
            let pattern = Regex::new(&pattern).map_err(|e| format!("invalid regex: {e}"))?;
            Ok(ScorerSpec::Regex {
                pattern,
                turn: opt_turn(o, max_turn)?,
            })
        }
        "equals" => {
            let (text, o) = main_and_options(kind, body, "text", &["text", "turn"])?;
            Ok(ScorerSpec::Equals {
                text,
                turn: opt_turn(o, max_turn)?,
            })
        }
        "tools" => parse_tools(body),
        "reference" => {
            let (answer, o) =
                main_and_options(kind, body, "answer", &["answer", "min_score", "turn"])?;
            Ok(ScorerSpec::Reference {
                answer,
                min_score: min_score(o)?,
                turn: opt_turn(o, max_turn)?,
            })
        }
        "llm_judge" => {
            let (rubric, o) = match body {
                Value::Null => (None, None),
                Value::String(s) => (Some(s.clone()), None),
                Value::Object(o) => {
                    check_keys(o, &["rubric", "min_score"])
                        .map_err(|e| format!("`llm_judge`: {e}"))?;
                    match o.get("rubric") {
                        None | Some(Value::Null) => (None, Some(o)),
                        Some(Value::String(s)) => (Some(s.clone()), Some(o)),
                        Some(_) => return Err("`rubric` must be a string".into()),
                    }
                }
                _ => return Err("`llm_judge` must be a string or a mapping".into()),
            };
            let missing = if max_turn.is_some() {
                "`llm_judge` needs a `rubric` (or a set-level `judge_rubric`)"
            } else {
                "`llm_judge` needs a `rubric`"
            };
            let rubric = rubric
                .filter(|r| !r.trim().is_empty())
                .or_else(|| default_rubric.map(str::to_owned))
                .ok_or(missing)?;
            Ok(ScorerSpec::LlmJudge {
                rubric,
                min_score: min_score(o)?,
            })
        }
        other => Err(format!(
            "unknown scorer `{other}` (expected one of: {SCORER_KINDS})"
        )),
    }
}

fn parse_tools(body: &Value) -> Result<ScorerSpec, String> {
    let (calls_v, o) = match body {
        Value::Array(a) => (a, None),
        Value::Object(o) => {
            check_keys(o, &["calls", "ordered", "exact"]).map_err(|e| format!("`tools`: {e}"))?;
            match o.get("calls") {
                Some(Value::Array(a)) => (a, Some(o)),
                _ => return Err("`tools` needs a list `calls`".into()),
            }
        }
        _ => return Err("`tools` must be a list of calls or a mapping with `calls`".into()),
    };
    if calls_v.is_empty() {
        return Err("`tools` needs at least one call".into());
    }
    let calls = calls_v
        .iter()
        .map(|c| match c {
            Value::String(name) if !name.is_empty() => Ok(ToolExpect {
                name: name.clone(),
                args: None,
            }),
            Value::Object(m) => {
                check_keys(m, &["name", "args"]).map_err(|e| format!("tool call: {e}"))?;
                let name = match m.get("name") {
                    Some(Value::String(n)) if !n.is_empty() => n.clone(),
                    _ => return Err("a tool call needs a non-empty `name`".to_owned()),
                };
                let args = match m.get("args") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(a)) => Some(a.clone()),
                    Some(_) => return Err(format!("tool call {name}: `args` must be a mapping")),
                };
                Ok(ToolExpect { name, args })
            }
            _ => Err("a tool call must be a name or a mapping with `name`".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ScorerSpec::Tools {
        calls,
        ordered: opt_bool(o, "ordered")?,
        exact: opt_bool(o, "exact")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The canonical example from the spec and README.
    const EXAMPLE: &str = r#"
name: support-bot
judge_rubric: "Polite, accurate, no invented policy."
cases:
  - id: refund-window
    state: { tier: gold }
    turns: ["I want a refund", "Order 123, bought 40 days ago"]
    expect:
      - contains: "30 days"
      - contains: { text: "sorry", ignore_case: true, turn: 2 }
      - not_contains: "guarantee"
      - regex: "(?i)store credit"
      - equals: { text: "OK", turn: 1 }
      - tools: [lookup_order]
      - tools: { calls: [lookup_order, { name: issue_credit, args: { order: "123" } }], ordered: true, exact: false }
      - reference: "Refunds only within 30 days; offer store credit."
      - llm_judge: { rubric: "Offers store credit", min_score: 4 }
      - llm_judge: {}
"#;

    fn load_err(yaml: &str) -> (Option<String>, String) {
        match EvalSet::from_yaml(yaml) {
            Err(EvalError::Load { case, message }) => (case, message),
            other => panic!("expected a load error, got {other:?}"),
        }
    }

    /// A one-case set whose single scorer is `scorer` (YAML flow mapping text).
    fn with_scorer(scorer: &str) -> String {
        format!("name: s\ncases:\n  - id: c\n    turns: [a, b]\n    expect:\n      - {scorer}\n")
    }

    #[test]
    fn parses_the_canonical_example() {
        let set = EvalSet::from_yaml(EXAMPLE).unwrap();
        assert_eq!(set.name, "support-bot");
        assert_eq!(
            set.judge_rubric.as_deref(),
            Some("Polite, accurate, no invented policy.")
        );
        let c = &set.cases[0];
        assert_eq!(c.id, "refund-window");
        assert_eq!(Value::Object(c.state.clone()), json!({"tier": "gold"}));
        assert_eq!(
            c.turns,
            ["I want a refund", "Order 123, bought 40 days ago"]
        );
        let kinds: Vec<_> = c.expect.iter().map(ScorerSpec::kind).collect();
        assert_eq!(
            kinds,
            [
                "contains",
                "contains",
                "not_contains",
                "regex",
                "equals",
                "tools",
                "tools",
                "reference",
                "llm_judge",
                "llm_judge"
            ]
        );
        assert!(
            matches!(&c.expect[0], ScorerSpec::Contains { text, ignore_case: false, turn: None } if text == "30 days")
        );
        assert!(
            matches!(&c.expect[1], ScorerSpec::Contains { text, ignore_case: true, turn: Some(2) } if text == "sorry")
        );
        assert!(
            matches!(&c.expect[3], ScorerSpec::Regex { pattern, turn: None } if pattern.as_str() == "(?i)store credit")
        );
        assert!(matches!(&c.expect[4], ScorerSpec::Equals { text, turn: Some(1) } if text == "OK"));
        match &c.expect[6] {
            ScorerSpec::Tools {
                calls,
                ordered,
                exact,
            } => {
                assert!(*ordered && !*exact);
                assert_eq!(
                    calls[0],
                    ToolExpect {
                        name: "lookup_order".into(),
                        args: None
                    }
                );
                assert_eq!(calls[1].name, "issue_credit");
                assert_eq!(
                    calls[1].args.clone().map(Value::Object),
                    Some(json!({"order": "123"}))
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &c.expect[7],
            ScorerSpec::Reference {
                min_score: 3,
                turn: None,
                ..
            }
        ));
        assert!(
            matches!(&c.expect[8], ScorerSpec::LlmJudge { rubric, min_score: 4 } if rubric == "Offers store credit")
        );
        assert!(
            matches!(&c.expect[9], ScorerSpec::LlmJudge { rubric, min_score: 3 } if rubric == "Polite, accurate, no invented policy.")
        );
    }

    #[test]
    fn load_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("set.yaml");
        std::fs::write(&p, EXAMPLE).unwrap();
        assert_eq!(EvalSet::load(&p).unwrap().cases.len(), 1);
        assert!(matches!(
            EvalSet::load(dir.path().join("missing.yaml")),
            Err(EvalError::Io(_))
        ));
    }

    #[test]
    fn set_level_errors_have_no_case() {
        for (yaml, needle) in [
            ("name: [", "invalid YAML"),
            ("cases: []", "`name`"),
            ("name: s", "`cases`"),
            ("name: s\ncases: []", "`cases`"),
            ("name: s\nextra: 1\ncases: []", "unknown key `extra`"),
        ] {
            let (case, msg) = load_err(yaml);
            assert_eq!(case, None, "{yaml}");
            assert!(msg.contains(needle), "{yaml}: {msg}");
        }
    }

    #[test]
    fn case_errors_name_the_case() {
        let cases = [
            (
                "name: s\ncases:\n  - turns: [a]\n    expect: [contains: x]",
                "#1",
                "`id`",
            ),
            (
                "name: s\ncases:\n  - id: ''\n    turns: [a]\n    expect: [contains: x]",
                "#1",
                "`id`",
            ),
            (
                "name: s\ncases:\n  - id: c\n    turns: []\n    expect: [contains: x]",
                "c",
                "`turns`",
            ),
            (
                "name: s\ncases:\n  - id: c\n    turns: [a]\n    expect: []",
                "c",
                "`expect`",
            ),
            (
                "name: s\ncases:\n  - id: c\n    turns: [a]\n    expect: [contains: x]\n  - id: c\n    turns: [a]\n    expect: [contains: x]",
                "c",
                "duplicate",
            ),
            (
                "name: s\ncases:\n  - id: c\n    turns: [a]\n    bogus: 1\n    expect: [contains: x]",
                "c",
                "unknown key `bogus`",
            ),
            (
                "name: s\ncases:\n  - id: c\n    state: [1]\n    turns: [a]\n    expect: [contains: x]",
                "c",
                "`state`",
            ),
        ];
        for (yaml, want_case, needle) in cases {
            let (case, msg) = load_err(yaml);
            assert_eq!(case.as_deref(), Some(want_case), "{yaml}");
            assert!(msg.contains(needle), "{yaml}: {msg}");
        }
    }

    #[test]
    fn scorer_errors_name_the_case_and_scorer() {
        let cases = [
            ("bogus: x", "unknown scorer `bogus`"),
            ("{ contains: x, regex: y }", "single-key"),
            ("contains: { text: x, colour: red }", "unknown key `colour`"),
            ("regex: \"(unclosed\"", "invalid regex"),
            (
                "contains: { text: x, turn: 3 }",
                "`turn` must be an integer from 1 to 2",
            ),
            ("contains: { text: x, turn: 0 }", "`turn`"),
            ("reference: { answer: x, min_score: 6 }", "`min_score`"),
            ("llm_judge: { rubric: x, min_score: 0 }", "`min_score`"),
            ("llm_judge: {}", "needs a `rubric`"),
            ("tools: []", "at least one call"),
            ("tools: { calls: [{ args: { a: 1 } }] }", "non-empty `name`"),
            ("contains: { ignore_case: true }", "string `text`"),
        ];
        for (scorer, needle) in cases {
            let (case, msg) = load_err(&with_scorer(scorer));
            assert_eq!(case.as_deref(), Some("c"), "{scorer}");
            assert!(msg.starts_with("expect[1]: "), "{scorer}: {msg}");
            assert!(msg.contains(needle), "{scorer}: {msg}");
        }
    }

    fn scorer(json: Value) -> Result<ScorerSpec, EvalError> {
        ScorerSpec::from_value(&json)
    }

    #[test]
    fn from_value_accepts_every_kind_without_a_turn_bound() {
        let ok = [
            (json!({"contains": "x"}), "contains"),
            (
                json!({"contains": {"text": "x", "ignore_case": true, "turn": 50}}),
                "contains",
            ),
            (json!({"not_contains": "x"}), "not_contains"),
            (json!({"regex": {"pattern": "a+", "turn": 2}}), "regex"),
            (json!({"equals": "x"}), "equals"),
            (
                json!({"tools": ["a", {"name": "b", "args": {"k": 1}}]}),
                "tools",
            ),
            (json!({"reference": "x"}), "reference"),
            (json!({"llm_judge": "be nice"}), "llm_judge"),
            (
                json!({"llm_judge": {"rubric": "be nice", "min_score": 4}}),
                "llm_judge",
            ),
        ];
        for (v, kind) in ok {
            assert_eq!(scorer(v.clone()).unwrap().kind(), kind, "{v}");
        }
        assert!(matches!(
            scorer(json!({"contains": {"text": "x", "turn": 50}})).unwrap(),
            ScorerSpec::Contains { turn: Some(50), .. }
        ));
    }

    #[test]
    fn from_value_errors_are_set_level_load_errors() {
        let cases = [
            (
                json!({"contains": {"text": "x", "turn": 0}}),
                "`turn` must be a positive integer",
            ),
            (
                json!({"contains": {"text": "x", "turn": "2"}}),
                "`turn` must be a positive integer",
            ),
            (json!({"llm_judge": {}}), "`llm_judge` needs a `rubric`"),
            (json!({"llm_judge": null}), "`llm_judge` needs a `rubric`"),
            (json!({"bogus": "x"}), "unknown scorer `bogus`"),
            (json!("contains"), "single-key"),
            (json!({"regex": "(unclosed"}), "invalid regex"),
        ];
        for (v, needle) in cases {
            match scorer(v.clone()) {
                Err(EvalError::Load {
                    case: None,
                    message,
                }) => {
                    assert!(message.contains(needle), "{v}: {message}")
                }
                other => panic!("{v}: expected a set-level load error, got {other:?}"),
            }
        }
        let Err(EvalError::Load { message, .. }) = scorer(json!({"llm_judge": {}})) else {
            panic!("expected an error");
        };
        assert!(!message.contains("judge_rubric"), "{message}");
    }
}
