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
//! A [`Guard`] watches a prompt rollout: it compares the label's version and the rollout
//! version on the production signals a [`GuardSource`] reads ([`BigQueryGuardSource`]) as a
//! [`GuardSpec`] (YAML) asks, decides to advance, abort or wait, and can write that decision to
//! the prompt store. `gadk-eval guard <spec.yaml>` ([`cli`]) runs it on a schedule; model prices
//! for its cost check come from a [`PriceTable`].
//!
//! An [`AlertSpec`] (YAML) sets fixed limits on the model calls in a recent window: cost, error
//! rate, calls and tokens ([`AlertMetric`]), each over every call or only those an
//! [`AlertFilter`] keeps. An [`AlertSource`] ([`BigQueryGuardSource`]) returns the window's calls
//! for an [`AlertQuery`] grouped by model, agent and prompt ([`UsageGroup`]), and
//! `gadk-eval alerts <source>...` ([`cli`]) checks every rule, posts to a webhook when one fires
//! and exits 1, so a scheduled run can page someone when usage goes over budget.
//!
//! [`judge_rows()`] turns a report's `llm_judge` and `reference` results into [`JudgeRow`]s
//! (console `FEEDBACK` rows with source `judge`), and [`write_judge_rows`] writes the ones a
//! [`ScoreSink`] does not hold yet, so the console's review queue can show them.
//!
//! [`monitor_points`], [`guard_points`] and [`alert_points`] turn the results of those commands
//! into Cloud Monitoring gauge points ([`MetricPoint`]), and a [`MetricsWriter`]
//! ([`CloudMetricsWriter`]) writes them as custom metrics under `custom.googleapis.com/gadk/`,
//! so dashboards and alert policies can follow pass rates, judge scores, rollouts and alert rules.
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

mod alert;
mod error;
mod guard_sources;
mod guard_stats;
mod notify;
#[cfg(test)]
mod testutil;

pub mod alert_source;
pub mod alert_spec;
pub mod batch;
pub mod batch_vertex;
pub mod cli;
pub mod eval_service;
pub mod experiment;
pub mod experiment_spec;
pub mod export;
pub mod gcp;
pub mod guard;
pub mod guard_report;
pub mod guard_source;
pub mod guard_spec;
pub mod judge;
pub mod judge_rows;
pub mod metrics;
pub mod monitor;
pub mod pairwise;
pub mod prices;
pub mod report;
pub mod source;
pub mod spec;
pub mod store;

pub use alert_source::{AlertQuery, AlertSource, UsageGroup};
pub use alert_spec::{AlertFilter, AlertMetric, AlertRule, AlertSpec};
pub use batch::{
    BatchLine, BatchPredictor, BatchReply, BatchResult, RecordingJudge, ReplayJudge, judge_in_batch,
};
pub use batch_vertex::VertexBatchPredictor;
pub use error::MonitorError;
pub use eval_service::{EvalServiceJudge, PairwiseChoice, PairwiseJudge, PairwiseOutcome};
pub use experiment::{
    CaseVerdicts, ExperimentReport, ExperimentVariant, VariantStats, Verdict, run_experiment,
};
pub use experiment_spec::{AgentInstruction, ExperimentAgent, ExperimentSpec};
pub use export::{
    CompareOp, ExportContext, ExportOptions, ExportSource, ExportedCase, RunScore, ScoreFilter,
    TraceInvocation, cases_text, export_runs, run_to_case, write_set_file,
};
pub use gcp::{AdcTokenSource, NoAuth, TokenSource};
pub use guard::Guard;
pub use guard_report::{
    Action, CheckResult, GuardReport, GuardStats, SideValue, Status, Unit, Verdict as GuardVerdict,
};
pub use guard_source::{GuardQuery, GuardSource, ModelTokens, VersionCalls, VersionFeedback};
pub use guard_spec::{Check, FeedbackSource, GuardSpec};
pub use judge::vertex_judge;
pub use judge_rows::{
    JUDGE_ROWS_PER_INSERT, JudgeRow, ScoreSink, WriteOutcome, judge_rows, write_judge_rows,
};
pub use metrics::{
    AlertRuleValue, AlertRun, CloudMetricsWriter, LABEL_VALUE_MAX, MetricPoint, MetricValue,
    MetricsWriter, SERIES_PER_REQUEST, alert_points, guard_points, monitor_points,
};
pub use monitor::{DEFAULT_CONCURRENCY, Monitor};
pub use pairwise::{
    DEFAULT_PAIRWISE_CRITERIA, PairwiseResult, PairwiseStats, PairwiseVerdict, run_pairwise,
};
pub use prices::{ModelPrice, PriceTable, TokenUsage};
pub use report::{MonitorReport, MonitoredRun, Window};
pub use source::bigquery::{BigQueryGuardSource, BigQueryTranscriptSource};
pub use source::cloud_trace::CloudTraceTranscriptSource;
pub use source::{ProductionRun, TranscriptQuery, TranscriptSource};
pub use spec::{JudgeSpec, MonitorSpec, SourceSpec};
pub use store::{ReportMeta, ReportStore};
