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

//! Offline evaluation of gadk agents.
//!
//! An [`EvalSet`] (YAML) holds test cases: user turns plus the scorers that grade the agent's
//! replies. An [`Evaluator`] runs every case against an agent built once per [`Variant`] —
//! typically one per prompt-registry `Selector` — and grades each run with deterministic checks,
//! tool-trajectory checks and an LLM judge, returning an [`EvalReport`].

mod error;
mod evaluator;
mod metrics;
mod report;
mod result;
mod run;
mod score;
mod set;
mod spec;
mod variant;
mod write;

pub use error::EvalError;
pub use evaluator::Evaluator;
pub use metrics::{MeasuredReport, RunMetrics, TokenCounts};
pub use report::{CaseRun, EvalReport, VariantReport};
pub use result::{Outcome, RunTranscript, ScoreResult, ToolCall, TurnRecord};
pub use score::score_transcript;
pub use spec::{EvalCase, EvalSet, ScorerSpec, ToolExpect};
pub use variant::Variant;
pub use write::append_cases;
