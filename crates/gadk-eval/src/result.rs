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

//! What a run records and what a scorer returns.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A function call the agent made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The tool name.
    pub name: String,
    /// The call arguments.
    pub args: Map<String, Value>,
}

/// One user turn and the agent's answer to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    /// The user message.
    pub user: String,
    /// The agent's reply text.
    pub reply: String,
    /// The function calls made during the turn, in order.
    pub calls: Vec<ToolCall>,
}

/// One run of a case: the turns completed and the run error, if any.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunTranscript {
    /// The turns, in order (fewer than the case's turns when the run failed).
    pub turns: Vec<TurnRecord>,
    /// Why the run failed (build error, agent error, timeout); `None` when it completed.
    pub error: Option<String>,
}

impl RunTranscript {
    /// The turn `turn` (1-based) or, for `None`, the last turn; `None` if there is no such turn.
    pub fn turn(&self, turn: Option<usize>) -> Option<&TurnRecord> {
        match turn {
            None => self.turns.last(),
            Some(n) => n.checked_sub(1).and_then(|i| self.turns.get(i)),
        }
    }

    /// The reply of [`RunTranscript::turn`], or `""` when there is no such turn.
    pub fn reply(&self, turn: Option<usize>) -> &str {
        self.turn(turn).map_or("", |t| t.reply.as_str())
    }

    /// Every function call of every turn, in order.
    pub fn all_calls(&self) -> Vec<&ToolCall> {
        self.turns.iter().flat_map(|t| t.calls.iter()).collect()
    }
}

/// A scorer's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The check held.
    Pass,
    /// The check did not hold.
    Fail,
    /// The check could not be made (run error, judge error or unparseable judge reply).
    Error,
}

/// One scorer's result for one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreResult {
    /// The scorer kind ([`crate::ScorerSpec::kind`]).
    pub kind: String,
    /// The verdict.
    pub outcome: Outcome,
    /// The judge's 1–5 score (judge scorers only).
    pub score: Option<u8>,
    /// Why.
    pub rationale: String,
}

impl ScoreResult {
    /// A pass or fail without a score.
    pub fn check(kind: &str, pass: bool, rationale: impl Into<String>) -> Self {
        ScoreResult {
            kind: kind.to_owned(),
            outcome: if pass { Outcome::Pass } else { Outcome::Fail },
            score: None,
            rationale: rationale.into(),
        }
    }

    /// A judged pass or fail with its score.
    pub fn judged(kind: &str, pass: bool, score: u8, rationale: impl Into<String>) -> Self {
        ScoreResult {
            score: Some(score),
            ..ScoreResult::check(kind, pass, rationale)
        }
    }

    /// An error.
    pub fn error(kind: &str, rationale: impl Into<String>) -> Self {
        ScoreResult {
            kind: kind.to_owned(),
            outcome: Outcome::Error,
            score: None,
            rationale: rationale.into(),
        }
    }
}

/// `s` cut to at most `max` bytes on a char boundary.
pub(crate) fn excerpt(s: &str, max: usize) -> &str {
    &s[..s.floor_char_boundary(max.min(s.len()))]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(reply: &str, calls: &[&str]) -> TurnRecord {
        TurnRecord {
            user: "u".into(),
            reply: reply.into(),
            calls: calls
                .iter()
                .map(|n| ToolCall {
                    name: (*n).into(),
                    args: Map::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn turn_selection_and_calls() {
        let t = RunTranscript {
            turns: vec![turn("one", &["a"]), turn("two", &["b", "c"])],
            error: None,
        };
        assert_eq!(t.reply(None), "two");
        assert_eq!(t.reply(Some(1)), "one");
        assert_eq!(t.reply(Some(3)), "");
        assert_eq!(t.reply(Some(0)), "");
        let names: Vec<_> = t.all_calls().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn excerpt_respects_char_boundaries() {
        assert_eq!(excerpt("héllo", 2), "h");
        assert_eq!(excerpt("abc", 10), "abc");
    }

    #[test]
    fn outcome_serializes_lowercase() {
        let r = ScoreResult::judged("llm_judge", true, 4, "ok");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["outcome"], "pass");
        assert_eq!(v["score"], 4);
    }
}
