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

//! Writing eval sets as YAML ([`EvalSet::to_yaml`]).
//!
//! Every string is written double-quoted: its JSON form, with the characters YAML does not allow
//! unescaped (U+007F, U+0080 to U+009F, U+FFFE and U+FFFF) written as `\uXXXX`. `state` and tool
//! `args` are JSON flow mappings, and each scorer is a one-key flow mapping in the forms the
//! eval-set syntax documents.

use serde_json::{Map, Value};

use crate::spec::{EvalCase, EvalSet, ScorerSpec, ToolExpect};

/// The indentation of the case items [`EvalSet::to_yaml`] writes.
const CASE_INDENT: usize = 2;

impl EvalSet {
    /// The set as YAML that [`EvalSet::from_yaml`] parses back to the same set. An `llm_judge`
    /// whose rubric is the set's `judge_rubric` is written as `llm_judge: {}` (with its
    /// `min_score` when that is not 3).
    pub fn to_yaml(&self) -> String {
        self.to_yaml_with_comments(&|_| Vec::new())
    }

    /// Like [`EvalSet::to_yaml`], with the lines `comments` returns for a case written above it
    /// as `# ` comments (line breaks and other control characters become spaces).
    pub fn to_yaml_with_comments(&self, comments: &dyn Fn(&EvalCase) -> Vec<String>) -> String {
        let mut out = format!("name: {}\n", yaml_str(&self.name));
        if let Some(rubric) = &self.judge_rubric {
            out.push_str(&format!("judge_rubric: {}\n", yaml_str(rubric)));
        }
        out.push_str("cases:\n");
        for case in &self.cases {
            push_case(
                &mut out,
                case,
                CASE_INDENT,
                self.judge_rubric.as_deref(),
                &comments(case),
            );
        }
        out
    }
}

/// Writes `case` as a list item at `indent` spaces, after its comment lines.
fn push_case(
    out: &mut String,
    case: &EvalCase,
    indent: usize,
    set_rubric: Option<&str>,
    comments: &[String],
) {
    let pad = " ".repeat(indent);
    for c in comments {
        out.push_str(format!("{pad}# {}", comment_text(c)).trim_end());
        out.push('\n');
    }
    out.push_str(&format!("{pad}- id: {}\n", yaml_str(&case.id)));
    if !case.state.is_empty() {
        out.push_str(&format!("{pad}  state: {}\n", flow_map(&case.state)));
    }
    out.push_str(&format!("{pad}  turns:\n"));
    for turn in &case.turns {
        out.push_str(&format!("{pad}    - {}\n", yaml_str(turn)));
    }
    out.push_str(&format!("{pad}  expect:\n"));
    for scorer in &case.expect {
        out.push_str(&format!("{pad}    - {}\n", scorer_yaml(scorer, set_rubric)));
    }
}

/// One scorer as a one-key flow mapping.
fn scorer_yaml(scorer: &ScorerSpec, set_rubric: Option<&str>) -> String {
    match scorer {
        ScorerSpec::Contains {
            text,
            ignore_case,
            turn,
        } => text_scorer("contains", "text", text, *ignore_case, *turn),
        ScorerSpec::NotContains {
            text,
            ignore_case,
            turn,
        } => text_scorer("not_contains", "text", text, *ignore_case, *turn),
        ScorerSpec::Regex { pattern, turn } => {
            text_scorer("regex", "pattern", pattern.as_str(), false, *turn)
        }
        ScorerSpec::Equals { text, turn } => text_scorer("equals", "text", text, false, *turn),
        ScorerSpec::Tools {
            calls,
            ordered,
            exact,
        } => {
            let calls: Vec<String> = calls.iter().map(tool_call).collect();
            format!(
                "tools: {{calls: [{}], ordered: {ordered}, exact: {exact}}}",
                calls.join(", ")
            )
        }
        ScorerSpec::Reference {
            answer,
            min_score,
            turn,
        } => {
            let mut fields = vec![
                format!("answer: {}", yaml_str(answer)),
                format!("min_score: {min_score}"),
            ];
            if let Some(turn) = turn {
                fields.push(format!("turn: {turn}"));
            }
            format!("reference: {{{}}}", fields.join(", "))
        }
        ScorerSpec::LlmJudge { rubric, min_score } => {
            if set_rubric == Some(rubric.as_str()) {
                if *min_score == 3 {
                    "llm_judge: {}".to_owned()
                } else {
                    format!("llm_judge: {{min_score: {min_score}}}")
                }
            } else {
                format!(
                    "llm_judge: {{rubric: {}, min_score: {min_score}}}",
                    yaml_str(rubric)
                )
            }
        }
    }
}

