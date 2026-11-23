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

//! Production eval monitors: grade real agent runs with gadk-eval's scorers.
//!
//! A [`TranscriptSource`] lists recent runs as [`ProductionRun`]s
//! ([`BigQueryTranscriptSource`] reads the agent-analytics table, [`CloudTraceTranscriptSource`]
//! the spans in Cloud Trace). A [`Monitor`] scores each run
//! against a [`MonitorSpec`] (YAML) and returns a [`MonitorReport`], which a [`ReportStore`]
//! keeps in a local directory or a `gs://` bucket. The `gadk-eval` binary ([`cli`]) runs all of
//! it.
//!
//! [`run_experiment`] runs one eval set against two or more variants of an agent (prompt
//! versions, models, or anything the `build` closure changes) and returns an
//! [`ExperimentReport`] that compares them run by run, with each run's duration and tokens. An
//! [`ExperimentSpec`] describes an experiment on one Gemini LLM agent in YAML, and
//! `gadk-eval experiment <spec.yaml> [--out <url>]` ([`cli`]) runs it without Rust code.
//!
//! ```no_run
//! # async fn demo() -> Result<(), gadk_eval_monitor::MonitorError> {
//! use gadk_eval_monitor::{
//!     BigQueryTranscriptSource, Monitor, MonitorSpec, ReportStore, vertex_judge,
//! };
//!
//! let spec = MonitorSpec::load("monitors/support.yaml")?;
//! let source = BigQueryTranscriptSource::new("my-proj", "agent_analytics", "agent_events")?;
//! let judge = match &spec.judge {
//!     Some(j) => Some(vertex_judge(j).await?),
//!     None => None,
//! };
//! let report = Monitor::new().run(&spec, &source, judge).await?;
//! print!("{}", report.summary());
//! ReportStore::open("gs://my-bucket/monitors")?.put(&report).await?;
//! # Ok(()) }
//! ```

mod error;
#[cfg(test)]
mod testutil;

pub mod cli;
pub mod experiment;
pub mod experiment_spec;
pub mod export;
pub mod gcp;
pub mod guard_spec;
pub mod judge;
pub mod monitor;
pub mod prices;
pub mod report;
pub mod source;
pub mod spec;
pub mod store;

pub use error::MonitorError;
pub use experiment::{
    CaseVerdicts, ExperimentReport, ExperimentVariant, VariantStats, Verdict, run_experiment,
};
pub use experiment_spec::{AgentInstruction, ExperimentAgent, ExperimentSpec};
pub use export::{
    CompareOp, ExportContext, ExportOptions, ExportSource, ExportedCase, RunScore, ScoreFilter,
    TraceInvocation, cases_text, export_runs, run_to_case, write_set_file,
};
pub use gcp::{AdcTokenSource, NoAuth, TokenSource};
pub use guard_spec::{Check, FeedbackSource, GuardSpec};
pub use judge::vertex_judge;
pub use monitor::{DEFAULT_CONCURRENCY, Monitor};
pub use prices::{ModelPrice, PriceTable, TokenUsage};
pub use report::{MonitorReport, MonitoredRun, Window};
pub use source::bigquery::BigQueryTranscriptSource;
pub use source::cloud_trace::CloudTraceTranscriptSource;
pub use source::{ProductionRun, TranscriptQuery, TranscriptSource};
pub use spec::{JudgeSpec, MonitorSpec, SourceSpec};
pub use store::{ReportMeta, ReportStore};
