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

//! The eval-set model: cases and their scorers. Parsing and validation live in `set.rs`.

use regex::Regex;
use serde_json::{Map, Value};

/// A validated eval set.
#[derive(Debug, Clone)]
pub struct EvalSet {
    /// The set's name.
    pub name: String,
    /// The default rubric for `llm_judge` scorers without their own (already applied to them).
    pub judge_rubric: Option<String>,
    /// The cases, in file order; ids are unique.
    pub cases: Vec<EvalCase>,
}

/// One test case: a conversation and the scorers that grade it.
#[derive(Debug, Clone)]
pub struct EvalCase {
    /// Unique, non-empty id.
    pub id: String,
    /// The initial session state.
    pub state: Map<String, Value>,
    /// The user turns, sent in order in one session (at least one).
    pub turns: Vec<String>,
    /// The scorers (at least one).
    pub expect: Vec<ScorerSpec>,
}

/// One scorer of a case. `turn` is 1-based; `None` grades the last turn.
#[derive(Debug, Clone)]
pub enum ScorerSpec {
    /// The reply contains `text`.
    Contains {
        text: String,
        ignore_case: bool,
        turn: Option<usize>,
    },
    /// The reply does not contain `text`.
    NotContains {
        text: String,
        ignore_case: bool,
        turn: Option<usize>,
    },
    /// The reply matches `pattern` anywhere.
    Regex { pattern: Regex, turn: Option<usize> },
    /// The reply equals `text` after trimming whitespace on both.
    Equals { text: String, turn: Option<usize> },
    /// The tool calls of all turns satisfy `calls`.
    Tools {
        calls: Vec<ToolExpect>,
        ordered: bool,
        exact: bool,
    },
    /// The judge finds the reply consistent with the reference `answer`.
    Reference {
        answer: String,
        min_score: u8,
        turn: Option<usize>,
    },
    /// The judge grades the whole conversation against `rubric`.
    LlmJudge { rubric: String, min_score: u8 },
}

impl ScorerSpec {
    /// The scorer kind as written in YAML: `contains`, `not_contains`, `regex`, `equals`,
    /// `tools`, `reference` or `llm_judge`.
    pub fn kind(&self) -> &'static str {
        match self {
            ScorerSpec::Contains { .. } => "contains",
            ScorerSpec::NotContains { .. } => "not_contains",
            ScorerSpec::Regex { .. } => "regex",
            ScorerSpec::Equals { .. } => "equals",
            ScorerSpec::Tools { .. } => "tools",
            ScorerSpec::Reference { .. } => "reference",
            ScorerSpec::LlmJudge { .. } => "llm_judge",
        }
    }
}

/// One expected tool call: its name and, optionally, a subset of its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExpect {
    /// The tool name.
    pub name: String,
    /// Keys the call's args must contain with equal JSON values.
    pub args: Option<Map<String, Value>>,
}