/// `kind: {key: "text", ignore_case: true, turn: n}`, leaving out a false `ignore_case` and a
/// missing `turn`.
fn text_scorer(
    kind: &str,
    key: &str,
    text: &str,
    ignore_case: bool,
    turn: Option<usize>,
) -> String {
    let mut fields = vec![format!("{key}: {}", yaml_str(text))];
    if ignore_case {
        fields.push("ignore_case: true".to_owned());
    }
    if let Some(turn) = turn {
        fields.push(format!("turn: {turn}"));
    }
    format!("{kind}: {{{}}}", fields.join(", "))
}

/// A tool call: its quoted name, or `{name: "…", args: {…}}`.
fn tool_call(call: &ToolExpect) -> String {
    match &call.args {
        None => yaml_str(&call.name),
        Some(args) => format!(
            "{{name: {}, args: {}}}",
            yaml_str(&call.name),
            flow_map(args)
        ),
    }
}

/// A JSON object as a YAML flow mapping.
fn flow_map(map: &Map<String, Value>) -> String {
    let entries: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}: {}", yaml_str(k), flow_value(v)))
        .collect();
    format!("{{{}}}", entries.join(", "))
}

/// A JSON value in YAML flow style.
fn flow_value(v: &Value) -> String {
    match v {
        Value::String(s) => yaml_str(s),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(flow_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(map) => flow_map(map),
        other => other.to_string(),
    }
}

/// `s` as a YAML double-quoted scalar: its JSON form with the characters YAML does not allow
/// unescaped written as `\uXXXX`.
fn yaml_str(s: &str) -> String {
    let json = Value::String(s.to_owned()).to_string();
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if matches!(c, '\u{7f}'..='\u{9f}' | '\u{fffe}' | '\u{ffff}') {
            out.push_str(&format!("\\u{:04X}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` for a `# ` comment line: control characters (line breaks and tabs included), U+2028,
/// U+2029, U+FEFF, U+FFFE and U+FFFF become spaces.
fn comment_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '\u{2028}' | '\u{2029}' | '\u{feff}' | '\u{fffe}' | '\u{ffff}'
                )
            {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serde_json::{Map, Value, json};

    use super::*;

    /// Quotes, a backslash, line breaks, a tab, control characters, U+007F, C1 controls,
    /// noncharacters, the Unicode line and paragraph separators and an emoji.
    const NASTY: &str = "q\"uote b\\ack n\nl r\rx t\tab c\u{0}\u{1}\u{1f} del\u{7f} \
                         c1\u{80}\u{85}\u{9f} nc\u{fffe}\u{ffff} ls\u{2028}ps\u{2029} emoji \u{1F600}";

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    /// A scorer as comparable JSON: its kind and every field (a regex as its pattern text).
    fn scorer_json(s: &ScorerSpec) -> Value {
        match s {
            ScorerSpec::Contains {
                text,
                ignore_case,
                turn,
            } => json!({"contains": [text, ignore_case, turn]}),
            ScorerSpec::NotContains {
                text,
                ignore_case,
                turn,
            } => json!({"not_contains": [text, ignore_case, turn]}),
            ScorerSpec::Regex { pattern, turn } => json!({"regex": [pattern.as_str(), turn]}),
            ScorerSpec::Equals { text, turn } => json!({"equals": [text, turn]}),
            ScorerSpec::Tools {
                calls,
                ordered,
                exact,
            } => {
                let calls: Vec<Value> = calls
                    .iter()
                    .map(|c| json!([c.name, c.args.clone().map(Value::Object)]))
                    .collect();
                json!({"tools": [calls, ordered, exact]})
            }
            ScorerSpec::Reference {
                answer,
                min_score,
                turn,
            } => json!({"reference": [answer, min_score, turn]}),
            ScorerSpec::LlmJudge { rubric, min_score } => {
                json!({"llm_judge": [rubric, min_score]})
            }
        }
    }

    /// Asserts `got` equals `want` field by field: name, rubric, and each case's id, state,
    /// turns and scorers.
    fn assert_same(got: &EvalSet, want: &EvalSet) {
        assert_eq!(got.name, want.name);
        assert_eq!(got.judge_rubric, want.judge_rubric);
        assert_eq!(got.cases.len(), want.cases.len());
        for (g, w) in got.cases.iter().zip(&want.cases) {
            assert_eq!(g.id, w.id);
            assert_eq!(g.state, w.state, "{}", w.id);
            assert_eq!(g.turns, w.turns, "{}", w.id);
            let g: Vec<Value> = g.expect.iter().map(scorer_json).collect();
            let w: Vec<Value> = w.expect.iter().map(scorer_json).collect();
            assert_eq!(g, w);
        }
    }

    fn case(id: &str, turns: &[&str], expect: Vec<ScorerSpec>) -> EvalCase {
        EvalCase {
            id: id.to_owned(),
            state: Map::new(),
            turns: turns.iter().map(|t| (*t).to_owned()).collect(),
            expect,
        }
    }

    /// A set with every scorer kind, `NASTY` in every string, state, and judges with and
    /// without the set's rubric.
    fn every_scorer() -> EvalSet {
        let set_rubric = format!("set {NASTY}");
        let mut first = case(
            NASTY,
            &[NASTY, "second"],
            vec![
                ScorerSpec::Contains {
                    text: NASTY.into(),
                    ignore_case: true,
                    turn: Some(2),
                },
                ScorerSpec::Contains {
                    text: "plain".into(),
                    ignore_case: false,
                    turn: None,
                },
                ScorerSpec::NotContains {
                    text: NASTY.into(),
                    ignore_case: false,
                    turn: Some(1),
                },
                ScorerSpec::Regex {
                    pattern: Regex::new(r#"(?i)a\d+"\\n"#).unwrap(),
                    turn: Some(1),
                },
                ScorerSpec::Equals {
                    text: NASTY.into(),
                    turn: None,
                },
                ScorerSpec::Tools {
                    calls: vec![
                        ToolExpect {
                            name: "lookup".into(),
                            args: None,
                        },
                        ToolExpect {
                            name: NASTY.into(),
                            args: Some(map(
                                json!({"order": "123", "n": 7, "f": 2.5, "deep": {"k": [1, NASTY, null, true]}}),
                            )),
                        },
                    ],
                    ordered: true,
                    exact: false,
                },
                ScorerSpec::Tools {
                    calls: vec![ToolExpect {
                        name: "x".into(),
                        args: None,
                    }],
                    ordered: false,
                    exact: true,
                },
                ScorerSpec::Reference {
                    answer: NASTY.into(),
                    min_score: 4,
                    turn: Some(1),
                },
                ScorerSpec::Reference {
                    answer: "a".into(),
                    min_score: 3,
                    turn: None,
                },
                ScorerSpec::LlmJudge {
                    rubric: set_rubric.clone(),
                    min_score: 3,
                },
                ScorerSpec::LlmJudge {
                    rubric: set_rubric.clone(),
                    min_score: 5,
                },
                ScorerSpec::LlmJudge {
                    rubric: format!("own {NASTY}"),
                    min_score: 2,
                },
            ],
        );
        first.state =
            map(json!({NASTY: {"n": 1, "f": 2.5, "list": [true, null, NASTY], "empty": {}}}));
        EvalSet {
            name: NASTY.into(),
            judge_rubric: Some(set_rubric),
            cases: vec![
                first,
                case(
                    "second",
                    &["hi"],
                    vec![ScorerSpec::Equals {
                        text: "ok".into(),
                        turn: None,
                    }],
                ),
            ],
        }
    }

    #[test]
    fn to_yaml_round_trips_every_scorer_and_character() {
        let set = every_scorer();
        let yaml = set.to_yaml();
        assert_same(&EvalSet::from_yaml(&yaml).unwrap(), &set);
        assert!(yaml.contains("\n      - llm_judge: {}\n"), "{yaml}");
        assert!(
            yaml.contains("\n      - llm_judge: {min_score: 5}\n"),
            "{yaml}"
        );
        assert!(
            yaml.contains("del\\u007F c1\\u0080\\u0085\\u009F"),
            "{yaml}"
        );
        assert!(yaml.contains("\\uFFFE\\uFFFF"), "{yaml}");
        for c in ['\u{7f}', '\u{85}', '\u{fffe}', '\r', '\t'] {
            assert!(!yaml.contains(c), "{c:?} is escaped: {yaml}");
        }
        assert!(
            yaml.starts_with("name: \"q\\\"uote b\\\\ack n\\nl r\\rx t\\tab c\\u0000"),
            "{yaml}"
        );
    }

    #[test]
    fn comments_are_written_above_each_case_on_one_line() {
        let set = every_scorer();
        let yaml = set.to_yaml_with_comments(&|c| {
            vec![
                format!("case {}", c.id),
                "line\r\nbreak\tand\u{2028}more\u{7f}".to_owned(),
            ]
        });
        assert_same(&EvalSet::from_yaml(&yaml).unwrap(), &set);
        assert!(
            yaml.contains("\n  # case second\n  # line  break and more\n  - id: \"second\"\n"),
            "{yaml}"
        );
        for line in yaml.lines().filter(|l| l.trim_start().starts_with('#')) {
            assert!(line.starts_with("  # "), "{line:?}");
            assert!(!line.chars().any(char::is_control), "{line:?}");
        }
    }
}
