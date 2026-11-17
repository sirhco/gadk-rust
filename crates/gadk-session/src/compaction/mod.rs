// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Context compaction: summarize older session events so an agent's prompt stays small as its
//! conversation grows.
//!
//! A compaction never modifies or deletes history. Summarizing a range of events appends one new
//! [`Event`](crate::Event) carrying an [`EventCompaction`](crate::EventCompaction) that records the
//! covered timestamp range and the summary content. When the next prompt is built, the raw events
//! inside that range are dropped and the summary is materialized in their place.
//!
//! Two strategies are available (see [`Config`]):
//! - **sliding window** (`compaction_interval`, `overlap_size`) runs after an invocation completes
//!   and replaces each group of invocations with one summary; summaries are never re-summarized,
//!   so the prompt still grows with the conversation, at a reduced rate;
//! - **tail retention** (`token_threshold`, `event_retention_size`) runs before a model call and
//!   summarizes everything but the most recent events once the prompt grows past a token budget.
//!   Each new summary is seeded with the previous one, which is what bounds the prompt.
//!
//! Compaction is enabled per runner, through `RunnerConfig::compaction` in `gadk-engine`. A `None`
//! config disables it.

mod config;
mod error;
mod go_fmt;
pub(crate) mod llm_summarizer;
mod summarizer;
mod util;

pub use config::{CompactionConfigError, Config};
pub use error::{CompactionError, ERR_COMPACTION};
pub use llm_summarizer::{CONVERSATION_HISTORY_PLACEHOLDER, LlmSummarizer, LlmSummarizerConfig};
pub use summarizer::{SummarizeFailure, SummarizeResult, Summarizer};
#[doc(hidden)]
pub use summarizer::{bare_type_name, failure_usage};
#[doc(hidden)]
pub use util::{content, is_prose_part};
