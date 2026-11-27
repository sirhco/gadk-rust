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

//! The `gadk-eval` command-line tool, as a library for testing.
//!
//! ```text
//! gadk-eval monitor <spec.yaml> [--out <url>] [--limit N] [--write-scores] [<judge flags>]
//! gadk-eval report <url> [<name>] [--limit N]
//! gadk-eval export --source bigquery://<project>.<dataset>.<table> --out <file> [--append]
//!     [--name N] [--trace ID]... [--since <n>h|<n>d] [--agent A] [--score F]... [--limit N]
//!     [--rubric T] [--min-good X] [--no-history] [--tool-args]
//! gadk-eval experiment <spec.yaml> [--out <url>] [<judge flags>]
//! gadk-eval guard <spec.yaml> [--apply] [--prices <file>] [--out <url>] [--max-bytes <n>]
//!     [<judge flags>]
//! gadk-eval guards <source>... [--apply] [--prices <file|gs://bucket/object>] [--out <url>]
//!     [--max-bytes <n>] [--notify <url|env:NAME>] [--notify-format slack|json] [<judge flags>]
//! gadk-eval alerts <source>... [--prices <file|gs://bucket/object>] [--max-bytes <n>]
//!     [--notify <url|env:NAME>] [--notify-format slack|json]
//!
//! <judge flags>: [--judge-backend gemini|eval-service] [--eval-location <location>]
//! ```
//!
//! `--judge-backend` picks what grades `llm_judge` and `reference` scorers: `gemini` (the
//! default) asks the spec's judge model for a JSON verdict, and `eval-service` sends each grade
//! to the Vertex AI Gen AI evaluation service in `--eval-location` (default `global`), with the
//! spec's judge model as the service's judge model ([`Backends::judge_backend`]).
//!
//! `monitor` exits 0 on success, 1 when the spec's `min_pass_rate` is set and either the pass
//! rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0), and 2 when the spec cannot be loaded, the runs cannot be
//! listed, the judge cannot be built or the report cannot be stored, or, with `--write-scores`,
//! when the source is not BigQuery (checked before anything runs) or the judge rows cannot be
//! written. With `--write-scores` it writes the report's judge results as `FEEDBACK` rows
//! ([`write_judge_rows`]) after the report is stored and
//! printed, then prints `scores: wrote <W> judge rows (<P> already present)` and applies
//! `min_pass_rate`. `report` exits 0, or 2 on a
//! store error or an unknown or invalid report name. `export` exits 0 when it added at least one
//! case, 1 when it added none (and wrote nothing), and 2 when the eval-set file cannot be read,
//! extended or written or the source fails. `experiment` runs an
//! [`ExperimentSpec`], stores the report first when `--out` is given, then
//! prints its summary; it exits 0 when the experiment ran (whatever the case results), and 2 on
//! an invalid spec, an unreadable or invalid eval set, a prompt store or prompt version error, a
//! model or judge build error, an experiment error or a store error. `guard` runs a
//! [`GuardSpec`] against the prompt rollout it names, prints the report and,
//! with `--out`, stores it (`stored report <name>`); with `--apply` it also writes the decision
//! (ramp, promote or abort) to the prompt store. Every BigQuery query it sends bills at most
//! `--max-bytes` bytes (default 1073741824). With the spec's `min_step_age` it needs `--out` and
//! reads the spec's newest stored report there that applied a ramp. It exits 0 when the
//! verdict is `advance`, 1 when it is `abort`, 3 when it is `wait` (applied or not), 4 when the label has no rollout (it
//! prints `guard <name>: <prompt>/<label> has no rollout` and stores nothing), and 2 on an
//! invalid spec (or `min_step_age` without `--out`), an unreadable price file, a prompt store
//! error (an unknown prompt or label, or a store changed since it was read), a query or judge
//! error, or a report store error (after `--apply` the decision stays written). Usage errors
//! exit 2.
//!
//! `guards` runs every guard spec its sources name, one at a time: a file, a directory (its
//! `.yaml` and `.yml` files), a `gs://<bucket>/<object>` or every `.yaml` and `.yml` object
//! directly under `gs://<bucket>/<prefix>/`. It reads and checks every spec, the price file
//! (a path or a `gs://` object) and `--out` before running any guard. Each guard's output
//! follows a `== <name> ==` line and is what `guard` prints for it; a guard that fails prints
//! `error: <message>` and the run goes on. The last line counts the outcomes
//! (`guards: <n> run — <a> advance, <b> abort, <c> wait, <d> no rollout, <e> errors`). With
//! `--notify <url|env:NAME>` (checked with the others before any guard runs), a run in which a
//! guard advanced, aborted or errored then posts a summary to that webhook, Slack-style text or
//! with `--notify-format json` the counts and each such guard, retries any transport error
//! or 5xx reply once, and prints `notify: posted` or
//! `notify: POST <origin> failed: <reason>` (the URL's `scheme://host[:port]` only). It exits 0
//! when every guard reached an outcome (advance, abort, wait or no rollout) and any post
//! succeeded, and 2 when a guard failed, the post failed, a source, spec, price file, `--out` or
//! `--notify` check failed (nothing runs and no summary is printed), stdout could not be
//! written, or on a usage error.
//!
//! `alerts` checks every [`AlertSpec`] its sources name (read as for `guards`), one at a time: one
//! BigQuery query per spec over `[now - window, now)`, capped at `--max-bytes`, then one line per
//! rule under `== <name> ==` (`<rule>: <metric> <value> > <max> — FIRED`, `≤ … — ok`, or
//! `insufficient` for an `error_rate` rule with too few calls); a spec that fails prints `error:
//! <message>` and the run goes on. The last line counts the rules (`alerts: <s> specs, <r> rules —
//! <f> fired, <o> ok, <i> insufficient, <e> errors`). Every spec, the price file (required by
//! `cost` rules) and `--notify` are checked before any query. With `--notify`, a run in which a
//! rule fired or a spec failed posts the summary and each such rule or spec, as `guards` posts. It
//! exits 0 when no rule fired, 1 when a rule fired, and 2 when a spec failed, the post failed, a
//! check before the run failed (nothing is queried and no summary is printed), stdout could not be
//! written, or on a usage error.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use gadk_agent::Agent;
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_eval::{EvalSet, Evaluator, GeminiJudge, JudgeBackend, Variant};
use gadk_model::Llm;
use gadk_prompts::{PromptRegistry, Selector, StoreError};

use crate::alert::{AlertCounts, AlertOutcome, RuleStatus, alerts_body, evaluate};
use crate::alert_source::{AlertQuery, AlertSource, UsageGroup};
use crate::alert_spec::{AlertMetric, AlertSpec};
use crate::error::MonitorError;
use crate::eval_service::{EvalServiceJudge, PairwiseJudge};
use crate::experiment::run_experiment;
use crate::experiment_spec::{AgentInstruction, ExperimentSpec};
use crate::export::{
    ExportOptions, ExportSource, ScoreFilter, cases_text, export_runs, write_set_file,
};
use crate::gcp::{AdcTokenSource, GcpHttp};
use crate::guard::Guard;
use crate::guard_report::GuardReport;
use crate::guard_source::GuardSource;
use crate::guard_sources::{load_alert_specs, load_prices, load_specs};
use crate::guard_spec::GuardSpec;
use crate::judge::vertex_judge;
use crate::judge_rows::{ScoreSink, judge_rows, write_judge_rows};
use crate::monitor::Monitor;
use crate::notify::{self, Counts, Notifier, NotifyFormat, Outcome};
use crate::prices::PriceTable;
use crate::source::bigquery::{BigQueryGuardSource, BigQueryTranscriptSource};
use crate::source::cloud_trace::CloudTraceTranscriptSource;
use crate::source::{TranscriptQuery, TranscriptSource};
use crate::spec::{
    DEFAULT_JUDGE_TIMEOUT, JudgeSpec, MAX_LIMIT, MonitorSpec, SourceSpec, amount,
    is_dataset_or_table, is_project_id,
};
use crate::store::{ReportStore, gcs};

/// The default number of reports `report` lists.
const DEFAULT_REPORT_LIMIT: usize = 20;

/// The default number of runs `export` reads with a window.
const DEFAULT_EXPORT_LIMIT: usize = 50;

/// The default `export --since` window.
const DEFAULT_SINCE: TimeDelta = TimeDelta::days(7);

/// What the `monitor` command builds from a spec: replaced in tests.
#[async_trait::async_trait]
pub trait Backends: Send + Sync {
    /// The transcript source for `spec`.
    fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError>;

    /// The judge model for `spec`.
    async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError>;

    /// The judge backend `choice` for `spec`. The default builds [`Backends::judge`]'s model
    /// as a [`GeminiJudge`] bounded by `spec.timeout` for [`JudgeBackendChoice::Gemini`], and
    /// refuses [`JudgeBackendChoice::EvalService`]; [`CloudBackends`] builds an
    /// [`EvalServiceJudge`] in `spec.project` / `eval_location` with `spec.model` as the judge
    /// model.
    async fn judge_backend(
        &self,
        spec: &JudgeSpec,
        choice: JudgeBackendChoice,
        eval_location: &str,
    ) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
        let _ = eval_location;
        match choice {
            JudgeBackendChoice::Gemini => gemini_judge(self.judge(spec).await?, spec),
            JudgeBackendChoice::EvalService => {
                Err(MonitorError::Judge(EVAL_SERVICE_UNSUPPORTED.into()))
            }
        }
    }

    /// The pairwise judge for `experiment --pairwise`, in `spec.project` / `eval_location` with
    /// `spec.model` as the judge model. The default refuses; [`CloudBackends`] returns an
    /// [`EvalServiceJudge`].
    async fn pairwise_judge(
        &self,
        spec: &JudgeSpec,
        eval_location: &str,
    ) -> Result<Arc<dyn PairwiseJudge>, MonitorError> {
        let _ = (spec, eval_location);
        Err(MonitorError::Judge(EVAL_SERVICE_UNSUPPORTED.into()))
    }

    /// The sink `monitor --write-scores` writes judge rows to, for the source `spec`. The
    /// default refuses with `--write-scores needs a bigquery source`; [`CloudBackends`] returns
    /// a [`BigQueryTranscriptSource`] for a `bigquery` source.
    fn score_sink(&self, _spec: &SourceSpec) -> Result<Arc<dyn ScoreSink>, MonitorError> {
        Err(MonitorError::Spec(WRITE_SCORES_NEEDS_BIGQUERY.into()))
    }

    /// The export source for the agent-analytics table `project.dataset.table` (`export`). The
    /// default refuses; [`CloudBackends`] returns a [`BigQueryTranscriptSource`].
    fn export_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Arc<dyn ExportSource>, MonitorError> {
        let _ = (project, dataset, table);
        Err(MonitorError::Source(
            "export is not supported by these backends".into(),
        ))
    }

    /// A model `name` on Vertex AI in `project` / `location` for the `experiment` command's
    /// agents. The default refuses; [`CloudBackends`] builds a Gemini model.
    async fn model(
        &self,
        project: &str,
        location: &str,
        name: &str,
    ) -> Result<Arc<dyn Llm>, MonitorError> {
        let _ = (project, location, name);
        Err(MonitorError::Judge(
            "models are not supported by these backends".into(),
        ))
    }

    /// The guard data source for the agent-analytics table `project.dataset.table`, every query
    /// capped at `max_bytes` billed (`guard`). The default refuses; [`CloudBackends`] returns a
    /// [`BigQueryGuardSource`].
    fn guard_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn GuardSource>, MonitorError> {
        let _ = (project, dataset, table, max_bytes);
        Err(MonitorError::Source(
            "guards are not supported by these backends".into(),
        ))
    }

    /// The alert data source for the agent-analytics table `project.dataset.table`, every
    /// query capped at `max_bytes` billed (`alerts`). The default refuses; [`CloudBackends`]
    /// returns a [`BigQueryGuardSource`].
    fn alert_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn AlertSource>, MonitorError> {
        let _ = (project, dataset, table, max_bytes);
        Err(MonitorError::Source(
            "alerts are not supported by these backends".into(),
        ))
    }

    /// The full names of the objects directly under `prefix` in the Cloud Storage `bucket`
    /// (`guards` with a `gs://` source). The default refuses; [`CloudBackends`] lists them with
    /// Application Default Credentials.
    async fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, MonitorError> {
        let _ = (bucket, prefix);
        Err(MonitorError::Store(GS_REFUSED.into()))
    }

    /// The contents of the Cloud Storage object `name` in `bucket`, or `None` when it does not
    /// exist (`guards` with a `gs://` source or price file). The default refuses;
    /// [`CloudBackends`] reads it with Application Default Credentials.
    async fn read_object(&self, bucket: &str, name: &str) -> Result<Option<String>, MonitorError> {
        let _ = (bucket, name);
        Err(MonitorError::Store(GS_REFUSED.into()))
    }

    /// Posts `body` as JSON to the webhook `url` (`guards --notify`) and returns the reply's
    /// HTTP status, or the transport error text. The default refuses; [`CloudBackends`] sends
    /// it with a 10 s timeout, without following redirects or sending credentials.
    ///
    /// The `Err` text is printed and posted as-is, apart from redaction of the URL, so it must
    /// not contain the URL or any part of it beyond its origin. Return a short kind such as
    /// `timed out` or `connection failed`, never a `reqwest::Error`'s `Display` (it embeds the
    /// URL).
    ///
    /// An error of exactly the default's text is not retried.
    async fn post_webhook(&self, url: &str, body: &serde_json::Value) -> Result<u16, String> {
        let _ = (url, body);
        Err(WEBHOOKS_UNSUPPORTED.into())
    }
}

/// What grades `llm_judge` and `reference` scorers (`--judge-backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum JudgeBackendChoice {
    /// The spec's judge model, asked for a JSON verdict (`gemini`, the default).
    #[default]
    Gemini,
    /// The Vertex AI Gen AI evaluation service (`eval-service`).
    EvalService,
}

impl JudgeBackendChoice {
    /// The choice named `name` on the command line: `gemini` or `eval-service`.
    pub fn from_flag(name: &str) -> Option<Self> {
        match name {
            "gemini" => Some(Self::Gemini),
            "eval-service" => Some(Self::EvalService),
            _ => None,
        }
    }
}

/// The error of the default [`Backends::judge_backend`] (for the evaluation service) and
/// [`Backends::pairwise_judge`].
const EVAL_SERVICE_UNSUPPORTED: &str = "the evaluation service is not supported by these backends";

/// The default `--eval-location`: `us-central1` does not serve `gemini-3.8-flash` as a judge
/// model.
const DEFAULT_EVAL_LOCATION: &str = "global";

/// `model` as a [`GeminiJudge`] bounded by `spec.timeout` (values below 1 ms mean 1 ms), as
/// [`Monitor::run`] and [`Guard::run`] build it.
fn gemini_judge(
    model: Arc<dyn Llm>,
    spec: &JudgeSpec,
) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
    let timeout = spec.timeout.max(Duration::from_millis(1));
    Ok(Arc::new(GeminiJudge::new(model, timeout)))
}

/// The error of the default [`Backends::list_objects`] and [`Backends::read_object`].
const GS_REFUSED: &str = "gs:// sources are not supported by these backends";

/// The error of `monitor --write-scores` without a BigQuery source.
const WRITE_SCORES_NEEDS_BIGQUERY: &str = "--write-scores needs a bigquery source";

/// The error of the default [`Backends::post_webhook`].
pub(crate) const WEBHOOKS_UNSUPPORTED: &str = "webhooks are not supported by these backends";

/// How long [`CloudBackends::post_webhook`] waits for a webhook's reply.
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh Cloud Storage client with Application Default Credentials. It is built per call:
/// the credentials' token refresh task runs on the runtime that built them, so a client cached
/// across runtimes could outlive it.
fn cloud_storage() -> Result<GcpHttp, MonitorError> {
    let auth = AdcTokenSource::adc()
        .map_err(|e| MonitorError::Store(format!("Application Default Credentials: {e}")))?;
    Ok(GcpHttp::new(GcpHttp::default_http(), Arc::new(auth)))
}

/// The full names of the objects directly under `prefix` in `bucket`: [`gcs::list_names`]
/// gives them without `prefix`, so it is put back.
async fn list_full_names(
    gcp: &GcpHttp,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<String>, MonitorError> {
    let names = gcs::list_names(gcp, bucket, prefix).await?;
    Ok(names
        .into_iter()
        .map(|name| format!("{prefix}{name}"))
        .collect())
}

/// The real backends: the BigQuery or Cloud Trace transcript source and Gemini on Vertex AI,
/// with Application Default Credentials.
#[derive(Debug, Clone, Copy, Default)]
pub struct CloudBackends;

#[async_trait::async_trait]
impl Backends for CloudBackends {
    fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
        match spec {
            SourceSpec::BigQuery {
                project,
                dataset,
                table,
            } => Ok(Arc::new(BigQueryTranscriptSource::new(
                project, dataset, table,
            )?)),
            SourceSpec::CloudTrace { project } => {
                Ok(Arc::new(CloudTraceTranscriptSource::new(project)?))
            }
        }
    }

    async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
        vertex_judge(spec).await
    }

    async fn judge_backend(
        &self,
        spec: &JudgeSpec,
        choice: JudgeBackendChoice,
        eval_location: &str,
    ) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
        match choice {
            JudgeBackendChoice::Gemini => gemini_judge(self.judge(spec).await?, spec),
            JudgeBackendChoice::EvalService => Ok(Arc::new(EvalServiceJudge::new(
                &spec.project,
                eval_location,
                &spec.model,
                spec.timeout,
            )?)),
        }
    }

    async fn pairwise_judge(
        &self,
        spec: &JudgeSpec,
        eval_location: &str,
    ) -> Result<Arc<dyn PairwiseJudge>, MonitorError> {
        Ok(Arc::new(EvalServiceJudge::new(
            &spec.project,
            eval_location,
            &spec.model,
            spec.timeout,
        )?))
    }

    fn score_sink(&self, spec: &SourceSpec) -> Result<Arc<dyn ScoreSink>, MonitorError> {
        match spec {
            SourceSpec::BigQuery {
                project,
                dataset,
                table,
            } => Ok(Arc::new(BigQueryTranscriptSource::new(
                project, dataset, table,
            )?)),
            SourceSpec::CloudTrace { .. } => {
                Err(MonitorError::Spec(WRITE_SCORES_NEEDS_BIGQUERY.into()))
            }
        }
    }

    fn export_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Arc<dyn ExportSource>, MonitorError> {
        Ok(Arc::new(BigQueryTranscriptSource::new(
            project, dataset, table,
        )?))
    }

    async fn model(
        &self,
        project: &str,
        location: &str,
        name: &str,
    ) -> Result<Arc<dyn Llm>, MonitorError> {
        // The same Gemini-on-Vertex-AI build as the judge's, with the same error text.
        vertex_judge(&JudgeSpec {
            model: name.to_owned(),
            project: project.to_owned(),
            location: location.to_owned(),
            timeout: DEFAULT_JUDGE_TIMEOUT,
        })
        .await
    }

    fn guard_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn GuardSource>, MonitorError> {
        Ok(Arc::new(
            BigQueryGuardSource::new(project, dataset, table)?.with_max_bytes_billed(max_bytes),
        ))
    }

    fn alert_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn AlertSource>, MonitorError> {
        Ok(Arc::new(
            BigQueryGuardSource::new(project, dataset, table)?.with_max_bytes_billed(max_bytes),
        ))
    }

    async fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, MonitorError> {
        list_full_names(&cloud_storage()?, bucket, prefix).await
    }

    async fn read_object(&self, bucket: &str, name: &str) -> Result<Option<String>, MonitorError> {
        gcs::read_text(&cloud_storage()?, bucket, name).await
    }

    /// Posts with a client of its own: a 10 s timeout, no redirects, no Google
    /// credentials. A transport error is returned as a kind that never names the URL: `timed
    /// out`, `connection failed` or `request failed` (a `reqwest` error's own text embeds it).
    async fn post_webhook(&self, url: &str, body: &serde_json::Value) -> Result<u16, String> {
        let client = reqwest::Client::builder()
            .timeout(WEBHOOK_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(gadk_platform::tracking_header())
            .build()
            .map_err(|_| "request failed".to_owned())?;
        let reply = client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| {
                let kind = if e.is_timeout() {
                    "timed out"
                } else if e.is_connect() {
                    "connection failed"
                } else {
                    "request failed"
                };
                kind.to_owned()
            })?;
        Ok(reply.status().as_u16())
    }
}

/// `--judge-backend` and `--eval-location` (`monitor`, `experiment`, `guard` and `guards`).
fn judge_flags() -> [Arg; 2] {
    [
        Arg::new("judge-backend")
            .long("judge-backend")
            .value_parser(["gemini", "eval-service"])
            .default_value("gemini")
            .help("what grades llm_judge and reference scorers"),
        Arg::new("eval-location")
            .long("eval-location")
            .default_value(DEFAULT_EVAL_LOCATION)
            .help("the evaluation service location (with --judge-backend eval-service)"),
    ]
}

/// The judge flags of a command.
#[derive(Debug, Clone)]
struct JudgeFlags {
    backend: JudgeBackendChoice,
    location: String,
}

impl JudgeFlags {
    fn of(m: &ArgMatches) -> Self {
        let backend = m
            .get_one::<String>("judge-backend")
            .and_then(|name| JudgeBackendChoice::from_flag(name))
            .unwrap_or_default();
        let location = m
            .get_one::<String>("eval-location")
            .cloned()
            .unwrap_or_else(|| DEFAULT_EVAL_LOCATION.to_owned());
        JudgeFlags { backend, location }
    }
}

fn limit_arg() -> Arg {
    Arg::new("limit")
        .long("limit")
        .value_parser(value_parser!(u64).range(1..=MAX_LIMIT as u64))
}

fn cli() -> Command {
    Command::new("gadk-eval")
        .about("Grade production agent runs with gadk-eval scorers")
        .subcommand_required(true)
        .subcommand(
            Command::new("monitor")
                .about("Run a monitor spec and print its summary")
                .arg(
                    Arg::new("spec")
                        .required(true)
                        .help("monitor spec YAML file"),
                )
                .arg(
                    Arg::new("out")
                        .long("out")
                        .help("store the report: <dir>, file://<dir> or gs://<bucket>[/<prefix>]"),
                )
                .arg(limit_arg().help("runs to grade, 1-1000 (overrides the spec)"))
                .arg(
                    Arg::new("write-scores")
                        .long("write-scores")
                        .action(ArgAction::SetTrue)
                        .help(
                            "write the judge scores to the bigquery table as FEEDBACK rows \
                             (source judge)",
                        ),
                )
                .args(judge_flags()),
        )
        .subcommand(
            Command::new("report")
                .about("List stored reports, or print one report's summary")
                .arg(
                    Arg::new("url")
                        .required(true)
                        .help("<dir>, file://<dir> or gs://<bucket>[/<prefix>]"),
                )
                .arg(Arg::new("name").help("the report to print"))
                .arg(limit_arg().help("reports to list (default 20)")),
        )
        .subcommand(export_cli())
        .subcommand(
            Command::new("experiment")
                .about("Run an experiment spec: an eval set against each variant of an agent")
                .arg(
                    Arg::new("spec")
                        .required(true)
                        .help("experiment spec YAML file"),
                )
                .arg(
                    Arg::new("out")
                        .long("out")
                        .help("store the report: <dir>, file://<dir> or gs://<bucket>[/<prefix>]"),
                )
                .args(judge_flags()),
        )
        .subcommand(guard_cli())
        .subcommand(guards_cli())
        .subcommand(alerts_cli())
}

/// The flags `guard` and `guards` share, with `prices` as the `--prices` argument (a path for
/// `guard`, a path or `gs://` URL for `guards`).
fn guard_flags(prices: Arg) -> [Arg; 4] {
    [
        Arg::new("apply")
            .long("apply")
            .action(ArgAction::SetTrue)
            .help("write the decision to the prompt store (ramp, promote or abort)"),
        prices.long("prices"),
        Arg::new("out")
            .long("out")
            .help("store the report: <dir>, file://<dir> or gs://<bucket>[/<prefix>]"),
        Arg::new("max-bytes")
            .long("max-bytes")
            .value_parser(value_parser!(i64).range(1..))
            .help("most bytes billed per BigQuery query (default 1073741824)"),
    ]
}

fn guard_cli() -> Command {
    Command::new("guard")
        .about("Compare both sides of a prompt rollout and advance, abort or wait")
        .after_help("Exit codes: 0 advance, 1 abort, 3 wait, 4 no rollout, 2 error.")
        .arg(Arg::new("spec").required(true).help("guard spec YAML file"))
        .args(guard_flags(
            Arg::new("prices")
                .value_parser(value_parser!(PathBuf))
                .help("price file (YAML) for cost_per_call checks"),
        ))
        .args(judge_flags())
}

fn guards_cli() -> Command {
    Command::new("guards")
        .about("Run every guard spec in files, directories or gs:// locations, one at a time")
        .after_help("Exit codes: 0 every guard reached an outcome, 2 a guard failed or an error.")
        .arg(
            Arg::new("sources")
                .value_name("source")
                .required(true)
                .num_args(1..)
                .help(
                    "guard spec file, directory, gs://<bucket>/<object> or \
                     gs://<bucket>/<prefix>/",
                ),
        )
        .args(guard_flags(Arg::new("prices").help(
            "price file (YAML) for cost_per_call checks: <file> or gs://<bucket>/<object>",
        )))
        .arg(
            Arg::new("notify")
                .long("notify")
                .value_name("url|env:NAME")
                .help("post a summary to this webhook when a guard advances, aborts or errors"),
        )
        .arg(
            Arg::new("notify-format")
                .long("notify-format")
                .value_parser(["slack", "json"])
                .requires("notify")
                .help("webhook body: slack (default) or json"),
        )
        .args(judge_flags())
}

fn alerts_cli() -> Command {
    Command::new("alerts")
        .about(
            "Check usage limits (cost, error rate, calls, tokens) in alert specs from files, \
             directories or gs:// locations",
        )
        .after_help("Exit codes: 0 no rule fired, 1 a rule fired, 2 a spec failed or an error.")
        .arg(
            Arg::new("sources")
                .value_name("source")
                .required(true)
                .num_args(1..)
                .help(
                    "alert spec file, directory, gs://<bucket>/<object> or \
                     gs://<bucket>/<prefix>/",
                ),
        )
        .arg(
            Arg::new("prices")
                .long("prices")
                .help("price file (YAML) for cost rules: <file> or gs://<bucket>/<object>"),
        )
        .arg(
            Arg::new("max-bytes")
                .long("max-bytes")
                .value_parser(value_parser!(i64).range(1..))
                .help("most bytes billed per BigQuery query (default 1073741824)"),
        )
        .arg(
            Arg::new("notify")
                .long("notify")
                .value_name("url|env:NAME")
                .help("post a summary to this webhook when a rule fires or a spec errors"),
        )
        .arg(
            Arg::new("notify-format")
                .long("notify-format")
                .value_parser(["slack", "json"])
                .requires("notify")
                .help("webhook body: slack (default) or json"),
        )
}

fn export_cli() -> Command {
    let flag = |id: &'static str, help: &'static str| {
        Arg::new(id).long(id).action(ArgAction::SetTrue).help(help)
    };
    Command::new("export")
        .about("Write production runs as eval-set cases")
        .arg(
            Arg::new("source")
                .long("source")
                .required(true)
                .value_parser(parse_source)
                .help("bigquery://<project>.<dataset>.<table>"),
        )
        .arg(
            Arg::new("out")
                .long("out")
                .required(true)
                .value_parser(value_parser!(PathBuf))
                .help("eval-set YAML file to create (never overwritten), or to extend with --append"),
        )
        .arg(flag("append", "add the cases to the existing eval set at --out").conflicts_with("name"))
        .arg(Arg::new("name").long("name").value_parser(parse_nonblank).help("name of the new eval set (default: the file stem)"))
        .arg(
            Arg::new("trace")
                .long("trace")
                .action(ArgAction::Append)
                .value_parser(parse_trace)
                .conflicts_with_all(["since", "agent", "score", "limit"])
                .help("export the runs of this trace (32 hex digits; repeatable)"),
        )
        .arg(
            Arg::new("since")
                .long("since")
                .value_parser(parse_since)
                .help("runs started in the last <n>h or <n>d, 1h-90d (default 7d)"),
        )
        .arg(Arg::new("agent").long("agent").help("only runs of this root agent"))
        .arg(
            Arg::new("score")
                .long("score")
                .action(ArgAction::Append)
                .value_parser(ScoreFilter::parse)
                .help("latest score filter: name=value, name<n, name<=n, name>n or name>=n (repeatable; all must hold)"),
        )
        .arg(limit_arg().help("runs to read, 1-1000 (default 50)"))
        .arg(
            Arg::new("rubric")
                .long("rubric")
                .value_parser(parse_nonblank)
                .help("rubric of llm_judge cases (default: the eval set's judge_rubric)"),
        )
        .arg(
            Arg::new("min-good")
                .long("min-good")
                .value_parser(parse_min_good)
                .help("lowest number score that counts as positive (default 4)"),
        )
        .arg(flag("no-history", "leave out the user messages of the session's earlier runs"))
        .arg(flag("tool-args", "keep each tool call's arguments in tools scorers"))
}

/// `bigquery://<project>.<dataset>.<table>`: a `[A-Za-z0-9_-]+` project and `[A-Za-z0-9_]+`
/// dataset and table.
fn parse_source(s: &str) -> Result<(String, String, String), String> {
    let bad = || format!("expected bigquery://<project>.<dataset>.<table>, got {s:?}");
    let parts: Vec<&str> = s
        .strip_prefix("bigquery://")
        .ok_or_else(bad)?
        .split('.')
        .collect();
    match parts[..] {
        [project, dataset, table]
            if is_project_id(project)
                && is_dataset_or_table(dataset)
                && is_dataset_or_table(table) =>
        {
            Ok((project.to_owned(), dataset.to_owned(), table.to_owned()))
        }
        _ => Err(bad()),
    }
}

/// A trace id: 32 hex digits, lowercased.
fn parse_trace(s: &str) -> Result<String, String> {
    if s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s.to_ascii_lowercase())
    } else {
        Err(format!("expected 32 hex digits, got {s:?}"))
    }
}

/// `<n>h` or `<n>d`, from 1 hour to 90 days.
fn parse_since(s: &str) -> Result<TimeDelta, String> {
    let window = match amount(s) {
        Some((n, 'h')) => TimeDelta::try_hours(n),
        Some((n, 'd')) => TimeDelta::try_days(n),
        _ => None,
    };
    window
        .filter(|w| *w <= TimeDelta::days(90))
        .ok_or_else(|| format!("expected <n>h or <n>d from 1h to 90d, got {s:?}"))
}

/// Text that is not empty or whitespace only.
fn parse_nonblank(s: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err("expected a non-blank value".to_owned())
    } else {
        Ok(s.to_owned())
    }
}

/// A finite number.
fn parse_min_good(s: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("expected a number, got {s:?}"))
}

/// Runs the tool with `args` (including the program name) and returns the exit code (see the
/// module docs).
pub async fn run(
    args: Vec<String>,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let m = match cli().try_get_matches_from(args) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = write!(stdout, "{}", e.render());
                return 0;
            }
            let _ = write!(stderr, "{}", e.render());
            return 2;
        }
    };
    let limit = |m: &clap::ArgMatches| {
        m.get_one::<u64>("limit")
            .and_then(|n| usize::try_from(*n).ok())
    };
    let result = match m.subcommand() {
        Some(("monitor", m)) => {
            let spec = m.get_one::<String>("spec").map_or("", String::as_str);
            let out = m.get_one::<String>("out").map(String::as_str);
            monitor(
                spec,
                out,
                limit(m),
                m.get_flag("write-scores"),
                &JudgeFlags::of(m),
                backends,
                stdout,
            )
            .await
        }
        Some(("report", m)) => {
            let url = m.get_one::<String>("url").map_or("", String::as_str);
            let name = m.get_one::<String>("name").map(String::as_str);
            report(url, name, limit(m), stdout).await.map(|()| 0)
        }
        Some(("export", m)) => export(m, limit(m), backends, stdout).await,
        Some(("experiment", m)) => {
            let spec = m.get_one::<String>("spec").map_or("", String::as_str);
            let out = m.get_one::<String>("out").map(String::as_str);
            experiment(spec, out, &JudgeFlags::of(m), backends, stdout).await
        }
        Some(("guard", m)) => guard(m, backends, stdout).await,
        Some(("guards", m)) => guards(m, backends, stdout).await,
        Some(("alerts", m)) => alerts(m, backends, stdout).await,
        _ => Ok(2),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(stderr, "gadk-eval: {e}");
            2
        }
    }
}

/// `monitor`: 0 or 1 (below `min_pass_rate`); errors map to 2. With `write_scores` the score
/// sink is resolved before anything runs, and the report's judge rows are written after it is
/// stored and printed, before `min_pass_rate` is applied.
async fn monitor(
    spec_path: &str,
    out: Option<&str>,
    limit: Option<usize>,
    write_scores: bool,
    judge_flags: &JudgeFlags,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let mut spec = MonitorSpec::load(spec_path)?;
    if let Some(n) = limit {
        spec.limit = n;
    }
    let store = out.map(ReportStore::open).transpose()?;
    let sink = if write_scores {
        Some(backends.score_sink(&spec.source)?)
    } else {
        None
    };
    let source = backends.source(&spec.source)?;
    let judge = match &spec.judge {
        Some(j) => Some(
            backends
                .judge_backend(j, judge_flags.backend, &judge_flags.location)
                .await?,
        ),
        None => None,
    };
    let report = Monitor::new()
        .run_with_backend(&spec, source.as_ref(), judge)
        .await?;
    // Store before printing, so a failing stdout (a closed pipe) does not lose the report.
    let stored = match store {
        Some(store) => Some(store.put(&report).await?),
        None => None,
    };
    write!(stdout, "{}", report.summary())?;
    if let Some(name) = stored {
        writeln!(stdout, "stored report {name}")?;
    }
    if let Some(sink) = sink {
        let outcome = write_judge_rows(
            sink.as_ref(),
            judge_rows(&report, &spec.name),
            report.window.from,
            &spec.name,
        )
        .await?;
        writeln!(
            stdout,
            "scores: wrote {} judge rows ({} already present)",
            outcome.written, outcome.present
        )?;
    }
    if let Some(min) = spec.min_pass_rate {
        match report.pass_rate() {
            Some(rate) if rate >= min => {}
            Some(rate) => {
                writeln!(
                    stdout,
                    "FAILED: pass rate {:.1}% is below the minimum {:.1}%",
                    rate * 100.0,
                    min * 100.0
                )?;
                return Ok(1);
            }
            None => {
                writeln!(
                    stdout,
                    "FAILED: no runs to grade (minimum pass rate {:.1}%)",
                    min * 100.0
                )?;
                return Ok(1);
            }
        }
    }
    Ok(0)
}

/// `report`: lists the newest reports, or prints one report's summary.
async fn report(
    url: &str,
    name: Option<&str>,
    limit: Option<usize>,
    stdout: &mut dyn Write,
) -> Result<(), MonitorError> {
    let store = ReportStore::open(url)?;
    if let Some(name) = name {
        let report = store.get(name).await?;
        write!(stdout, "{}", report.summary())?;
        return Ok(());
    }
    let reports = store.list(limit.unwrap_or(DEFAULT_REPORT_LIMIT)).await?;
    if reports.is_empty() {
        writeln!(stdout, "no reports")?;
        return Ok(());
    }
    writeln!(
        stdout,
        "{:<48} {:<24} {:<20} {:>5} {:>9} {:>6}",
        "NAME", "SPEC", "GENERATED", "RUNS", "PASS RATE", "ERRORS"
    )?;
    for r in reports {
        let rate = r
            .pass_rate
            .map_or_else(|| "n/a".to_owned(), |p| format!("{:.1}%", p * 100.0));
        writeln!(
            stdout,
            "{:<48} {:<24} {:<20} {:>5} {:>9} {:>6}",
            r.name,
            r.spec,
            r.generated_at.format("%Y-%m-%dT%H:%M:%SZ"),
            r.runs,
            rate,
            r.errors
        )?;
    }
    Ok(())
}

/// Where [`run_one_guard`] gets model prices.
enum Prices {
    /// A price file read when the guard runs, after the prompt store opens (`guard`).
    Path(Option<PathBuf>),
    /// A table read before any guard runs (`guards`).
    Loaded(Option<PriceTable>),
}

/// The settings of one guard run, shared by `guard` and `guards`.
struct GuardRunOpts {
    /// Write the decision to the prompt store.
    apply: bool,
    prices: Prices,
    /// The report store, already opened.
    out: Option<ReportStore>,
    /// The most bytes billed per BigQuery query.
    max_bytes: i64,
    /// `--judge-backend` and `--eval-location`.
    judge: JudgeFlags,
}

/// `guard`: 0 (advance), 1 (abort), 3 (wait) or 4 (no rollout); errors map to 2.
async fn guard(
    m: &ArgMatches,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let spec_path = m.get_one::<String>("spec").map_or("", String::as_str);
    let spec = GuardSpec::load(spec_path)?;
    let out_url = m.get_one::<String>("out");
    if spec.min_step_age.is_some() && out_url.is_none() {
        return Err(MonitorError::Spec(
            "guard spec: min_step_age needs --out".into(),
        ));
    }
    // Opened before the run, so a bad URL fails before anything is written.
    let out = out_url.map(|url| ReportStore::open(url)).transpose()?;
    let opts = GuardRunOpts {
        apply: m.get_flag("apply"),
        prices: Prices::Path(m.get_one::<PathBuf>("prices").cloned()),
        out,
        max_bytes: m
            .get_one::<i64>("max-bytes")
            .copied()
            .unwrap_or(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED),
        judge: JudgeFlags::of(m),
    };
    Ok(run_one_guard(&spec, &opts, backends, stdout).await?.code)
}

/// What [`run_one_guard`] found.
struct GuardRun {
    /// 0 (advance), 1 (abort), 3 (wait) or 4 (no rollout).
    code: i32,
    /// The report, unless the label had no rollout.
    report: Option<GuardReport>,
    /// The stored report's name, with `--out`.
    stored: Option<String>,
}

/// Runs `spec` once and prints what `guard` prints; the code is 0 (advance), 1 (abort), 3
/// (wait) or 4 (no rollout).
async fn run_one_guard(
    spec: &GuardSpec,
    opts: &GuardRunOpts,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<GuardRun, MonitorError> {
    let previous = match (&opts.out, spec.min_step_age) {
        (Some(out), Some(_)) => out.latest_applied_ramp(&spec.name).await?,
        _ => None,
    };
    let store = gadk_prompts_cli::open_store(&spec.store)
        .map_err(|e| MonitorError::Store(format!("prompt store: {e}")))?;
    let read;
    let prices = match &opts.prices {
        Prices::Path(path) => {
            read = path.as_ref().map(PriceTable::load).transpose()?;
            read.as_ref()
        }
        Prices::Loaded(table) => table.as_ref(),
    };
    let SourceSpec::BigQuery {
        project,
        dataset,
        table,
    } = &spec.source
    else {
        return Err(MonitorError::Spec(format!(
            "guard spec: unsupported source {}",
            spec.source.describe()
        )));
    };
    let source = backends.guard_source(project, dataset, table, opts.max_bytes)?;
    let judge = match &spec.judge {
        Some(j) if spec.wants_judge() => Some(
            backends
                .judge_backend(j, opts.judge.backend, &opts.judge.location)
                .await?,
        ),
        _ => None,
    };
    let report = Guard::new()
        .run_with_backend(
            spec,
            store.as_ref(),
            source.as_ref(),
            prices,
            judge,
            previous.as_ref(),
            opts.apply,
        )
        .await?;
    let Some(report) = report else {
        writeln!(
            stdout,
            "guard {}: {}/{} has no rollout",
            spec.name, spec.prompt, spec.label
        )?;
        return Ok(GuardRun {
            code: 4,
            report: None,
            stored: None,
        });
    };
    // Print before storing: with --apply the decision is already written to the prompt store,
    // so the operator must see it even when storing the report fails.
    write!(stdout, "{}", report.summary())?;
    let mut stored = None;
    if let Some(out) = &opts.out {
        let name = out.put_guard(&report).await?;
        writeln!(stdout, "stored report {name}")?;
        stored = Some(name);
    }
    Ok(GuardRun {
        code: report.exit_code(),
        report: Some(report),
        stored,
    })
}

/// `guards`: 0 when every guard reached an outcome, 2 when one failed or the `--notify` post
/// failed; errors map to 2.
async fn guards(
    m: &ArgMatches,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let sources: Vec<String> = m
        .get_many::<String>("sources")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let specs = load_specs(&sources, backends).await?;
    let out_url = m.get_one::<String>("out");
    if out_url.is_none()
        && let Some(s) = specs.iter().find(|s| s.spec.min_step_age.is_some())
    {
        return Err(MonitorError::Spec(format!(
            "guard spec {}: min_step_age needs --out",
            s.origin
        )));
    }
    let prices = match m.get_one::<String>("prices") {
        Some(source) => Some(load_prices(source, backends).await?),
        None => None,
    };
    let out = out_url.map(|url| ReportStore::open(url)).transpose()?;
    // The last pre-run check, so a bad --notify also fails before any guard runs.
    let notifier = notifier(m)?;
    let opts = GuardRunOpts {
        apply: m.get_flag("apply"),
        prices: Prices::Loaded(prices),
        out,
        max_bytes: m
            .get_one::<i64>("max-bytes")
            .copied()
            .unwrap_or(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED),
        judge: JudgeFlags::of(m),
    };
    let mut stdout = Watched {
        inner: stdout,
        failed: false,
    };
    let mut counts = Counts {
        run: specs.len(),
        ..Counts::default()
    };
    // The guards that advanced, aborted or errored, in run order, for the --notify body.
    let mut outcomes = Vec::new();
    for s in &specs {
        writeln!(stdout, "== {} ==", s.spec.name)?;
        let error = match run_one_guard(&s.spec, &opts, backends, &mut stdout).await {
            Ok(GuardRun {
                code: code @ (0 | 1),
                report: Some(report),
                stored,
            }) => {
                if code == 0 {
                    counts.advance += 1;
                } else {
                    counts.abort += 1;
                }
                outcomes.push(Outcome::Report { report, stored });
                None
            }
            Ok(GuardRun { code: 3, .. }) => {
                counts.wait += 1;
                None
            }
            Ok(GuardRun { code: 4, .. }) => {
                counts.no_rollout += 1;
                None
            }
            Ok(GuardRun { code, .. }) => Some(format!("unexpected exit code {code}")),
            // A stdout failure ends the run, as it does for `guard`.
            Err(e) if stdout.failed => return Err(e),
            Err(e) => Some(e.to_string()),
        };
        if let Some(message) = error {
            writeln!(stdout, "error: {message}")?;
            counts.errors += 1;
            outcomes.push(Outcome::Error {
                name: s.spec.name.clone(),
                message,
            });
        }
    }
    let summary = format!(
        "guards: {} run — {} advance, {} abort, {} wait, {} no rollout, {} errors",
        counts.run, counts.advance, counts.abort, counts.wait, counts.no_rollout, counts.errors
    );
    writeln!(stdout, "{summary}")?;
    let code = if counts.errors == 0 { 0 } else { 2 };
    let Some(notifier) = notifier else {
        return Ok(code);
    };
    if counts.advance + counts.abort + counts.errors == 0 {
        return Ok(code);
    }
    let body = notify::body(notifier.format, &summary, &counts, &outcomes);
    match notify::post(backends, &notifier.url, &body, tokio::time::sleep).await {
        Ok(()) => {
            writeln!(stdout, "notify: posted")?;
            Ok(code)
        }
        Err(reason) => {
            writeln!(stdout, "notify: {reason}")?;
            Ok(2)
        }
    }
}

/// The webhook `--notify` and `--notify-format` name (`guards` and `alerts`), if any.
fn notifier(m: &ArgMatches) -> Result<Option<Notifier>, MonitorError> {
    let Some(target) = m.get_one::<String>("notify") else {
        return Ok(None);
    };
    Ok(Some(Notifier {
        url: notify::resolve(target, &|name| std::env::var(name).ok())?,
        format: match m.get_one::<String>("notify-format").map(String::as_str) {
            Some("json") => NotifyFormat::Json,
            _ => NotifyFormat::Slack,
        },
    }))
}

/// `alerts`: 0 when no rule fired, 1 when one did, 2 when a spec errored or the `--notify`
/// post failed; errors map to 2.
async fn alerts(
    m: &ArgMatches,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let sources: Vec<String> = m
        .get_many::<String>("sources")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let specs = load_alert_specs(&sources, backends).await?;
    let prices_source = m.get_one::<String>("prices");
    if prices_source.is_none()
        && let Some(s) = specs.iter().find(|s| {
            s.spec
                .rules
                .iter()
                .any(|r| matches!(r.metric, AlertMetric::Cost { .. }))
        })
    {
        return Err(MonitorError::Spec(format!(
            "alert spec {}: cost rules need --prices",
            s.origin
        )));
    }
    let prices = match prices_source {
        Some(source) => Some(load_prices(source, backends).await?),
        None => None,
    };
    // The last pre-run check, so a bad --notify also fails before any query.
    let notifier = notifier(m)?;
    let max_bytes = m
        .get_one::<i64>("max-bytes")
        .copied()
        .unwrap_or(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED);
    let mut counts = AlertCounts {
        specs: specs.len(),
        ..AlertCounts::default()
    };
    // The rules that fired and the specs that errored, in output order, for the --notify body.
    let mut outcomes = Vec::new();
    for s in &specs {
        writeln!(stdout, "== {} ==", s.spec.name)?;
        match usage(&s.spec, max_bytes, backends).await {
            Ok(groups) => {
                let results = evaluate(&s.spec, &groups, prices.as_ref());
                counts.add(&results);
                for result in results {
                    writeln!(stdout, "{}", result.line)?;
                    if result.status == RuleStatus::Fired {
                        outcomes.push(AlertOutcome::Fired {
                            spec: s.spec.name.clone(),
                            result,
                        });
                    }
                }
            }
            Err(e) => {
                let message = e.to_string();
                writeln!(stdout, "error: {message}")?;
                counts.errors += 1;
                outcomes.push(AlertOutcome::Error {
                    spec: s.spec.name.clone(),
                    message,
                });
            }
        }
    }
    let summary = counts.summary_line();
    writeln!(stdout, "{summary}")?;
    let code = if counts.errors > 0 {
        2
    } else if counts.fired > 0 {
        1
    } else {
        0
    };
    let Some(notifier) = notifier else {
        return Ok(code);
    };
    if counts.fired + counts.errors == 0 {
        return Ok(code);
    }
    let body = alerts_body(notifier.format, &summary, &counts, &outcomes);
    match notify::post(backends, &notifier.url, &body, tokio::time::sleep).await {
        Ok(()) => {
            writeln!(stdout, "notify: posted")?;
            Ok(code)
        }
        Err(reason) => {
            writeln!(stdout, "notify: {reason}")?;
            Ok(2)
        }
    }
}

/// The usage groups of `spec`'s calls in `[now - window, now)`, read through
/// [`Backends::alert_source`] with every query capped at `max_bytes`.
async fn usage(
    spec: &AlertSpec,
    max_bytes: i64,
    backends: &dyn Backends,
) -> Result<Vec<UsageGroup>, MonitorError> {
    let SourceSpec::BigQuery {
        project,
        dataset,
        table,
    } = &spec.source
    else {
        return Err(MonitorError::Spec(format!(
            "alert spec: unsupported source {}",
            spec.source.describe()
        )));
    };
    let source = backends.alert_source(project, dataset, table, max_bytes)?;
    let to = Utc::now();
    source.usage(&AlertQuery::new(to - spec.window, to)).await
}

/// A writer that remembers whether a write to `inner` failed.
struct Watched<'a> {
    inner: &'a mut dyn Write,
    failed: bool,
}

impl Write for Watched<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| {
            if e.kind() != std::io::ErrorKind::Interrupted {
                self.failed = true;
            }
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|_| self.failed = true)
    }
}

/// An error about the eval-set file `path`.
fn file_error(path: &Path, e: impl std::fmt::Display) -> MonitorError {
    MonitorError::Io(std::io::Error::other(format!("{}: {e}", path.display())))
}

/// `export`: 0 when cases were added, 1 when none were (nothing is written); errors map to 2.
async fn export(
    m: &ArgMatches,
    limit: Option<usize>,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let Some((project, dataset, table)) = m.get_one::<(String, String, String)>("source") else {
        return Ok(2);
    };
    let Some(out) = m.get_one::<PathBuf>("out") else {
        return Ok(2);
    };
    let existing = if m.get_flag("append") {
        let set = EvalSet::load(out).map_err(|e| file_error(out, e))?;
        let text = std::fs::read_to_string(out).map_err(|e| file_error(out, e))?;
        // Checked before any query: whether the file can take appended cases at all.
        gadk_eval::append_cases(&text, &[], &|_| Vec::new()).map_err(|e| file_error(out, e))?;
        Some((text, set))
    } else {
        if out.exists() {
            return Err(file_error(
                out,
                "already exists (use --append to add to it)",
            ));
        }
        None
    };
    let rubric = m.get_one::<String>("rubric").cloned();
    let mut opts = ExportOptions::default()
        .with_history(!m.get_flag("no-history"))
        .with_tool_args(m.get_flag("tool-args"));
    if let Some(r) = rubric.clone().or_else(|| {
        existing
            .as_ref()
            .and_then(|(_, set)| set.judge_rubric.clone())
    }) {
        opts = opts.with_rubric(r);
    }
    if let Some(min_good) = m.get_one::<f64>("min-good") {
        opts = opts.with_min_good(*min_good);
    }
    let source = backends.export_source(project, dataset, table)?;
    let traces: Vec<String> = m
        .get_many::<String>("trace")
        .map(|t| t.cloned().collect())
        .unwrap_or_default();
    let selected: Vec<(String, DateTime<Utc>)> = if traces.is_empty() {
        let now = Utc::now();
        let since = m
            .get_one::<TimeDelta>("since")
            .copied()
            .unwrap_or(DEFAULT_SINCE);
        let q = TranscriptQuery {
            from: now - since,
            to: now,
            agent: m.get_one::<String>("agent").cloned(),
            limit: limit.unwrap_or(DEFAULT_EXPORT_LIMIT),
        };
        let filters: Vec<ScoreFilter> = m
            .get_many::<ScoreFilter>("score")
            .map(|f| f.cloned().collect())
            .unwrap_or_default();
        source.matching(&filters, &q).await?
    } else {
        source
            .trace_invocations(&traces)
            .await?
            .into_iter()
            .map(|t| (t.invocation_id, t.started_at))
            .collect()
    };
    let known: HashSet<String> = existing
        .as_ref()
        .map(|(_, set)| set.cases.iter().map(|c| c.id.clone()).collect())
        .unwrap_or_default();
    let mut lines = Vec::new();
    let mut added = Vec::new();
    for (run, result) in export_runs(source.as_ref(), &selected, &opts).await? {
        let id = format!("inv-{}", run.invocation_id);
        match result {
            _ if known.contains(&id) => {
                lines.push(format!("skipped {id}: already in {}", out.display()));
            }
            Ok(exported) => {
                lines.push(format!(
                    "added {id} ({}) trace {}",
                    if exported.positive {
                        "reference"
                    } else {
                        "judge"
                    },
                    run.trace_id.as_deref().unwrap_or("none")
                ));
                added.push(exported);
            }
            Err(reason) => lines.push(format!("skipped {id}: {reason}")),
        }
    }
    for line in &lines {
        writeln!(stdout, "{line}")?;
    }
    if !added.is_empty() {
        let name = m.get_one::<String>("name").cloned().unwrap_or_else(|| {
            out.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let current = existing.as_ref().map(|(text, _)| text.as_str());
        let text = cases_text(current, &name, rubric.as_deref(), &added)
            .map_err(|e| file_error(out, e))?;
        write_set_file(out, &text, current.is_none()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                file_error(out, "was created while exporting; nothing was written")
            } else {
                file_error(out, e)
            }
        })?;
    }
    writeln!(stdout, "{} cases added to {}", added.len(), out.display())?;
    Ok(if added.is_empty() { 1 } else { 0 })
}

/// An error reading the eval set `path` of an experiment spec.
fn set_error(path: &Path, e: impl std::fmt::Display) -> MonitorError {
    MonitorError::Spec(format!("experiment spec: set {}: {e}", path.display()))
}

/// An error opening or loading an experiment spec's prompt store.
fn prompts_error(e: StoreError) -> MonitorError {
    MonitorError::Spec(format!("experiment spec: prompts: {e}"))
}

/// `experiment`: 0 once the experiment ran and, with `--out`, was stored; errors map to 2.
async fn experiment(
    spec_path: &str,
    out: Option<&str>,
    judge_flags: &JudgeFlags,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let spec = ExperimentSpec::load(spec_path)?;
    let text = std::fs::read_to_string(&spec.set).map_err(|e| set_error(&spec.set, e))?;
    let set = EvalSet::from_yaml(&text).map_err(|e| set_error(&spec.set, e))?;
    let registry = match (&spec.agent.instruction, spec.prompts.as_deref()) {
        (AgentInstruction::Prompt(_), Some(url)) => {
            let store = gadk_prompts_cli::open_store(url).map_err(prompts_error)?;
            // Loaded once: every run of the experiment sees the same prompt versions.
            let registry = PromptRegistry::builder(store)
                .refresh_interval(Duration::ZERO)
                .load()
                .await
                .map_err(prompts_error)?;
            Some(registry)
        }
        _ => None,
    };
    let result = run_spec(
        &spec,
        &set,
        registry.as_ref(),
        out,
        judge_flags,
        backends,
        stdout,
    )
    .await;
    // Closed on every path once loaded, whatever `run_spec` returned.
    if let Some(registry) = &registry {
        registry.close();
    }
    result
}

/// The `experiment` command after the spec, eval set and prompt registry are loaded.
async fn run_spec(
    spec: &ExperimentSpec,
    set: &EvalSet,
    registry: Option<&PromptRegistry>,
    out: Option<&str>,
    judge_flags: &JudgeFlags,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    // Every variant's prompt version must exist before any model is built or run.
    if let (AgentInstruction::Prompt(prompt), Some(registry)) = (&spec.agent.instruction, registry)
    {
        for (i, v) in spec.variants.iter().enumerate() {
            let selector = v.selector().cloned().unwrap_or(Selector::Latest);
            registry.get(prompt, &selector).map_err(|e| {
                MonitorError::Spec(format!("experiment spec: variants[{}].prompt: {e}", i + 1))
            })?;
        }
    }
    // Opened before the run, so a bad URL fails before any model call.
    let store = out.map(ReportStore::open).transpose()?;
    // The Gemini judge is the run-timeout-bounded model of `Evaluator::new`, as before; the
    // evaluation service needs no judge model.
    let evaluator = match judge_flags.backend {
        JudgeBackendChoice::Gemini => Evaluator::new(backends.judge(&spec.judge).await?),
        JudgeBackendChoice::EvalService => Evaluator::with_backend(
            backends
                .judge_backend(&spec.judge, judge_flags.backend, &judge_flags.location)
                .await?,
        ),
    };
    let mut models: HashMap<&str, Arc<dyn Llm>> = HashMap::new();
    for name in spec.variants.iter().filter_map(Variant::model) {
        if !models.contains_key(name) {
            let model = backends.model(&spec.project, &spec.location, name).await?;
            models.insert(name, model);
        }
    }
    let evaluator = evaluator.runs_per_case(spec.runs);
    let report = run_experiment(&spec.name, &evaluator, set, &spec.variants, |v| {
        build_agent(spec, registry, &models, v)
    })
    .await?;
    // Store before printing, so a failing stdout (a closed pipe) does not lose the report.
    let stored = match store {
        Some(store) => Some(store.put_experiment(&report).await?),
        None => None,
    };
    write!(stdout, "{}", report.summary())?;
    if let Some(name) = stored {
        writeln!(stdout, "stored report {name}")?;
    }
    Ok(0)
}

/// The LLM agent of `spec` for variant `v`: the variant's model and the agent's instruction
/// (the text, or the prompt version the variant's selector picks).
fn build_agent(
    spec: &ExperimentSpec,
    registry: Option<&PromptRegistry>,
    models: &HashMap<&str, Arc<dyn Llm>>,
    v: &Variant,
) -> Result<Arc<dyn Agent>, String> {
    let model = v
        .model()
        .and_then(|m| models.get(m))
        .cloned()
        .ok_or_else(|| format!("variant {}: no model", v.name()))?;
    let mut cfg = LlmAgentConfig {
        name: spec.agent.name.clone(),
        description: spec.agent.description.clone(),
        model: Some(model),
        ..Default::default()
    };
    match &spec.agent.instruction {
        AgentInstruction::Text(text) => cfg.instruction = text.clone(),
        AgentInstruction::Prompt(prompt) => {
            let registry =
                registry.ok_or_else(|| "no prompt store for `agent.prompt`".to_owned())?;
            let selector = v.selector().cloned().unwrap_or(Selector::Latest);
            cfg.instruction_provider = Some(registry.instruction(prompt, selector).into_provider());
        }
    }
    llm_agent::new(cfg).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use crate::export::{ExportContext, RunScore, TraceInvocation};
    use crate::source::ProductionRun;
    use crate::testutil::{FakeJudge, FakeSink, FakeSource, at, run as prod_run};

    use super::*;

    /// A writer that is interrupted on its first write.
    struct InterruptedOnce {
        interrupted: bool,
        written: Vec<u8>,
    }

    impl Write for InterruptedOnce {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_interrupted_write_is_not_a_failure() {
        let mut inner = InterruptedOnce {
            interrupted: false,
            written: Vec::new(),
        };
        let mut w = Watched {
            inner: &mut inner,
            failed: false,
        };
        w.write_all(b"hello").unwrap();
        assert!(!w.failed);
        assert_eq!(inner.written, b"hello");
    }

    const VERDICT: &str = r#"{"pass": true, "score": 5, "rationale": "ok"}"#;

    /// Backends serving `source` and a judge that always passes (or fails to build).
    struct Fake {
        source: Arc<FakeSource>,
        judge_error: Option<String>,
    }

    impl Fake {
        fn new(runs: Vec<ProductionRun>) -> Self {
            Self {
                source: Arc::new(FakeSource::new(runs)),
                judge_error: None,
            }
        }
    }

    #[async_trait::async_trait]
    impl Backends for Fake {
        fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            Ok(self.source.clone())
        }

        async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            match &self.judge_error {
                Some(e) => Err(MonitorError::Judge(e.clone())),
                None => Ok(FakeJudge::new(&[VERDICT; 10])),
            }
        }
    }

    /// A spec file in `dir` with `extra` YAML lines and scorer `contains: "order"`.
    fn spec_file(dir: &Path, extra: &str) -> String {
        let p = dir.join("m.yaml");
        std::fs::write(
            &p,
            format!(
                "name: support\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
                 window: 1d\n{extra}scorers: [contains: \"order\"]\n"
            ),
        )
        .unwrap();
        p.display().to_string()
    }

    /// Runs the CLI: `(exit code, stdout, stderr)`.
    async fn cli_run(args: &[&str], backends: &dyn Backends) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut argv = vec!["gadk-eval".to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let code = run(argv, backends, &mut out, &mut err).await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn passing(id: &str) -> ProductionRun {
        let now = chrono::Utc::now().timestamp();
        prod_run(
            id,
            now - 60,
            &[("Where is my order?", "Your order ships.", &[])],
        )
    }

    fn failing(id: &str) -> ProductionRun {
        let now = chrono::Utc::now().timestamp();
        prod_run(id, now - 120, &[("hi", "hello", &[])])
    }

    /// A writer whose every write fails, like a closed pipe.
    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_report_is_stored_even_when_stdout_fails() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "");
        let out = dir.path().join("reports");
        let fake = Fake::new(vec![passing("a")]);
        let argv = [
            "gadk-eval",
            "monitor",
            &spec,
            "--out",
            &out.display().to_string(),
        ]
        .map(str::to_owned)
        .to_vec();
        let mut err = Vec::new();
        let code = run(argv, &fake, &mut BrokenPipe, &mut err).await;
        assert_eq!(code, 2, "{}", String::from_utf8_lossy(&err));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn monitor_passes_and_stores_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "min_pass_rate: 0.5\n");
        let out = dir.path().join("reports");
        let fake = Fake::new(vec![passing("a"), failing("b")]);
        let (code, stdout, stderr) = cli_run(
            &["monitor", &spec, "--out", &out.display().to_string()],
            &fake,
        )
        .await;
        assert_eq!(code, 0, "{stdout}{stderr}");
        assert!(
            stdout.starts_with("monitor support: 2 runs, 1 passed, 1 failed"),
            "{stdout}"
        );
        assert!(stdout.contains("stored report support-"), "{stdout}");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn monitor_below_the_minimum_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "min_pass_rate: 0.9\n");
        let fake = Fake::new(vec![passing("a"), failing("b")]);
        let (code, stdout, _) = cli_run(&["monitor", &spec], &fake).await;
        assert_eq!(code, 1);
        assert!(
            stdout.contains("FAILED: pass rate 50.0% is below the minimum 90.0%"),
            "{stdout}"
        );
    }

    #[tokio::test]
    async fn zero_runs_with_a_minimum_exits_1_and_without_exits_0() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "min_pass_rate: 0.9\n");
        let (code, stdout, _) = cli_run(&["monitor", &spec], &Fake::new(Vec::new())).await;
        assert_eq!(code, 1);
        assert!(stdout.contains("FAILED: no runs to grade"), "{stdout}");
        let spec = spec_file(dir.path(), "");
        let (code, stdout, _) = cli_run(&["monitor", &spec], &Fake::new(Vec::new())).await;
        assert_eq!(code, 0);
        assert!(stdout.contains("pass rate n/a"), "{stdout}");
    }

    #[tokio::test]
    async fn limit_overrides_the_spec() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "limit: 5\n");
        let fake = Fake::new(Vec::new());
        let (code, _, _) = cli_run(&["monitor", &spec, "--limit", "7"], &fake).await;
        assert_eq!(code, 0);
        assert_eq!(fake.source.queries.lock().unwrap()[0].limit, 7);
        for bad in ["0", "1001", "x"] {
            let (code, _, stderr) = cli_run(&["monitor", &spec, "--limit", bad], &fake).await;
            assert_eq!(code, 2, "{bad}");
            assert!(stderr.contains("--limit"), "{bad}: {stderr}");
        }
    }

    #[tokio::test]
    async fn a_bad_spec_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.yaml").display().to_string();
        let (code, _, stderr) = cli_run(&["monitor", &missing], &Fake::new(Vec::new())).await;
        assert_eq!(code, 2);
        assert!(
            stderr.starts_with("gadk-eval: monitor spec: reading "),
            "{stderr}"
        );
        let spec = spec_file(dir.path(), "window: 0h\n");
        let (code, _, stderr) = cli_run(&["monitor", &spec], &Fake::new(Vec::new())).await;
        assert_eq!(code, 2);
        assert!(stderr.contains("monitor spec:"), "{stderr}");
    }

    #[tokio::test]
    async fn source_judge_and_store_failures_exit_2() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "");
        let mut source = FakeSource::new(Vec::new());
        source.fail = Some("HTTP 403: Access Denied".into());
        let fake = Fake {
            source: Arc::new(source),
            judge_error: None,
        };
        let (code, _, stderr) = cli_run(&["monitor", &spec], &fake).await;
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            "gadk-eval: transcript source: HTTP 403: Access Denied\n"
        );

        let judged = dir.path().join("judged.yaml");
        std::fs::write(
            &judged,
            "name: s\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1d\n\
             judge: { model: gemini-2.5-flash }\nscorers: [llm_judge: be nice]\n",
        )
        .unwrap();
        let fake = Fake {
            source: Arc::new(FakeSource::new(Vec::new())),
            judge_error: Some("no Application Default Credentials".into()),
        };
        let (code, _, stderr) = cli_run(&["monitor", &judged.display().to_string()], &fake).await;
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            "gadk-eval: judge: no Application Default Credentials\n"
        );

        let blocker = dir.path().join("a-file");
        std::fs::write(&blocker, "x").unwrap();
        let (code, _, stderr) = cli_run(
            &[
                "monitor",
                &spec,
                "--out",
                &blocker.join("sub").display().to_string(),
            ],
            &Fake::new(vec![passing("a")]),
        )
        .await;
        assert_eq!(code, 2);
        assert!(stderr.starts_with("gadk-eval: report store: "), "{stderr}");
    }

    #[tokio::test]
    async fn report_lists_and_prints() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "");
        let out = dir.path().join("reports").display().to_string();
        let fake = Fake::new(vec![passing("a")]);
        assert_eq!(
            cli_run(&["monitor", &spec, "--out", &out], &fake).await.0,
            0
        );
        let (code, stdout, _) = cli_run(&["report", &out], &fake).await;
        assert_eq!(code, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2, "{stdout}");
        assert!(lines[0].starts_with("NAME"), "{stdout}");
        assert!(lines[1].starts_with("support-"), "{stdout}");
        assert!(lines[1].contains("100.0%"), "{stdout}");
        let name = lines[1].split_whitespace().next().unwrap();
        let (code, stdout, _) = cli_run(&["report", &out, name], &fake).await;
        assert_eq!(code, 0);
        assert!(
            stdout.starts_with("monitor support: 1 runs, 1 passed"),
            "{stdout}"
        );
        let (code, _, stderr) =
            cli_run(&["report", &out, "support-20200101T000000Z.json"], &fake).await;
        assert_eq!(code, 2);
        assert!(stderr.contains("not found"), "{stderr}");
        let (code, _, stderr) = cli_run(&["report", &out, "../m.yaml"], &fake).await;
        assert_eq!(code, 2);
        assert!(stderr.contains("invalid report name"), "{stderr}");
        let empty = dir.path().join("none").display().to_string();
        let (code, stdout, _) = cli_run(&["report", &empty], &fake).await;
        assert_eq!((code, stdout.as_str()), (0, "no reports\n"));
    }

    #[tokio::test]
    async fn usage() {
        let fake = Fake::new(Vec::new());
        let (code, stdout, _) = cli_run(&["--help"], &fake).await;
        assert_eq!(code, 0);
        assert!(stdout.contains("monitor"), "{stdout}");
        assert_eq!(cli_run(&[], &fake).await.0, 2);
        assert_eq!(cli_run(&["bogus"], &fake).await.0, 2);
    }

    #[test]
    fn cloud_backends_reject_an_invalid_cloud_trace_project() {
        let spec = SourceSpec::CloudTrace {
            project: "bad project".into(),
        };
        let err = CloudBackends.source(&spec).err().unwrap();
        assert!(
            matches!(&err, MonitorError::Source(m) if m.contains("\"bad project\"")),
            "{err}"
        );
    }

    /// [`Fake`] with `sink` as its score sink.
    struct Scoring {
        inner: Fake,
        sink: Arc<FakeSink>,
    }

    #[async_trait::async_trait]
    impl Backends for Scoring {
        fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            self.inner.source(spec)
        }

        async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            self.inner.judge(spec).await
        }

        fn score_sink(&self, _spec: &SourceSpec) -> Result<Arc<dyn ScoreSink>, MonitorError> {
            Ok(self.sink.clone())
        }
    }

    fn scoring(runs: Vec<ProductionRun>, sink: FakeSink) -> Scoring {
        Scoring {
            inner: Fake::new(runs),
            sink: Arc::new(sink),
        }
    }

    /// A spec file in `dir` with `extra` YAML lines, a judge, and scorers `contains: "order"`
    /// and `llm_judge`.
    fn judged_spec(dir: &Path, extra: &str) -> String {
        let p = dir.join("judged.yaml");
        std::fs::write(
            &p,
            format!(
                "name: support\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
                 window: 1d\n{extra}judge: {{ model: gemini-3.8-flash }}\n\
                 scorers: [contains: \"order\", llm_judge: be nice]\n"
            ),
        )
        .unwrap();
        p.display().to_string()
    }

    #[tokio::test]
    async fn write_scores_prints_the_scores_line_after_the_stored_report() {
        let dir = tempfile::tempdir().unwrap();
        let spec = judged_spec(dir.path(), "");
        let out = dir.path().join("reports");
        let mut sink = FakeSink::default();
        sink.present
            .insert(("c".to_owned(), "support/llm_judge".to_owned()));
        let backends = scoring(vec![passing("a"), passing("b"), passing("c")], sink);
        let (code, stdout, stderr) = cli_run(
            &[
                "monitor",
                &spec,
                "--out",
                &out.display().to_string(),
                "--write-scores",
            ],
            &backends,
        )
        .await;
        assert_eq!(code, 0, "{stdout}{stderr}");
        let lines: Vec<&str> = stdout.lines().collect();
        let n = lines.len();
        assert!(
            lines[n - 2].starts_with("stored report support-"),
            "{stdout}"
        );
        assert_eq!(
            lines[n - 1],
            "scores: wrote 2 judge rows (1 already present)"
        );

        let stored = std::fs::read_dir(&out)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let report = crate::report::MonitorReport::read_json(&stored).unwrap();
        let calls = backends.sink.existing_calls.lock().unwrap();
        assert_eq!(*calls, [(3, report.window.from)]);
        let inserts = backends.sink.inserts.lock().unwrap();
        assert_eq!(inserts.len(), 1);
        let mut ids: Vec<&str> = inserts[0]
            .0
            .iter()
            .map(|r| r.invocation_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["a", "b"]);
        assert!(
            inserts[0]
                .0
                .iter()
                .all(|r| r.name == "support/llm_judge" && r.score == 5 && r.pass),
            "{:?}",
            inserts[0].0
        );
        assert_eq!(inserts[0].1, "support");
        // The stored report has no new field.
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&stored).unwrap()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["generated_at", "runs", "source", "spec", "window"]);
    }

    #[tokio::test]
    async fn write_scores_without_a_score_sink_exits_2_before_any_query() {
        let dir = tempfile::tempdir().unwrap();
        let spec = judged_spec(dir.path(), "");
        let fake = Fake::new(vec![passing("a")]);
        let (code, stdout, stderr) = cli_run(&["monitor", &spec, "--write-scores"], &fake).await;
        assert_eq!(code, 2);
        assert_eq!(stdout, "");
        assert!(
            stderr.contains("--write-scores needs a bigquery source"),
            "{stderr}"
        );
        assert_eq!(
            stderr,
            "gadk-eval: monitor spec: --write-scores needs a bigquery source\n"
        );
        assert!(fake.source.queries.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_backends_have_no_score_sink_for_cloud_trace() {
        let err = CloudBackends
            .score_sink(&SourceSpec::cloud_trace("proj"))
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            "monitor spec: --write-scores needs a bigquery source"
        );
    }

    #[tokio::test]
    async fn a_failed_write_exits_2_after_the_summary() {
        let dir = tempfile::tempdir().unwrap();
        let spec = judged_spec(dir.path(), "min_pass_rate: 0.9\n");
        let hint = "query failed: Access Denied; writing scores needs \
                    roles/bigquery.dataEditor on the table";
        let backends = scoring(
            vec![passing("a")],
            FakeSink {
                fail_insert: Some(hint.into()),
                ..FakeSink::default()
            },
        );
        let (code, stdout, stderr) =
            cli_run(&["monitor", &spec, "--write-scores"], &backends).await;
        assert_eq!(code, 2);
        assert!(
            stdout.starts_with("monitor support: 1 runs, 1 passed"),
            "{stdout}"
        );
        assert!(!stdout.contains("scores:"), "{stdout}");
        assert!(!stdout.contains("FAILED"), "{stdout}");
        assert!(
            stderr.contains("; writing scores needs roles/bigquery.dataEditor on the table"),
            "{stderr}"
        );
        assert_eq!(stderr, format!("gadk-eval: transcript source: {hint}\n"));
    }

    #[tokio::test]
    async fn min_pass_rate_applies_after_a_successful_write() {
        let dir = tempfile::tempdir().unwrap();
        let spec = judged_spec(dir.path(), "min_pass_rate: 0.9\n");
        let backends = scoring(vec![passing("a"), failing("b")], FakeSink::default());
        let (code, stdout, _) = cli_run(&["monitor", &spec, "--write-scores"], &backends).await;
        assert_eq!(code, 1);
        assert!(
            stdout.ends_with(
                "scores: wrote 2 judge rows (0 already present)\n\
                 FAILED: pass rate 50.0% is below the minimum 90.0%\n"
            ),
            "{stdout}"
        );
    }

    #[tokio::test]
    async fn no_judge_rows_print_zero_and_call_no_sink() {
        let dir = tempfile::tempdir().unwrap();
        let spec = spec_file(dir.path(), "");
        let backends = scoring(vec![passing("a")], FakeSink::default());
        let (code, stdout, _) = cli_run(&["monitor", &spec, "--write-scores"], &backends).await;
        assert_eq!(code, 0);
        assert!(
            stdout.ends_with("scores: wrote 0 judge rows (0 already present)\n"),
            "{stdout}"
        );
        assert!(backends.sink.existing_calls.lock().unwrap().is_empty());
        assert!(backends.sink.inserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn without_the_flag_nothing_is_written_or_printed() {
        let dir = tempfile::tempdir().unwrap();
        let spec = judged_spec(dir.path(), "");
        let backends = scoring(vec![passing("a")], FakeSink::default());
        let (code, stdout, _) = cli_run(&["monitor", &spec], &backends).await;
        assert_eq!(code, 0);
        assert!(!stdout.contains("scores:"), "{stdout}");
        assert!(backends.sink.existing_calls.lock().unwrap().is_empty());
        assert!(backends.sink.inserts.lock().unwrap().is_empty());
        let (code, help, _) = cli_run(&["monitor", "--help"], &backends).await;
        assert_eq!(code, 0);
        assert!(help.contains("--write-scores"), "{help}");
    }

    // ---- export ----

    /// A scripted export source recording each call as text.
    #[derive(Default)]
    struct FakeExport {
        invocations: Vec<TraceInvocation>,
        matches: Vec<(String, DateTime<Utc>)>,
        runs: Vec<ProductionRun>,
        contexts: HashMap<String, ExportContext>,
        fail: Option<String>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeExport {
        fn record(&self, call: String) -> Result<(), MonitorError> {
            self.calls.lock().unwrap().push(call);
            match &self.fail {
                Some(e) => Err(MonitorError::Source(e.clone())),
                None => Ok(()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ExportSource for FakeExport {
        async fn trace_invocations(
            &self,
            traces: &[String],
        ) -> Result<Vec<TraceInvocation>, MonitorError> {
            self.record(format!("trace_invocations {traces:?}"))?;
            Ok(self.invocations.clone())
        }

        async fn runs(
            &self,
            ids: &[String],
            from: DateTime<Utc>,
        ) -> Result<Vec<ProductionRun>, MonitorError> {
            self.record(format!("runs {ids:?} {}", from.timestamp()))?;
            Ok(self
                .runs
                .iter()
                .filter(|r| ids.contains(&r.invocation_id))
                .cloned()
                .collect())
        }

        async fn histories(
            &self,
            runs: &[ProductionRun],
        ) -> Result<HashMap<String, Vec<ProductionRun>>, MonitorError> {
            self.record(format!("histories {}", runs.len()))?;
            Ok(HashMap::new())
        }

        async fn contexts(
            &self,
            ids: &[String],
            from: DateTime<Utc>,
        ) -> Result<HashMap<String, ExportContext>, MonitorError> {
            self.record(format!("contexts {ids:?} {}", from.timestamp()))?;
            Ok(self.contexts.clone())
        }

        async fn matching(
            &self,
            filters: &[ScoreFilter],
            q: &TranscriptQuery,
        ) -> Result<Vec<(String, DateTime<Utc>)>, MonitorError> {
            self.record(format!(
                "matching {filters:?} {:?} {} {}",
                q.agent,
                q.limit,
                (q.to - q.from).num_hours()
            ))?;
            Ok(self.matches.clone())
        }
    }

    /// Backends whose only service is `export`.
    struct Exporter(Arc<FakeExport>);

    #[async_trait::async_trait]
    impl Backends for Exporter {
        fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            Err(MonitorError::Source("no monitor source".into()))
        }

        async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            Err(MonitorError::Judge("no judge".into()))
        }

        fn export_source(
            &self,
            project: &str,
            dataset: &str,
            table: &str,
        ) -> Result<Arc<dyn ExportSource>, MonitorError> {
            self.0
                .calls
                .lock()
                .unwrap()
                .push(format!("source {project}.{dataset}.{table}"));
            Ok(self.0.clone())
        }
    }

    const TRACE: &str = "0123456789abcdef0123456789abcdef";
    const SOURCE: &str = "bigquery://proj.analytics.agent_events";

    /// Runs `export` with `args`: `(exit code, stdout, stderr)`.
    async fn export_run(args: &[&str], fake: &Arc<FakeExport>) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut argv = vec!["gadk-eval".to_owned(), "export".to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let code = run(argv, &Exporter(fake.clone()), &mut out, &mut err).await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// Runs `e-1` (thumbs up, trace `TRACE`) and `e-2` (no score) of session `s`, started at
    /// 1,000 and 1,060 s.
    fn two_runs() -> FakeExport {
        let mut e1 = prod_run("e-1", 1_000, &[("first", "one", &[])]);
        e1.trace_id = Some(TRACE.into());
        let e2 = prod_run("e-2", 1_060, &[("second", "two", &[])]);
        let mut contexts = HashMap::new();
        contexts.insert(
            "e-1".to_owned(),
            ExportContext::new(vec![RunScore::new(
                "thumbs",
                serde_json::json!(true),
                "bool",
                "app",
                "u",
                at(1_100),
            )]),
        );
        FakeExport {
            invocations: vec![
                TraceInvocation::new(TRACE, "e-1", at(1_000)),
                TraceInvocation::new(TRACE, "e-2", at(1_060)),
            ],
            matches: vec![("e-2".into(), at(1_060)), ("e-1".into(), at(1_000))],
            runs: vec![e1, e2],
            contexts,
            ..FakeExport::default()
        }
    }

    fn path_str(p: &Path) -> String {
        p.display().to_string()
    }

    #[tokio::test]
    async fn export_by_trace_writes_a_new_set() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("regressions.yaml");
        let fake = Arc::new(two_runs());
        let (code, stdout, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--trace",
                &TRACE.to_uppercase(),
                "--rubric",
                "Answers well.",
            ],
            &fake,
        )
        .await;
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(
            stdout,
            format!(
                "added inv-e-1 (reference) trace {TRACE}\nadded inv-e-2 (judge) trace none\n\
                 2 cases added to {}\n",
                out.display()
            )
        );
        assert_eq!(
            fake.calls(),
            [
                "source proj.analytics.agent_events".to_owned(),
                format!("trace_invocations [{TRACE:?}]"),
                "runs [\"e-1\", \"e-2\"] 1000".to_owned(),
                "histories 2".to_owned(),
                "contexts [\"e-1\", \"e-2\"] 1000".to_owned(),
            ]
        );
        let set = EvalSet::load(&out).unwrap();
        assert_eq!(set.name, "regressions");
        assert_eq!(set.judge_rubric.as_deref(), Some("Answers well."));
        let ids: Vec<&str> = set.cases.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["inv-e-1", "inv-e-2"]);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(&format!("  # trace {TRACE}\n")), "{text}");
        assert!(text.contains("      - llm_judge: {}\n"), "{text}");
    }

    #[tokio::test]
    async fn export_never_overwrites_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("x.yaml");
        std::fs::write(&out, "keep").unwrap();
        let fake = Arc::new(two_runs());
        let (code, _, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--trace",
                TRACE,
            ],
            &fake,
        )
        .await;
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            format!(
                "gadk-eval: {}: already exists (use --append to add to it)\n",
                out.display()
            )
        );
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "keep");
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn export_appends_skipping_cases_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("set.yaml");
        let before = "name: kept\njudge_rubric: \"Be right.\"\ncases:\n  - id: inv-e-1\n    \
                      turns: [first]\n    expect: [contains: one]\n";
        std::fs::write(&out, before).unwrap();
        let fake = Arc::new(two_runs());
        let (code, stdout, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--append",
                "--since",
                "1h",
                "--score",
                "rating>=4",
                "--score",
                "thumbs=true",
                "--agent",
                "support",
                "--limit",
                "2",
                "--no-history",
            ],
            &fake,
        )
        .await;
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(
            stdout,
            format!(
                "skipped inv-e-1: already in {0}\nadded inv-e-2 (judge) trace none\n\
                 1 cases added to {0}\n",
                out.display()
            )
        );
        let calls = fake.calls();
        assert!(
            calls[1].starts_with("matching [Compare { name: \"rating\", op: Ge, value: 4.0 }, Equals { name: \"thumbs\", value: \"true\" }] Some(\"support\") 2 1"),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("histories")),
            "{calls:?}"
        );
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.starts_with(before), "{text}");
        assert!(text.ends_with("      - llm_judge: {}\n"), "{text}");
        let set = EvalSet::load(&out).unwrap();
        assert_eq!(set.name, "kept");
        assert_eq!(set.cases.len(), 2);
    }

    #[tokio::test]
    async fn export_adding_nothing_exits_1_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("none.yaml");
        let fake = Arc::new(FakeExport::default());
        let (code, stdout, _) =
            export_run(&["--source", SOURCE, "--out", &path_str(&out)], &fake).await;
        assert_eq!(code, 1);
        assert_eq!(stdout, format!("0 cases added to {}\n", out.display()));
        assert!(!out.exists());
        assert!(
            fake.calls()[1].ends_with(" None 50 168"),
            "default window 7d and limit 50: {:?}",
            fake.calls()
        );

        // A rating of 5 is not positive with --min-good 6, and there is no rubric.
        let mut fake = two_runs();
        fake.contexts.insert(
            "e-1".to_owned(),
            ExportContext::new(vec![RunScore::new(
                "rating",
                serde_json::json!(5),
                "number",
                "app",
                "u",
                at(1_100),
            )]),
        );
        let (code, stdout, _) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--trace",
                TRACE,
                "--min-good",
                "6",
            ],
            &Arc::new(fake),
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(
            stdout,
            format!(
                "skipped inv-e-1: needs a rubric\nskipped inv-e-2: needs a rubric\n\
                 0 cases added to {}\n",
                out.display()
            )
        );
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn export_usage_errors_exit_2() {
        let dir = tempfile::tempdir().unwrap();
        let out = path_str(&dir.path().join("u.yaml"));
        let cases: [&[&str]; 12] = [
            &["--source", SOURCE, "--out", &out, "--name", " \t"],
            &["--source", SOURCE, "--out", &out, "--rubric", "  "],
            &["--out", &out],
            &["--source", "proj.analytics.agent_events", "--out", &out],
            &["--source", "bigquery://example.com:p.d.t", "--out", &out],
            &["--source", SOURCE, "--out", &out, "--trace", "abc"],
            &[
                "--source", SOURCE, "--out", &out, "--trace", TRACE, "--since", "1h",
            ],
            &[
                "--source", SOURCE, "--out", &out, "--trace", TRACE, "--limit", "5",
            ],
            &["--source", SOURCE, "--out", &out, "--append", "--name", "n"],
            &["--source", SOURCE, "--out", &out, "--since", "91d"],
            &["--source", SOURCE, "--out", &out, "--since", "30m"],
            &["--source", SOURCE, "--out", &out, "--score", "rating>>4"],
        ];
        for args in cases {
            let fake = Arc::new(two_runs());
            let (code, _, stderr) = export_run(args, &fake).await;
            assert_eq!(code, 2, "{args:?}: {stderr}");
            assert!(fake.calls().is_empty(), "{args:?}");
        }
        let fake = Arc::new(two_runs());
        let (code, _, _) = export_run(
            &["--source", SOURCE, "--out", &out, "--min-good", "nan"],
            &fake,
        )
        .await;
        assert_eq!(code, 2);
    }

    #[tokio::test]
    async fn export_file_and_source_errors_exit_2() {
        let dir = tempfile::tempdir().unwrap();
        let flow = dir.path().join("flow.yaml");
        let text = "name: s\ncases: [{id: a, turns: [hi], expect: [contains: x]}]\n";
        std::fs::write(&flow, text).unwrap();
        let fake = Arc::new(two_runs());
        let (code, _, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&flow),
                "--append",
                "--trace",
                TRACE,
                "--rubric",
                "r",
            ],
            &fake,
        )
        .await;
        assert_eq!(code, 2);
        assert!(
            stderr.contains("`cases:` must be the last top-level key"),
            "{stderr}"
        );
        assert_eq!(std::fs::read_to_string(&flow).unwrap(), text);

        let missing = dir.path().join("missing.yaml");
        let (code, _, stderr) = export_run(
            &["--source", SOURCE, "--out", &path_str(&missing), "--append"],
            &Arc::new(two_runs()),
        )
        .await;
        assert_eq!(code, 2);
        assert!(stderr.starts_with("gadk-eval: "), "{stderr}");

        let mut failing = two_runs();
        failing.fail = Some("HTTP 403: Access Denied".into());
        let out = dir.path().join("f.yaml");
        let (code, _, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--trace",
                TRACE,
            ],
            &Arc::new(failing),
        )
        .await;
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            "gadk-eval: transcript source: HTTP 403: Access Denied\n"
        );
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn export_checks_an_appended_file_before_any_source_call() {
        let dir = tempfile::tempdir().unwrap();
        let ok = "name: s\ncases:\n  - id: a\n    turns: [hi]\n    expect: [contains: x]\n";
        let unappendable = [
            (
                "name: s\ncases: [{id: a, turns: [hi], expect: [contains: x]}]\n".to_owned(),
                "`cases:` must be the last top-level key",
            ),
            (format!("{ok}...\n"), "`...` document-end marker"),
            (
                "cases:\n  - id: a\n    turns: [hi]\n    expect: [contains: x]\nname: s\n"
                    .to_owned(),
                "`cases:` must be the last top-level key",
            ),
        ];
        for (text, message) in unappendable {
            let out = dir.path().join("set.yaml");
            std::fs::write(&out, &text).unwrap();
            let fake = Arc::new(two_runs());
            let (code, stdout, stderr) = export_run(
                &[
                    "--source",
                    SOURCE,
                    "--out",
                    &path_str(&out),
                    "--append",
                    "--trace",
                    TRACE,
                    "--rubric",
                    "r",
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 2, "{text}");
            assert!(
                stderr.starts_with(&format!("gadk-eval: {}: ", out.display()))
                    && stderr.contains(message),
                "{stderr}"
            );
            assert_eq!(stdout, "");
            assert!(fake.calls().is_empty(), "{text}: {:?}", fake.calls());
            assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
        }
    }

    #[tokio::test]
    async fn export_prints_each_run_before_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("missing-dir").join("set.yaml");
        let fake = Arc::new(two_runs());
        let (code, stdout, stderr) = export_run(
            &[
                "--source",
                SOURCE,
                "--out",
                &path_str(&out),
                "--trace",
                TRACE,
                "--rubric",
                "r",
            ],
            &fake,
        )
        .await;
        assert_eq!(code, 2);
        assert_eq!(
            stdout,
            format!("added inv-e-1 (reference) trace {TRACE}\nadded inv-e-2 (judge) trace none\n")
        );
        assert!(
            stderr.starts_with(&format!("gadk-eval: {}: ", out.display())),
            "{stderr}"
        );
        assert!(!out.exists());
    }

    #[tokio::test]
    async fn backends_without_export_refuse_it() {
        let dir = tempfile::tempdir().unwrap();
        let out = path_str(&dir.path().join("x.yaml"));
        let (code, _, stderr) = cli_run(
            &["export", "--source", SOURCE, "--out", &out],
            &Fake::new(Vec::new()),
        )
        .await;
        assert_eq!(code, 2);
        assert_eq!(
            stderr,
            "gadk-eval: transcript source: export is not supported by these backends\n"
        );
    }

    #[test]
    fn backends_without_guard_source_refuse_it() {
        let Err(err) = Fake::new(Vec::new()).guard_source("p", "d", "t", 1_073_741_824) else {
            panic!("the default guard_source returned a source");
        };
        assert_eq!(
            err.to_string(),
            "transcript source: guards are not supported by these backends"
        );
    }

    #[tokio::test]
    async fn backends_without_gs_refuse_it() {
        let fake = Fake::new(Vec::new());
        let refused = "report store: gs:// sources are not supported by these backends";
        assert_eq!(
            fake.list_objects("b", "p/").await.unwrap_err().to_string(),
            refused
        );
        assert_eq!(
            fake.read_object("b", "p/a.yaml")
                .await
                .unwrap_err()
                .to_string(),
            refused
        );
    }

    #[tokio::test]
    async fn backends_without_webhooks_refuse_them() {
        let body = serde_json::json!({ "text": "hi" });
        assert_eq!(
            Fake::new(Vec::new())
                .post_webhook("https://hooks.example/T0/secret", &body)
                .await,
            Err(WEBHOOKS_UNSUPPORTED.to_owned())
        );
    }

    #[tokio::test]
    async fn cloud_backends_post_a_webhook_as_json_without_credentials() {
        use gadk_model::rest::test_util::{FakeResponse, FakeServer};

        let server = FakeServer::always(FakeResponse::new(204));
        let url = format!("{}/hooks/T0/secret?token=t", server.url());
        let body = serde_json::json!({ "text": "guards: 1 run — 1 advance" });
        assert_eq!(CloudBackends.post_webhook(&url, &body).await, Ok(204));
        let req = server.last_request();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path_and_query(), "/hooks/T0/secret?token=t");
        assert_eq!(req.header("content-type"), "application/json");
        assert_eq!(req.header("user-agent"), gadk_platform::tracking_header());
        assert_eq!(req.json(), body);
        assert!(req.header_values("authorization").is_empty(), "{req:?}");

        // A status, any status, is returned as is; a redirect is not followed.
        let moved = FakeServer::always(
            FakeResponse::new(302).header("Location", format!("{}/elsewhere", server.url())),
        );
        assert_eq!(
            CloudBackends.post_webhook(&moved.url(), &body).await,
            Ok(302)
        );
        assert_eq!(moved.hits(), 1);
        assert_eq!(server.hits(), 1);
        let failing = FakeServer::always(FakeResponse::new(500));
        assert_eq!(
            CloudBackends.post_webhook(&failing.url(), &body).await,
            Ok(500)
        );

        // A refused connection is a URL-free error kind.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hooks/T0/secret", closed.local_addr().unwrap());
        drop(closed);
        assert_eq!(
            CloudBackends.post_webhook(&url, &body).await,
            Err("connection failed".to_owned())
        );
    }

    // ---- experiment ----

    mod experiment_command {
        use std::any::Any;

        use futures::StreamExt as _;
        use futures::stream::BoxStream;
        use gadk_genai::{Content, ROLE_MODEL};
        use gadk_model::{LlmRequest, LlmResponse, ModelError};
        use gadk_prompts::{FileStore, NewVersion, PromptStore as _};

        use crate::experiment::ExperimentReport;

        use super::*;

        /// A model named `name` whose every reply is `<name>: <system instruction>`.
        struct Echo(String);

        impl Llm for Echo {
            fn name(&self) -> &str {
                &self.0
            }

            fn generate_content(
                &self,
                req: LlmRequest,
                _stream: bool,
            ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
                let instruction = req
                    .config
                    .and_then(|c| c.system_instruction)
                    .map(|c| {
                        c.parts
                            .iter()
                            .map(|p| p.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let text = format!("{}: {instruction}", self.0);
                let model = self.0.clone();
                futures::stream::once(async move {
                    Ok(LlmResponse {
                        content: Some(Content::from_text(&text, ROLE_MODEL)),
                        model_version: model,
                        ..Default::default()
                    })
                })
                .boxed()
            }

            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        /// Backends for `experiment`: [`Echo`] models (or the error `fail_model`) and a judge
        /// that always passes, recording each judge and model built.
        #[derive(Default)]
        struct Lab {
            calls: Mutex<Vec<String>>,
            fail_model: Option<String>,
        }

        impl Lab {
            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
            }
        }

        #[async_trait::async_trait]
        impl Backends for Lab {
            fn source(
                &self,
                _spec: &SourceSpec,
            ) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
                Err(MonitorError::Source("no monitor source".into()))
            }

            async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
                self.calls.lock().unwrap().push(format!(
                    "judge {} {}/{}",
                    spec.model, spec.project, spec.location
                ));
                Ok(FakeJudge::new(&[VERDICT; 10]))
            }

            async fn model(
                &self,
                project: &str,
                location: &str,
                name: &str,
            ) -> Result<Arc<dyn Llm>, MonitorError> {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("model {project}/{location}/{name}"));
                match &self.fail_model {
                    Some(e) => Err(MonitorError::Judge(format!(
                        "building {name} on Vertex AI: {e}"
                    ))),
                    None => Ok(Arc::new(Echo(name.to_owned()))),
                }
            }
        }

        /// One case whose reply must contain `concise`.
        const SMOKE: &str = "name: smoke\ncases:\n  - id: brief\n    turns: [\"Say something.\"]\n    expect: [contains: concise]\n";

        /// A prompt agent with variants on versions 1 and 2 of `exp-assistant`, on two models.
        const PROMPT_AGENT: &str = "prompts: file://prompts\n\
            agent: { name: assistant, prompt: exp-assistant }\n\
            variants:\n\
            \x20 - { name: flash-v1, model: m-flash, prompt: { version: 1 } }\n\
            \x20 - { name: hydro-v2, model: m-hydro, prompt: { version: 2 } }\n";

        /// An instruction agent with two variants on one model, two runs per case and a judge.
        const TEXT_AGENT: &str = "model: m-flash\nruns: 2\njudge: { model: m-judge }\n\
            agent: { name: assistant, instruction: Be concise. }\n\
            variants: [{ name: a }, { name: b }]\n";

        /// Writes `sets/smoke.yaml` and `exp.yaml` (`name: exp`, `set: sets/smoke.yaml`,
        /// `project: p`, then `body`) in `dir`; returns the spec path.
        fn experiment_file(dir: &Path, body: &str) -> String {
            std::fs::create_dir_all(dir.join("sets")).unwrap();
            std::fs::write(dir.join("sets/smoke.yaml"), SMOKE).unwrap();
            let p = dir.join("exp.yaml");
            std::fs::write(
                &p,
                format!("name: exp\nset: sets/smoke.yaml\nproject: p\n{body}"),
            )
            .unwrap();
            p.display().to_string()
        }

        /// A file prompt store in `dir` with versions 1 (`Be concise.`) and 2 (`Be precise.`)
        /// of `exp-assistant`.
        async fn two_versions(dir: &Path) {
            let store = FileStore::new(dir);
            for template in ["Be concise.", "Be precise."] {
                store
                    .create_version(NewVersion {
                        name: "exp-assistant".into(),
                        template: template.into(),
                        config: None,
                        author: "test".into(),
                        message: "variant".into(),
                    })
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn runs_every_variant_and_stores_the_report() {
            let dir = tempfile::tempdir().unwrap();
            two_versions(&dir.path().join("prompts")).await;
            let spec = experiment_file(dir.path(), PROMPT_AGENT);
            let out = dir.path().join("reports");
            let lab = Lab::default();
            let (code, stdout, stderr) = cli_run(
                &["experiment", &spec, "--out", &out.display().to_string()],
                &lab,
            )
            .await;
            // 0 although hydro-v2's case fails: the experiment ran.
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(
                stdout.starts_with("experiment: exp\neval set: smoke\n"),
                "{stdout}"
            );
            for text in [
                "flash-v1",
                "m-flash",
                "hydro-v2",
                "m-hydro",
                "1/1 (100%)",
                "0/1 (0%)",
            ] {
                assert!(stdout.contains(text), "{text}: {stdout}");
            }
            let last = stdout.lines().last().unwrap();
            let name = last.strip_prefix("stored report ").unwrap();
            assert!(
                name.starts_with("exp-") && name.ends_with(".json"),
                "{last}"
            );
            let json = std::fs::read_to_string(out.join("experiments").join(name)).unwrap();
            let report = ExperimentReport::from_json(&json).unwrap();
            let variants: Vec<(&str, Option<&str>, Option<&str>)> = report
                .variants
                .iter()
                .map(|v| (v.name.as_str(), v.prompt.as_deref(), v.model.as_deref()))
                .collect();
            assert_eq!(
                variants,
                [
                    ("flash-v1", Some("v1"), Some("m-flash")),
                    ("hydro-v2", Some("v2"), Some("m-hydro")),
                ]
            );
            let replies: Vec<&str> = report
                .report
                .variants
                .iter()
                .map(|v| v.runs[0].transcript.turns[0].reply.as_str())
                .collect();
            assert!(
                replies[0].starts_with("m-flash: ") && replies[0].contains("Be concise."),
                "{replies:?}"
            );
            assert!(
                replies[1].starts_with("m-hydro: ") && replies[1].contains("Be precise."),
                "{replies:?}"
            );
            // Without `judge`, the first variant's model judges in the experiment's project and
            // location; each model is built once.
            assert_eq!(
                lab.calls(),
                [
                    "judge m-flash p/us",
                    "model p/us/m-flash",
                    "model p/us/m-hydro"
                ]
            );
        }

        #[tokio::test]
        async fn an_instruction_agent_prints_the_summary_without_storing() {
            let dir = tempfile::tempdir().unwrap();
            let spec = experiment_file(dir.path(), TEXT_AGENT);
            let lab = Lab::default();
            let (code, stdout, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            // Two runs per case, both passing, for each variant.
            assert_eq!(stdout.matches("2/2 (100%)").count(), 2, "{stdout}");
            assert!(!stdout.contains("stored report"), "{stdout}");
            assert_eq!(lab.calls(), ["judge m-judge p/us", "model p/us/m-flash"]);
        }

        #[tokio::test]
        async fn the_report_is_stored_even_when_stdout_fails() {
            let dir = tempfile::tempdir().unwrap();
            let spec = experiment_file(dir.path(), TEXT_AGENT);
            let out = dir.path().join("reports");
            let argv = [
                "gadk-eval",
                "experiment",
                &spec,
                "--out",
                &out.display().to_string(),
            ]
            .map(str::to_owned)
            .to_vec();
            let mut err = Vec::new();
            let code = run(argv, &Lab::default(), &mut BrokenPipe, &mut err).await;
            assert_eq!(code, 2, "{}", String::from_utf8_lossy(&err));
            assert_eq!(
                std::fs::read_dir(out.join("experiments")).unwrap().count(),
                1
            );
        }

        #[tokio::test]
        async fn spec_and_set_errors_exit_2() {
            let dir = tempfile::tempdir().unwrap();
            let lab = Lab::default();
            let (code, _, stderr) = cli_run(&["experiment"], &lab).await;
            assert_eq!(code, 2, "{stderr}");
            let missing = dir.path().join("missing.yaml").display().to_string();
            let (code, _, stderr) = cli_run(&["experiment", &missing], &lab).await;
            assert_eq!(code, 2);
            assert!(
                stderr.starts_with("gadk-eval: monitor spec: experiment spec: reading "),
                "{stderr}"
            );
            let spec = experiment_file(dir.path(), &TEXT_AGENT.replace("runs: 2", "runs: 0"));
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: monitor spec: experiment spec: `runs` must be an integer from 1 to 20\n"
            );
            let spec = experiment_file(dir.path(), TEXT_AGENT);
            let set = dir.path().join("sets/smoke.yaml");
            let prefix = format!(
                "gadk-eval: monitor spec: experiment spec: set {}: ",
                set.display()
            );
            std::fs::remove_file(&set).unwrap();
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert!(stderr.starts_with(&prefix), "{stderr}");
            std::fs::write(&set, "name: [").unwrap();
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert!(stderr.starts_with(&prefix), "{stderr}");
            assert!(lab.calls().is_empty(), "{:?}", lab.calls());
        }

        #[tokio::test]
        async fn prompt_errors_exit_2_before_any_model_is_built() {
            let dir = tempfile::tempdir().unwrap();
            two_versions(&dir.path().join("prompts")).await;
            let lab = Lab::default();
            let spec = experiment_file(
                dir.path(),
                &PROMPT_AGENT.replace("version: 2", "version: 3"),
            );
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert!(
                stderr
                    .starts_with("gadk-eval: monitor spec: experiment spec: variants[2].prompt: "),
                "{stderr}"
            );
            // The prompt store directory is a file: the registry cannot load.
            let blocker = dir.path().join("a-file");
            std::fs::write(&blocker, "x").unwrap();
            let spec = experiment_file(
                dir.path(),
                &PROMPT_AGENT.replace("file://prompts", "file://a-file"),
            );
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert!(
                stderr.starts_with("gadk-eval: monitor spec: experiment spec: prompts: "),
                "{stderr}"
            );
            assert!(lab.calls().is_empty(), "{:?}", lab.calls());
        }

        #[tokio::test]
        async fn model_and_store_errors_exit_2() {
            let dir = tempfile::tempdir().unwrap();
            let spec = experiment_file(dir.path(), TEXT_AGENT);
            let lab = Lab {
                fail_model: Some("no Application Default Credentials".into()),
                ..Default::default()
            };
            let (code, _, stderr) = cli_run(&["experiment", &spec], &lab).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: judge: building m-flash on Vertex AI: no Application Default Credentials\n"
            );
            let blocker = dir.path().join("a-file");
            std::fs::write(&blocker, "x").unwrap();
            let out = blocker.join("sub").display().to_string();
            let (code, _, stderr) =
                cli_run(&["experiment", &spec, "--out", &out], &Lab::default()).await;
            assert_eq!(code, 2);
            assert!(stderr.starts_with("gadk-eval: report store: "), "{stderr}");
        }

        #[tokio::test]
        async fn backends_without_models_refuse_experiment() {
            let dir = tempfile::tempdir().unwrap();
            let spec = experiment_file(dir.path(), TEXT_AGENT);
            let (code, _, stderr) = cli_run(&["experiment", &spec], &Fake::new(Vec::new())).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: judge: models are not supported by these backends\n"
            );
            let (code, stdout, _) = cli_run(&["--help"], &Fake::new(Vec::new())).await;
            assert_eq!(code, 0);
            assert!(stdout.contains("experiment"), "{stdout}");
        }

        /// [`Lab`] with an evaluation service: `judge_backend` returns `backend` (recording
        /// `judge_backend <choice> <model> <project>/<location>`).
        #[derive(Default)]
        struct Serviced {
            lab: Lab,
            backend: Option<Arc<crate::testutil::FakeBackend>>,
        }

        #[async_trait::async_trait]
        impl Backends for Serviced {
            fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
                self.lab.source(spec)
            }

            async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
                self.lab.judge(spec).await
            }

            async fn model(
                &self,
                project: &str,
                location: &str,
                name: &str,
            ) -> Result<Arc<dyn Llm>, MonitorError> {
                self.lab.model(project, location, name).await
            }

            async fn judge_backend(
                &self,
                spec: &JudgeSpec,
                choice: JudgeBackendChoice,
                eval_location: &str,
            ) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
                self.lab.calls.lock().unwrap().push(format!(
                    "judge_backend {choice:?} {} {}/{eval_location}",
                    spec.model, spec.project
                ));
                let backend = self.backend.clone().unwrap();
                Ok(backend)
            }
        }

        /// One case graded by `llm_judge: Polite`.
        const JUDGED: &str = "name: judged\ncases:\n  - id: polite\n    turns: [\"Say something.\"]\n    expect: [llm_judge: Polite]\n";

        /// [`experiment_file`] with [`TEXT_AGENT`] and the [`JUDGED`] set.
        fn judged_experiment(dir: &Path) -> String {
            let spec = experiment_file(dir, TEXT_AGENT);
            std::fs::write(dir.join("sets/smoke.yaml"), JUDGED).unwrap();
            spec
        }

        #[tokio::test]
        async fn the_eval_service_grades_without_a_judge_model() {
            let dir = tempfile::tempdir().unwrap();
            let spec = judged_experiment(dir.path());
            let backends = Serviced {
                backend: Some(crate::testutil::FakeBackend::new(4)),
                ..Serviced::default()
            };
            let (code, stdout, stderr) = cli_run(
                &[
                    "experiment",
                    &spec,
                    "--judge-backend",
                    "eval-service",
                    "--eval-location",
                    "us-central1",
                ],
                &backends,
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(
                backends.lab.calls(),
                [
                    "judge_backend EvalService m-judge p/us-central1",
                    "model p/us/m-flash"
                ]
            );
            // 2 variants x 1 case x 2 runs.
            assert_eq!(backends.backend.as_ref().unwrap().calls(), 4);
            assert!(stdout.contains("2/2 (100%)"), "{stdout}");
        }

        #[tokio::test]
        async fn the_default_judge_backend_builds_the_judge_model() {
            let dir = tempfile::tempdir().unwrap();
            let spec = judged_experiment(dir.path());
            let backends = Serviced::default();
            let (code, stdout, stderr) = cli_run(&["experiment", &spec], &backends).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(
                backends.lab.calls(),
                ["judge m-judge p/us", "model p/us/m-flash"]
            );
        }

        #[tokio::test]
        async fn backends_without_the_eval_service_refuse_it() {
            let dir = tempfile::tempdir().unwrap();
            let spec = judged_experiment(dir.path());
            let lab = Lab::default();
            let (code, _, stderr) = cli_run(
                &["experiment", &spec, "--judge-backend", "eval-service"],
                &lab,
            )
            .await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: judge: the evaluation service is not supported by these backends\n"
            );
            assert!(lab.calls().is_empty(), "{:?}", lab.calls());
        }
    }

    // ---- guard ----

    mod guard_command {
        use std::collections::{BTreeMap, VecDeque};
        use std::path::PathBuf;

        use gadk_prompts::{FileStore, Label, NewVersion, PromptStore, Rollout};

        use regex::Regex;

        use crate::guard_report::GuardReport;
        use crate::guard_source::{GuardQuery, ModelTokens, VersionCalls, VersionFeedback};
        use crate::guard_spec::FeedbackSource;
        use crate::prices::TokenUsage;

        use super::*;

        /// A guard source returning fixed calls (and no feedback or runs).
        struct FakeGuardSource {
            calls: Vec<VersionCalls>,
        }

        #[async_trait::async_trait]
        impl GuardSource for FakeGuardSource {
            async fn version_calls(
                &self,
                _q: &GuardQuery,
            ) -> Result<Vec<VersionCalls>, MonitorError> {
                Ok(self.calls.clone())
            }

            async fn version_feedback(
                &self,
                _q: &GuardQuery,
                _name: &str,
                _source: FeedbackSource,
            ) -> Result<Vec<VersionFeedback>, MonitorError> {
                Ok(Vec::new())
            }

            async fn version_runs(
                &self,
                _q: &GuardQuery,
                _version: u32,
                _limit: usize,
            ) -> Result<Vec<ProductionRun>, MonitorError> {
                Ok(Vec::new())
            }
        }

        /// Backends serving a [`FakeGuardSource`], recording each `guard_source` and `judge`
        /// call; the judge fails to build with `judge_error`.
        struct Guarded {
            source: Arc<FakeGuardSource>,
            judge_error: Option<String>,
            calls: Mutex<Vec<String>>,
            /// Cloud Storage objects by `<bucket>/<name>`.
            objects: BTreeMap<String, String>,
            /// The `post_webhook` replies, in order; once they run out, every post gets 200.
            webhook: Mutex<VecDeque<Result<u16, String>>>,
            /// Every `post_webhook` call: the URL and the body.
            posts: Mutex<Vec<(String, serde_json::Value)>>,
        }

        impl Guarded {
            /// v1 and v2 with `(calls, errors)` each, p95 1000 ms, 1000 input tokens per call
            /// on `m-flash`.
            fn new(b: (u64, u64), r: (u64, u64)) -> Self {
                let side = |version, (calls, errors): (u64, u64)| {
                    VersionCalls::new(
                        version,
                        calls,
                        errors,
                        Some(1000.0),
                        vec![ModelTokens::new(
                            "m-flash",
                            calls,
                            TokenUsage::new(calls * 1000, 0, 0, 0),
                        )],
                    )
                };
                Self {
                    source: Arc::new(FakeGuardSource {
                        calls: vec![side(1, b), side(2, r)],
                    }),
                    judge_error: None,
                    calls: Mutex::new(Vec::new()),
                    objects: BTreeMap::new(),
                    webhook: Mutex::new(VecDeque::new()),
                    posts: Mutex::new(Vec::new()),
                }
            }

            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
            }

            fn posts(&self) -> Vec<(String, serde_json::Value)> {
                self.posts.lock().unwrap().clone()
            }
        }

        #[async_trait::async_trait]
        impl Backends for Guarded {
            fn source(
                &self,
                _spec: &SourceSpec,
            ) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
                Err(MonitorError::Source("no monitor source".into()))
            }

            async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("judge {}", spec.model));
                match &self.judge_error {
                    Some(e) => Err(MonitorError::Judge(e.clone())),
                    None => Ok(FakeJudge::new(&[VERDICT; 10])),
                }
            }

            fn guard_source(
                &self,
                project: &str,
                dataset: &str,
                table: &str,
                max_bytes: i64,
            ) -> Result<Arc<dyn GuardSource>, MonitorError> {
                self.calls.lock().unwrap().push(format!(
                    "guard_source {project}.{dataset}.{table} {max_bytes}"
                ));
                Ok(self.source.clone())
            }

            async fn list_objects(
                &self,
                bucket: &str,
                prefix: &str,
            ) -> Result<Vec<String>, MonitorError> {
                let start = format!("{bucket}/");
                Ok(self
                    .objects
                    .keys()
                    .filter_map(|k| k.strip_prefix(&start))
                    .filter(|n| {
                        n.strip_prefix(prefix)
                            .is_some_and(|rest| !rest.contains('/'))
                    })
                    .map(str::to_owned)
                    .collect())
            }

            async fn read_object(
                &self,
                bucket: &str,
                name: &str,
            ) -> Result<Option<String>, MonitorError> {
                Ok(self.objects.get(&format!("{bucket}/{name}")).cloned())
            }

            async fn post_webhook(
                &self,
                url: &str,
                body: &serde_json::Value,
            ) -> Result<u16, String> {
                self.posts
                    .lock()
                    .unwrap()
                    .push((url.to_owned(), body.clone()));
                self.webhook.lock().unwrap().pop_front().unwrap_or(Ok(200))
            }
        }

        /// 1000 calls per side, none failed: every check is `ok`.
        fn healthy() -> Guarded {
            Guarded::new((1000, 0), (1000, 0))
        }

        fn new_version(template: &str) -> NewVersion {
            NewVersion {
                name: "support".into(),
                template: template.into(),
                config: None,
                author: "test".into(),
                message: String::new(),
            }
        }

        /// A file prompt store at `<dir>/prompts` with prompt `support` at v1 and v2,
        /// `production` at v1 and, when `percent` is given, a rollout of v2 at that percent.
        async fn prompts(dir: &Path, percent: Option<u8>) -> FileStore {
            let s = FileStore::new(dir.join("prompts"));
            s.create_version(new_version("one")).await.unwrap();
            s.create_version(new_version("two")).await.unwrap();
            s.set_label("support", &Label::production(), 1, None)
                .await
                .unwrap();
            if let Some(p) = percent {
                let r = Rollout::new(2, p).unwrap();
                s.set_rollout("support", &Label::production(), Some(r))
                    .await
                    .unwrap();
            }
            s
        }

        /// The `production` label's version and rollout in `store`.
        async fn state(store: &FileStore) -> (Option<u32>, Option<Rollout>) {
            let (prompts, rollouts) = store.load_all_with_rollouts().await.unwrap();
            let label = prompts.prompts["support"]
                .labels
                .get(&Label::production())
                .copied();
            (
                label,
                rollouts.get("support", &Label::production()).copied(),
            )
        }

        /// Every file under `dir` with its bytes.
        fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
            let mut out = BTreeMap::new();
            let mut todo = vec![dir.to_path_buf()];
            while let Some(d) = todo.pop() {
                for entry in std::fs::read_dir(&d).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        todo.push(path);
                    } else {
                        let bytes = std::fs::read(&path).unwrap();
                        out.insert(path, bytes);
                    }
                }
            }
            out
        }

        /// Writes `g.yaml` in `dir`: guard `g` on `support`/`production` in the file store
        /// `<dir>/prompts`, ramp `[10, 25, 50]`, the YAML list `checks`.
        fn guard_file(dir: &Path, checks: &str) -> String {
            named_guard_file(dir, "g.yaml", "g", &dir.join("prompts"), checks)
        }

        /// Writes `file` in `dir`: guard `name` on `support`/`production` in the file store
        /// `prompts`, ramp `[10, 25, 50]`, then `checks` (the YAML list `checks:` and any
        /// top-level keys after it).
        fn named_guard_file(
            dir: &Path,
            file: &str,
            name: &str,
            prompts: &Path,
            checks: &str,
        ) -> String {
            let p = dir.join(file);
            std::fs::write(&p, guard_yaml(name, prompts, checks)).unwrap();
            p.display().to_string()
        }

        /// The text of [`named_guard_file`].
        fn guard_yaml(name: &str, prompts: &Path, checks: &str) -> String {
            format!(
                "name: {name}\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
                 store: file://{}\nprompt: support\nlabel: production\nwindow: 1h\n\
                 ramp: [10, 25, 50]\nchecks:\n{checks}",
                prompts.display()
            )
        }

        const ERROR_RATE: &str = "  - error_rate: { max_increase: 0.01 }\n";

        #[tokio::test]
        async fn a_dry_run_advances_exits_0_and_writes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let before = files(&dir.path().join("prompts"));
            let fake = healthy();
            let (code, stdout, stderr) = cli_run(&["guard", &spec], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(
                stdout.starts_with("guard g: support/production v1 → v2 (10%), window 1h\n"),
                "{stdout}"
            );
            assert!(
                stdout.ends_with("verdict: advance\naction: would ramp to 25%\n"),
                "{stdout}"
            );
            assert_eq!(files(&dir.path().join("prompts")), before);
            assert_eq!(fake.calls(), ["guard_source p.d.t 1073741824"]);
        }

        #[tokio::test]
        async fn apply_ramps_the_rollout() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let (code, stdout, stderr) = cli_run(&["guard", &spec, "--apply"], &healthy()).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(
                stdout.ends_with("verdict: advance\naction: ramped to 25%\n"),
                "{stdout}"
            );
            assert_eq!(state(&store).await, (Some(1), Rollout::new(2, 25).ok()));
        }

        #[tokio::test]
        async fn abort_exits_1_and_apply_removes_the_rollout() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let failing = || Guarded::new((1000, 0), (1000, 200));
            let (code, stdout, _) = cli_run(&["guard", &spec], &failing()).await;
            assert_eq!(code, 1, "{stdout}");
            assert!(
                stdout.ends_with("verdict: abort\naction: would abort\n"),
                "{stdout}"
            );
            assert_eq!(state(&store).await, (Some(1), Rollout::new(2, 10).ok()));
            let (code, stdout, _) = cli_run(&["guard", &spec, "--apply"], &failing()).await;
            assert_eq!(code, 1, "{stdout}");
            assert!(stdout.ends_with("action: aborted\n"), "{stdout}");
            assert_eq!(state(&store).await, (Some(1), None));
        }

        #[tokio::test]
        async fn wait_exits_3_and_writes_nothing_even_with_apply() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            // 50 rollout calls: fewer than min_calls (100), so the check is insufficient.
            let fake = Guarded::new((1000, 0), (50, 0));
            let (code, stdout, _) = cli_run(&["guard", &spec, "--apply"], &fake).await;
            assert_eq!(code, 3, "{stdout}");
            assert!(
                stdout.ends_with("verdict: wait\naction: none\n"),
                "{stdout}"
            );
            assert_eq!(state(&store).await, (Some(1), Rollout::new(2, 10).ok()));
        }

        #[tokio::test]
        async fn out_stores_the_report_and_prints_its_name() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let out = dir.path().join("reports");
            let (code, stdout, stderr) = cli_run(
                &["guard", &spec, "--out", &out.display().to_string()],
                &healthy(),
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            let names = ReportStore::open(&out.display().to_string())
                .unwrap()
                .guard_names(10)
                .await
                .unwrap();
            assert_eq!(names.len(), 1);
            assert!(
                stdout.ends_with(&format!("stored report {}\n", names[0])),
                "{stdout}"
            );
            assert!(names[0].starts_with("g-"), "{names:?}");
            let text = std::fs::read_to_string(out.join("guards").join(&names[0])).unwrap();
            let report = GuardReport::from_json(&text).unwrap();
            assert_eq!(
                (report.prompt.as_str(), report.baseline, report.rollout),
                ("support", 1, 2)
            );
        }

        #[tokio::test]
        async fn a_failing_out_store_after_apply_still_prints_the_decision() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            // A local store under a regular file: it opens, but writing fails.
            let blocker = dir.path().join("a-file");
            std::fs::write(&blocker, "x").unwrap();
            let out = blocker.join("sub").display().to_string();
            let (code, stdout, stderr) =
                cli_run(&["guard", &spec, "--apply", "--out", &out], &healthy()).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert!(
                stdout.ends_with("verdict: advance\naction: ramped to 25%\n"),
                "{stdout}"
            );
            assert!(stderr.starts_with("gadk-eval: report store: "), "{stderr}");
            assert_eq!(state(&store).await, (Some(1), Rollout::new(2, 25).ok()));
        }

        #[tokio::test]
        async fn prices_reach_the_cost_check() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), "  - cost_per_call: { max_increase: 0.2 }\n");
            // Without prices the cost check is unknown: wait.
            let (code, stdout, _) = cli_run(&["guard", &spec], &healthy()).await;
            assert_eq!(code, 3, "{stdout}");
            let prices = dir.path().join("prices.yaml");
            std::fs::write(&prices, "- { model: m-flash, input: 0.3, output: 2.5 }\n").unwrap();
            let (code, stdout, stderr) = cli_run(
                &["guard", &spec, "--prices", &prices.display().to_string()],
                &healthy(),
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(stdout.contains("$0.000300 (n=1000)"), "{stdout}");
        }

        #[tokio::test]
        async fn a_missing_price_file_exits_2() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let missing = dir.path().join("missing.yaml");
            let fake = healthy();
            let (code, _, stderr) = cli_run(
                &["guard", &spec, "--prices", &missing.display().to_string()],
                &fake,
            )
            .await;
            assert_eq!(code, 2);
            assert!(
                stderr.starts_with(&format!(
                    "gadk-eval: monitor spec: price file {}: ",
                    missing.display()
                )),
                "{stderr}"
            );
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }

        #[tokio::test]
        async fn max_bytes_reaches_the_source_and_must_be_positive() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let fake = healthy();
            let (code, _, _) = cli_run(&["guard", &spec, "--max-bytes", "5000"], &fake).await;
            assert_eq!(code, 0);
            assert_eq!(fake.calls(), ["guard_source p.d.t 5000"]);
            for bad in ["--max-bytes=0", "--max-bytes=-1", "--max-bytes=x"] {
                let fake = healthy();
                let (code, _, stderr) = cli_run(&["guard", &spec, bad], &fake).await;
                assert_eq!(code, 2, "{bad}");
                assert!(stderr.contains("--max-bytes"), "{bad}: {stderr}");
                assert!(fake.calls().is_empty(), "{bad}");
            }
        }

        #[tokio::test]
        async fn no_rollout_exits_4_and_stores_nothing() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), None).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let before = files(&dir.path().join("prompts"));
            let out = dir.path().join("reports");
            let fake = healthy();
            let (code, stdout, stderr) = cli_run(
                &[
                    "guard",
                    &spec,
                    "--apply",
                    "--out",
                    &out.display().to_string(),
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 4, "{stdout}{stderr}");
            assert_eq!(stdout, "guard g: support/production has no rollout\n");
            assert_eq!(stderr, "");
            assert!(!out.join("guards").exists());
            assert_eq!(files(&dir.path().join("prompts")), before);
        }

        #[tokio::test]
        async fn a_missing_prompt_or_label_exits_2_naming_them() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), None).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let other = std::fs::read_to_string(&spec)
                .unwrap()
                .replace("label: production", "label: staging");
            std::fs::write(&spec, other).unwrap();
            let (code, _, stderr) = cli_run(&["guard", &spec], &healthy()).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: report store: prompt store: support/staging: no such label\n"
            );
            let other = std::fs::read_to_string(&spec)
                .unwrap()
                .replace("prompt: support", "prompt: billing");
            std::fs::write(&spec, other).unwrap();
            let (code, _, stderr) = cli_run(&["guard", &spec], &healthy()).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: report store: prompt store: billing/staging: no such prompt\n"
            );
        }

        /// Adds `min_step_age: 1d` to the guard spec at `spec`.
        fn with_min_step_age(spec: &str) {
            let text = std::fs::read_to_string(spec).unwrap().replace(
                "ramp: [10, 25, 50]\n",
                "ramp: [10, 25, 50]\nmin_step_age: 1d\n",
            );
            std::fs::write(spec, text).unwrap();
        }

        /// Runs `guard <spec> --apply --out <out>` and checks it was held at 25% (exit 3, the
        /// note, the rollout unchanged).
        async fn held(spec: &str, out: &str, store: &FileStore) {
            let (code, stdout, stderr) =
                cli_run(&["guard", spec, "--apply", "--out", out], &healthy()).await;
            assert_eq!(code, 3, "{stdout}{stderr}");
            let summary = stdout.split("stored report").next().unwrap();
            let note = Regex::new(
                r"verdict: wait\nnote: step at 25% is \d+m old \(< min_step_age 1d\)\naction: none\n$",
            )
            .unwrap();
            assert!(note.is_match(summary), "{stdout}");
            assert_eq!(state(store).await, (Some(1), Rollout::new(2, 25).ok()));
        }

        /// Applies the ramp from 10% to 25% with `--out <out>` and returns the stored name.
        async fn ramp_to_25(spec: &str, out: &str, store: &FileStore) -> String {
            let (code, stdout, stderr) =
                cli_run(&["guard", spec, "--apply", "--out", out], &healthy()).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(state(store).await, (Some(1), Rollout::new(2, 25).ok()));
            let names = ReportStore::open(out)
                .unwrap()
                .guard_names(1)
                .await
                .unwrap();
            names[0].clone()
        }

        #[tokio::test]
        async fn a_young_step_stays_held_until_min_step_age() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let out_dir = dir.path().join("reports");
            let out = out_dir.display().to_string();
            let ramped = ramp_to_25(&spec, &out, &store).await;
            // A newer applied ramp of another spec (named `g-…` too) is skipped.
            let reports = ReportStore::open(&out).unwrap();
            let mut other = reports.find_guard(&ramped).await.unwrap().unwrap();
            other.spec = "g-other".into();
            other.generated_at -= TimeDelta::days(2);
            reports.put_guard(&other).await.unwrap();
            with_min_step_age(&spec);
            // Each held run stores a wait report; the next run still finds the applied ramp.
            held(&spec, &out, &store).await;
            held(&spec, &out, &store).await;
            // A day later (the applied report made two days ago), the guard advances.
            let path = out_dir.join("guards").join(&ramped);
            let mut report =
                GuardReport::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
            report.generated_at -= TimeDelta::days(2);
            std::fs::write(&path, report.to_json().unwrap()).unwrap();
            let (code, stdout, stderr) =
                cli_run(&["guard", &spec, "--apply", "--out", &out], &healthy()).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(!stdout.contains("note:"), "{stdout}");
            assert!(stdout.contains("action: ramped to 50%\n"), "{stdout}");
            assert_eq!(state(&store).await, (Some(1), Rollout::new(2, 50).ok()));
        }

        #[tokio::test]
        async fn a_dry_run_report_does_not_break_the_hold() {
            let dir = tempfile::tempdir().unwrap();
            let store = prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let out = dir.path().join("reports").display().to_string();
            ramp_to_25(&spec, &out, &store).await;
            let (code, stdout, stderr) =
                cli_run(&["guard", &spec, "--out", &out], &healthy()).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(stdout.contains("action: would ramp to 50%\n"), "{stdout}");
            with_min_step_age(&spec);
            held(&spec, &out, &store).await;
        }

        #[tokio::test]
        async fn min_step_age_without_out_exits_2_before_any_access() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            with_min_step_age(&spec);
            let fake = healthy();
            let (code, stdout, stderr) = cli_run(&["guard", &spec, "--apply"], &fake).await;
            assert_eq!(code, 2, "{stdout}");
            assert_eq!(
                stderr,
                "gadk-eval: monitor spec: guard spec: min_step_age needs --out\n"
            );
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }

        #[tokio::test]
        async fn spec_store_and_backend_errors_exit_2() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let missing = dir.path().join("missing.yaml").display().to_string();
            let (code, _, stderr) = cli_run(&["guard", &missing], &healthy()).await;
            assert_eq!(code, 2);
            assert!(
                stderr.starts_with("gadk-eval: monitor spec: guard spec: reading "),
                "{stderr}"
            );

            let spec = guard_file(dir.path(), ERROR_RATE);
            let bad_store = std::fs::read_to_string(&spec).unwrap().replace(
                &format!("store: file://{}", dir.path().join("prompts").display()),
                "store: s3://bucket/prompts",
            );
            let bad = dir.path().join("bad-store.yaml");
            std::fs::write(&bad, bad_store).unwrap();
            let (code, _, stderr) =
                cli_run(&["guard", &bad.display().to_string()], &healthy()).await;
            assert_eq!(code, 2);
            assert!(
                stderr.starts_with("gadk-eval: report store: prompt store: "),
                "{stderr}"
            );

            let (code, _, stderr) = cli_run(&["guard", &spec], &Fake::new(Vec::new())).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: transcript source: guards are not supported by these backends\n"
            );

            let blocker = dir.path().join("a-file");
            std::fs::write(&blocker, "x").unwrap();
            let out = blocker.join("sub").display().to_string();
            let (code, _, stderr) = cli_run(&["guard", &spec, "--out", &out], &healthy()).await;
            assert_eq!(code, 2);
            assert!(stderr.starts_with("gadk-eval: report store: "), "{stderr}");
        }

        #[tokio::test]
        async fn a_bad_prompt_store_is_reported_before_a_missing_price_file() {
            let dir = tempfile::tempdir().unwrap();
            let spec = guard_file(dir.path(), ERROR_RATE);
            let text = std::fs::read_to_string(&spec).unwrap().replace(
                &format!("store: file://{}", dir.path().join("prompts").display()),
                "store: s3://bucket/prompts",
            );
            std::fs::write(&spec, text).unwrap();
            let missing = dir.path().join("missing.yaml").display().to_string();
            let fake = healthy();
            let (code, stdout, stderr) =
                cli_run(&["guard", &spec, "--prices", &missing], &fake).await;
            assert_eq!(code, 2, "{stdout}");
            assert!(
                stderr.starts_with("gadk-eval: report store: prompt store: "),
                "{stderr}"
            );
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }

        #[tokio::test]
        async fn the_judge_is_built_only_for_llm_judge_scorers() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let mut fake = healthy();
            fake.judge_error = Some("no Application Default Credentials".into());
            let spec = guard_file(dir.path(), ERROR_RATE);
            let (code, _, _) = cli_run(&["guard", &spec], &fake).await;
            assert_eq!(code, 0);
            assert_eq!(fake.calls(), ["guard_source p.d.t 1073741824"]);

            let judged = dir.path().join("judged.yaml");
            let text = std::fs::read_to_string(&spec).unwrap()
                + "  - eval:\n      scorers: [llm_judge: be nice]\n      max_decrease: 0.05\n\
                   judge: { model: m-judge }\n";
            std::fs::write(&judged, text).unwrap();
            let (code, _, stderr) = cli_run(&["guard", &judged.display().to_string()], &fake).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: judge: no Application Default Credentials\n"
            );
            assert_eq!(fake.calls()[2], "judge m-judge");
        }

        #[tokio::test]
        async fn help_lists_guard_and_its_flags() {
            let (code, stdout, _) = cli_run(&["--help"], &healthy()).await;
            assert_eq!(code, 0);
            assert!(stdout.contains("guard"), "{stdout}");
            let (code, stdout, _) = cli_run(&["guard", "--help"], &healthy()).await;
            assert_eq!(code, 0);
            for flag in ["--apply", "--prices", "--out", "--max-bytes"] {
                assert!(stdout.contains(flag), "{flag}: {stdout}");
            }
            assert!(
                stdout.contains("Exit codes: 0 advance, 1 abort, 3 wait, 4 no rollout, 2 error."),
                "{stdout}"
            );
            let (code, _, stderr) = cli_run(&["guard"], &healthy()).await;
            assert_eq!(code, 2);
            assert!(stderr.contains("<spec>"), "{stderr}");
        }

        // ---- guards ----

        /// A `wait` check: `min_calls` above the 1000 calls per side the fake returns.
        const NOT_ENOUGH_CALLS: &str = "  - error_rate: { max_increase: 0.01 }\nmin_calls: 5000\n";

        /// Specs in `<dir>/specs`: `advance` and `wait` on the rollout at 10% in
        /// `<dir>/prompts`, and `idle` on `<dir>/idle/prompts`, whose label has no rollout.
        async fn three_specs(dir: &Path) -> (String, [String; 3]) {
            prompts(dir, Some(10)).await;
            prompts(&dir.join("idle"), None).await;
            let specs = dir.join("specs");
            std::fs::create_dir(&specs).unwrap();
            let files = [
                named_guard_file(
                    &specs,
                    "1.yaml",
                    "advance",
                    &dir.join("prompts"),
                    ERROR_RATE,
                ),
                named_guard_file(
                    &specs,
                    "2.yaml",
                    "wait",
                    &dir.join("prompts"),
                    NOT_ENOUGH_CALLS,
                ),
                named_guard_file(
                    &specs,
                    "3.yaml",
                    "idle",
                    &dir.join("idle").join("prompts"),
                    ERROR_RATE,
                ),
            ];
            (specs.display().to_string(), files)
        }

        #[tokio::test]
        async fn guards_runs_each_spec_under_its_header_and_sums_up() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, files) = three_specs(dir.path()).await;
            // What `guard` prints for each spec on its own (without --out).
            let mut alone = Vec::new();
            for file in &files {
                let (_, stdout, _) = cli_run(&["guard", file], &healthy()).await;
                alone.push(stdout);
            }
            let out = dir.path().join("reports").display().to_string();
            let fake = healthy();
            let (code, stdout, stderr) = cli_run(&["guards", &specs, "--out", &out], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            let reports = ReportStore::open(&out).unwrap();
            let stored = reports.guard_names(10).await.unwrap();
            let named = |prefix: &str| {
                stored
                    .iter()
                    .find(|n| n.starts_with(prefix))
                    .unwrap_or_else(|| panic!("{prefix}: {stored:?}"))
                    .clone()
            };
            let expected = format!(
                "== advance ==\n{}stored report {}\n\
                 == wait ==\n{}stored report {}\n\
                 == idle ==\n{}\
                 guards: 3 run — 1 advance, 0 abort, 1 wait, 1 no rollout, 0 errors\n",
                alone[0],
                named("advance-"),
                alone[1],
                named("wait-"),
                alone[2]
            );
            assert_eq!(stdout, expected);
            assert_eq!(stored.len(), 2, "{stored:?}");
            assert!(
                alone[1].ends_with("verdict: wait\naction: none\n"),
                "{}",
                alone[1]
            );
            assert_eq!(alone[2], "guard idle: support/production has no rollout\n");
        }

        #[tokio::test]
        async fn a_failing_guard_prints_its_error_and_the_rest_still_run() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let specs = dir.path().join("specs");
            std::fs::create_dir(&specs).unwrap();
            let store = dir.path().join("prompts");
            named_guard_file(&specs, "a.yaml", "first", &store, ERROR_RATE);
            // A store without the prompt.
            named_guard_file(
                &specs,
                "b.yaml",
                "broken",
                &dir.path().join("empty"),
                ERROR_RATE,
            );
            named_guard_file(&specs, "c.yaml", "third", &store, ERROR_RATE);
            let fake = healthy();
            let (code, stdout, stderr) =
                cli_run(&["guards", &specs.display().to_string()], &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert!(
                stdout.contains(
                    "== broken ==\nerror: report store: prompt store: support/production: \
                     no such prompt\n== third ==\nguard third: "
                ),
                "{stdout}"
            );
            assert!(
                stdout.ends_with(
                    "verdict: advance\naction: would ramp to 25%\n\
                     guards: 3 run — 2 advance, 0 abort, 0 wait, 0 no rollout, 1 errors\n"
                ),
                "{stdout}"
            );
        }

        #[tokio::test]
        async fn guards_reads_specs_and_prices_from_gs() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let mut fake = healthy();
            let cost = "  - cost_per_call: { max_increase: 0.2 }\n";
            fake.objects.insert(
                "evals/guards/cost.yaml".into(),
                guard_yaml("cost", &dir.path().join("prompts"), cost),
            );
            fake.objects
                .insert("evals/guards/readme.txt".into(), "not a spec".into());
            fake.objects.insert(
                "evals/prices.yaml".into(),
                "- { model: m-flash, input: 0.3, output: 2.5 }\n".into(),
            );
            let (code, stdout, stderr) = cli_run(
                &[
                    "guards",
                    "gs://evals/guards/",
                    "--prices",
                    "gs://evals/prices.yaml",
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(stdout.starts_with("== cost ==\nguard cost: "), "{stdout}");
            assert!(stdout.contains("$0.000300 (n=1000)"), "{stdout}");
            assert!(
                stdout.ends_with(
                    "guards: 1 run — 1 advance, 0 abort, 0 wait, 0 no rollout, 0 errors\n"
                ),
                "{stdout}"
            );
        }

        /// Runs `guards` with `args` and checks it exits 2 with `stderr` (exactly, or as a
        /// prefix when `prefix`), printing nothing and running no guard.
        async fn refused(args: &[&str], fake: &Guarded, stderr: &str, prefix: bool) {
            let mut argv = vec!["guards"];
            argv.extend_from_slice(args);
            let (code, out, err) = cli_run(&argv, fake).await;
            assert_eq!(code, 2, "{args:?}: {err}");
            assert_eq!(out, "", "{args:?}");
            if prefix {
                assert!(err.starts_with(stderr), "{args:?}: {err}");
            } else {
                assert_eq!(err, stderr, "{args:?}");
            }
            assert!(fake.calls().is_empty(), "{args:?}: {:?}", fake.calls());
        }

        #[tokio::test]
        async fn pre_run_checks_exit_2_before_any_guard_runs() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let store = dir.path().join("prompts");
            let a = named_guard_file(dir.path(), "a.yaml", "a", &store, ERROR_RATE);
            let fake = healthy();

            // An invalid second spec.
            let bad = dir.path().join("bad.yaml");
            std::fs::write(&bad, "name: bad\n").unwrap();
            let bad = bad.display().to_string();
            let want = format!("gadk-eval: monitor spec: guard spec {bad}: ");
            refused(&[&a, &bad], &fake, &want, true).await;

            // Two specs named `a`.
            let dup = named_guard_file(dir.path(), "dup.yaml", "a", &store, ERROR_RATE);
            let want = format!(
                "gadk-eval: monitor spec: guards: duplicate guard name a in {a} and {dup}\n"
            );
            refused(&[&a, &dup], &fake, &want, false).await;

            // 201 specs.
            let many = dir.path().join("many");
            std::fs::create_dir(&many).unwrap();
            for i in 0..=200 {
                let name = format!("g{i:03}");
                named_guard_file(&many, &format!("{name}.yaml"), &name, &store, ERROR_RATE);
            }
            let many = many.display().to_string();
            refused(
                &[&many],
                &fake,
                "gadk-eval: monitor spec: guards: more than 200 specs\n",
                false,
            )
            .await;

            // min_step_age without --out names the spec, and comes before a bad --prices.
            let aged = named_guard_file(dir.path(), "aged.yaml", "aged", &store, ERROR_RATE);
            with_min_step_age(&aged);
            let missing = dir.path().join("missing.yaml").display().to_string();
            let want =
                format!("gadk-eval: monitor spec: guard spec {aged}: min_step_age needs --out\n");
            refused(&[&a, &aged, "--prices", &missing], &fake, &want, false).await;

            // A bad --prices, local or gs://.
            let want = format!("gadk-eval: monitor spec: price file {missing}: ");
            refused(&[&a, "--prices", &missing], &fake, &want, true).await;
            refused(
                &[&a, "--prices", "gs://evals/missing.yaml"],
                &fake,
                "gadk-eval: monitor spec: price file gs://evals/missing.yaml: not found\n",
                false,
            )
            .await;

            // A bad --out.
            refused(
                &[&a, "--out", "s3://bucket/reports"],
                &fake,
                "gadk-eval: report store: unsupported store URL scheme",
                true,
            )
            .await;

            // A missing source.
            let want = format!("gadk-eval: monitor spec: guard spec {missing}: not found\n");
            refused(&[&a, &missing], &fake, &want, false).await;
        }

        #[tokio::test]
        async fn a_stdout_failure_ends_guards_with_exit_2() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let fake = healthy();
            let mut err = Vec::new();
            let argv = vec!["gadk-eval".to_owned(), "guards".to_owned(), spec];
            let code = run(argv, &fake, &mut BrokenPipe, &mut err).await;
            assert_eq!(code, 2);
            let err = String::from_utf8(err).unwrap();
            assert!(err.starts_with("gadk-eval: "), "{err}");
            assert!(!err.contains("guards: "), "{err}");
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }

        #[tokio::test]
        async fn help_lists_guards_and_its_flags() {
            let (code, stdout, _) = cli_run(&["--help"], &healthy()).await;
            assert_eq!(code, 0);
            assert!(stdout.contains("guards"), "{stdout}");
            let (code, stdout, _) = cli_run(&["guards", "--help"], &healthy()).await;
            assert_eq!(code, 0);
            for flag in ["--apply", "--prices", "--out", "--max-bytes", "<source>..."] {
                assert!(stdout.contains(flag), "{flag}: {stdout}");
            }
            assert!(
                stdout.contains(
                    "Exit codes: 0 every guard reached an outcome, 2 a guard failed or an error."
                ),
                "{stdout}"
            );
            let (code, _, stderr) = cli_run(&["guards"], &healthy()).await;
            assert_eq!(code, 2);
            assert!(stderr.contains("<source>..."), "{stderr}");
            let fake = healthy();
            let (code, _, stderr) = cli_run(&["guards", "x.yaml", "--max-bytes=0"], &fake).await;
            assert_eq!(code, 2);
            assert!(stderr.contains("--max-bytes"), "{stderr}");
        }

        /// A writer that takes `left` bytes, then fails every later write.
        struct FailsAfter {
            left: usize,
            taken: Vec<u8>,
        }

        impl Write for FailsAfter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                let n = buf.len().min(self.left);
                self.left -= n;
                self.taken.extend_from_slice(&buf[..n]);
                Ok(n)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        #[tokio::test]
        async fn a_stdout_failure_mid_report_ends_guards_with_exit_2() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let fake = healthy();
            // The `== g ==` header succeeds; the failure lands in the report.
            let mut stdout = FailsAfter {
                left: "== g ==\n".len() + 10,
                taken: Vec::new(),
            };
            let mut err = Vec::new();
            let argv = vec!["gadk-eval".to_owned(), "guards".to_owned(), spec];
            let code = run(argv, &fake, &mut stdout, &mut err).await;
            assert_eq!(code, 2);
            let taken = String::from_utf8(stdout.taken).unwrap();
            assert!(taken.starts_with("== g ==\nguard g: "), "{taken}");
            assert!(!taken.contains("error:"), "{taken}");
            assert!(!taken.contains("guards: "), "{taken}");
            let err = String::from_utf8(err).unwrap();
            assert!(err.starts_with("gadk-eval: "), "{err}");
            assert!(!err.contains("guards: "), "{err}");
        }

        // ---- guards --notify ----

        /// A webhook URL whose path is the secret.
        const HOOK: &str = "https://hooks.example/T0/B0/secret-token";

        #[tokio::test]
        async fn notify_posts_a_slack_summary_after_an_advance() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, _) = three_specs(dir.path()).await;
            let (_, plain, _) = cli_run(&["guards", &specs], &healthy()).await;
            let fake = healthy();
            let (code, stdout, stderr) =
                cli_run(&["guards", &specs, "--notify", HOOK], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(stdout, format!("{plain}notify: posted\n"));
            let text = "guards: 3 run — 1 advance, 0 abort, 1 wait, 1 no rollout, 0 errors\n\
                        • advance: support/production v1→v2 at 10% — advance: would ramp to 25%";
            assert_eq!(
                fake.posts(),
                [(HOOK.to_owned(), serde_json::json!({ "text": text }))]
            );
            // `--notify-format slack` is the default.
            let explicit = healthy();
            let args = [
                "guards",
                &specs,
                "--notify",
                HOOK,
                "--notify-format",
                "slack",
            ];
            assert_eq!(cli_run(&args, &explicit).await.0, 0);
            assert_eq!(explicit.posts(), fake.posts());
        }

        #[tokio::test]
        async fn notify_format_json_posts_the_counts_and_the_stored_report() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, _) = three_specs(dir.path()).await;
            let out = dir.path().join("reports").display().to_string();
            let fake = healthy();
            let args = [
                "guards",
                &specs,
                "--out",
                &out,
                "--notify",
                HOOK,
                "--notify-format",
                "json",
            ];
            let (code, stdout, stderr) = cli_run(&args, &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(
                stdout.ends_with(
                    "guards: 3 run — 1 advance, 0 abort, 1 wait, 1 no rollout, 0 errors\n\
                     notify: posted\n"
                ),
                "{stdout}"
            );
            let stored = ReportStore::open(&out)
                .unwrap()
                .guard_names(10)
                .await
                .unwrap();
            let advance = stored
                .iter()
                .find(|n| n.starts_with("advance-"))
                .unwrap_or_else(|| panic!("{stored:?}"));
            let want = serde_json::json!({
                "summary": {
                    "run": 3, "advance": 1, "abort": 0, "wait": 1, "no_rollout": 1, "errors": 0
                },
                "guards": [{
                    "name": "advance", "prompt": "support", "label": "production",
                    "baseline": 1, "rollout": 2, "percent": 10, "verdict": "advance",
                    "action": "would ramp to 25%", "applied": false, "report": advance
                }]
            });
            assert_eq!(fake.posts(), [(HOOK.to_owned(), want)]);
        }

        #[tokio::test]
        async fn a_quiet_run_posts_nothing_and_prints_the_same() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, files) = three_specs(dir.path()).await;
            // Leave the `wait` and `idle` specs only.
            std::fs::remove_file(&files[0]).unwrap();
            let (code, plain, _) = cli_run(&["guards", &specs], &healthy()).await;
            assert_eq!(code, 0, "{plain}");
            let fake = healthy();
            let (code, stdout, stderr) =
                cli_run(&["guards", &specs, "--notify", HOOK], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(stdout, plain);
            assert!(
                stdout.ends_with(
                    "guards: 2 run — 0 advance, 0 abort, 1 wait, 1 no rollout, 0 errors\n"
                ),
                "{stdout}"
            );
            assert!(!stdout.contains("notify:"), "{stdout}");
            assert!(fake.posts().is_empty(), "{:?}", fake.posts());
        }

        #[tokio::test]
        async fn an_error_run_posts_and_still_exits_2() {
            let dir = tempfile::tempdir().unwrap();
            let specs = dir.path().join("specs");
            std::fs::create_dir(&specs).unwrap();
            // A store without the prompt.
            named_guard_file(
                &specs,
                "b.yaml",
                "broken",
                &dir.path().join("empty"),
                ERROR_RATE,
            );
            let fake = healthy();
            let (code, stdout, stderr) = cli_run(
                &["guards", &specs.display().to_string(), "--notify", HOOK],
                &fake,
            )
            .await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(
                stdout,
                "== broken ==\n\
                 error: report store: prompt store: support/production: no such prompt\n\
                 guards: 1 run — 0 advance, 0 abort, 0 wait, 0 no rollout, 1 errors\n\
                 notify: posted\n"
            );
            let text = "guards: 1 run — 0 advance, 0 abort, 0 wait, 0 no rollout, 1 errors\n\
                        • broken: error: report store: prompt store: support/production: \
                        no such prompt";
            assert_eq!(
                fake.posts(),
                [(HOOK.to_owned(), serde_json::json!({ "text": text }))]
            );
        }

        #[tokio::test]
        async fn a_failed_post_exits_2_naming_only_the_origin() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let fake = healthy();
            fake.webhook.lock().unwrap().push_back(Ok(404));
            let (code, stdout, stderr) = cli_run(&["guards", &spec, "--notify", HOOK], &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert!(
                stdout.ends_with(
                    "guards: 1 run — 1 advance, 0 abort, 0 wait, 0 no rollout, 0 errors\n\
                     notify: POST https://hooks.example failed: http 404\n"
                ),
                "{stdout}"
            );
            for secret in ["secret-token", "/T0/"] {
                assert!(!stdout.contains(secret), "{stdout}");
                assert!(!stderr.contains(secret), "{stderr}");
            }
            assert_eq!(fake.posts().len(), 1);
        }

        #[tokio::test]
        async fn backends_without_webhooks_fail_the_post() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            // `Fake` has neither a guard source nor webhooks: the guard errors, then the default
            // is not retried, so the post fails at once.
            let (code, stdout, _) =
                cli_run(&["guards", &spec, "--notify", HOOK], &Fake::new(Vec::new())).await;
            assert_eq!(code, 2, "{stdout}");
            assert!(
                stdout.ends_with(
                    "notify: POST https://hooks.example failed: \
                     webhooks are not supported by these backends\n"
                ),
                "{stdout}"
            );
        }

        #[tokio::test]
        async fn a_bad_notify_target_exits_2_before_any_guard_runs() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let store = dir.path().join("prompts");
            let a = named_guard_file(dir.path(), "a.yaml", "a", &store, ERROR_RATE);
            let fake = healthy();
            refused(
                &[&a, "--notify", "env:GADK_EVAL_TEST_UNSET_WEBHOOK"],
                &fake,
                "gadk-eval: monitor spec: --notify: environment variable \
                 GADK_EVAL_TEST_UNSET_WEBHOOK is not set\n",
                false,
            )
            .await;
            let bad = "gadk-eval: monitor spec: --notify: expected an http:// or https:// URL \
                       or env:NAME\n";
            for target in ["ftp://hooks.example/secret-token", "env:", "env:PATH"] {
                refused(&[&a, "--notify", target], &fake, bad, false).await;
            }
            // The existing checks come first.
            refused(
                &[&a, "--out", "s3://bucket/reports", "--notify", "env:"],
                &fake,
                "gadk-eval: report store: unsupported store URL scheme",
                true,
            )
            .await;
            assert!(fake.posts().is_empty(), "{:?}", fake.posts());
        }

        #[tokio::test]
        async fn notify_format_needs_notify_and_slack_or_json() {
            let fake = healthy();
            let (code, out, stderr) =
                cli_run(&["guards", "x.yaml", "--notify-format", "json"], &fake).await;
            assert_eq!(code, 2);
            assert_eq!(out, "");
            assert!(stderr.contains("--notify <url|env:NAME>"), "{stderr}");
            let (code, _, stderr) = cli_run(
                &[
                    "guards",
                    "x.yaml",
                    "--notify",
                    HOOK,
                    "--notify-format",
                    "xml",
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 2);
            assert!(stderr.contains("--notify-format"), "{stderr}");
            assert!(fake.calls().is_empty() && fake.posts().is_empty());
        }

        #[tokio::test]
        async fn help_lists_the_notify_flags() {
            let (code, stdout, _) = cli_run(&["guards", "--help"], &healthy()).await;
            assert_eq!(code, 0);
            for text in [
                "--notify <url|env:NAME>",
                "post a summary to this webhook when a guard advances, aborts or errors",
                "--notify-format <notify-format>",
                "webhook body: slack (default) or json",
            ] {
                assert!(stdout.contains(text), "{text}: {stdout}");
            }
        }

        #[tokio::test]
        async fn a_stdout_failure_ends_guards_before_any_post() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), Some(10)).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let fake = healthy();
            let mut err = Vec::new();
            let argv = ["gadk-eval", "guards", &spec, "--notify", HOOK]
                .map(str::to_owned)
                .to_vec();
            let code = run(argv, &fake, &mut BrokenPipe, &mut err).await;
            assert_eq!(code, 2);
            assert!(fake.posts().is_empty(), "{:?}", fake.posts());
        }

        /// A guard spec in `dir` with an `llm_judge` eval check and a judge.
        async fn judged_guard(dir: &Path) -> String {
            prompts(dir, Some(10)).await;
            let spec = guard_file(dir, ERROR_RATE);
            let judged = dir.join("judged.yaml");
            let text = std::fs::read_to_string(&spec).unwrap()
                + "  - eval:\n      scorers: [llm_judge: be nice]\n      max_decrease: 0.05\n\
                   judge: { model: m-judge }\n";
            std::fs::write(&judged, text).unwrap();
            judged.display().to_string()
        }

        #[tokio::test]
        async fn guard_builds_the_chosen_judge_backend() {
            let dir = tempfile::tempdir().unwrap();
            let judged = judged_guard(dir.path()).await;
            let fake = healthy();
            let (code, _, stderr) = cli_run(
                &["guard", &judged, "--judge-backend", "eval-service"],
                &fake,
            )
            .await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: judge: the evaluation service is not supported by these backends\n"
            );
            assert_eq!(fake.calls(), ["guard_source p.d.t 1073741824"]);
        }

        #[tokio::test]
        async fn guards_builds_the_chosen_judge_backend() {
            let dir = tempfile::tempdir().unwrap();
            let judged = judged_guard(dir.path()).await;
            let fake = healthy();
            let (code, stdout, _) = cli_run(
                &[
                    "guards",
                    &judged,
                    "--judge-backend",
                    "eval-service",
                    "--eval-location",
                    "europe-west4",
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 2);
            assert!(
                stdout.contains(
                    "error: judge: the evaluation service is not supported by these backends\n"
                ),
                "{stdout}"
            );
            assert!(!fake.calls().iter().any(|c| c.starts_with("judge ")));
        }

        #[tokio::test]
        async fn guard_help_lists_the_judge_flags() {
            for command in ["guard", "guards"] {
                let (code, stdout, _) = cli_run(&[command, "--help"], &healthy()).await;
                assert_eq!(code, 0);
                for flag in ["--judge-backend", "--eval-location"] {
                    assert!(stdout.contains(flag), "{command} {flag}: {stdout}");
                }
            }
        }
    }

    // ---- alerts ----

    mod alerts_command {
        use std::collections::{BTreeMap, VecDeque};

        use crate::alert_source::{AlertQuery, AlertSource, UsageGroup};
        use crate::prices::TokenUsage;

        use super::*;

        /// A webhook URL whose path is the secret.
        const HOOK: &str = "https://hooks.example/T0/B0/secret-token";

        /// The groups of table `events`: 116 calls, 12 failed; `m-flash` and `m-pro` cost
        /// $0.11 and $0.25 with [`PRICES`], `m-new` has no price.
        fn events() -> Vec<UsageGroup> {
            vec![
                UsageGroup::new(
                    "m-flash",
                    "support",
                    Some("support/answer".into()),
                    100,
                    5,
                    TokenUsage::new(100_000, 20_000, 30_000, 0),
                ),
                UsageGroup::new(
                    "m-pro",
                    "support",
                    None,
                    10,
                    1,
                    TokenUsage::new(50_000, 0, 10_000, 5_000),
                ),
                UsageGroup::new(
                    "m-new",
                    "triage",
                    None,
                    6,
                    6,
                    TokenUsage::new(1_000, 0, 0, 0),
                ),
            ]
        }

        const PRICES: &str = "- { model: m-flash, input: 0.5, output: 2 }\n\
                              - { model: m-pro, input: 2, output: 10 }\n";

        /// An alert source answering with `groups` (or the error `error`), recording each
        /// query's window length in seconds.
        struct FakeUsage {
            groups: Result<Vec<UsageGroup>, String>,
            calls: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl AlertSource for FakeUsage {
            async fn usage(&self, q: &AlertQuery) -> Result<Vec<UsageGroup>, MonitorError> {
                // The window ends now.
                assert!((Utc::now() - q.to).num_seconds().abs() < 60, "{q:?}");
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("usage {}", (q.to - q.from).num_seconds()));
                self.groups.clone().map_err(MonitorError::Source)
            }
        }

        /// Backends serving the tables `events` and `empty`; building a source for `broken`
        /// fails, and `slow`'s query fails. Every `alert_source` and `usage` call is recorded.
        struct Alerting {
            calls: Arc<Mutex<Vec<String>>>,
            /// Cloud Storage objects by `<bucket>/<name>`.
            objects: BTreeMap<String, String>,
            /// The `post_webhook` replies, in order; once they run out, every post gets 200.
            webhook: Mutex<VecDeque<Result<u16, String>>>,
            /// Every `post_webhook` call: the URL and the body.
            posts: Mutex<Vec<(String, serde_json::Value)>>,
        }

        impl Alerting {
            fn new() -> Self {
                Self {
                    calls: Arc::new(Mutex::new(Vec::new())),
                    objects: BTreeMap::new(),
                    webhook: Mutex::new(VecDeque::new()),
                    posts: Mutex::new(Vec::new()),
                }
            }

            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
            }

            fn posts(&self) -> Vec<(String, serde_json::Value)> {
                self.posts.lock().unwrap().clone()
            }
        }

        #[async_trait::async_trait]
        impl Backends for Alerting {
            fn source(
                &self,
                _spec: &SourceSpec,
            ) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
                Err(MonitorError::Source("no monitor source".into()))
            }

            async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
                Err(MonitorError::Judge("no judge".into()))
            }

            fn alert_source(
                &self,
                project: &str,
                dataset: &str,
                table: &str,
                max_bytes: i64,
            ) -> Result<Arc<dyn AlertSource>, MonitorError> {
                self.calls.lock().unwrap().push(format!(
                    "alert_source {project}.{dataset}.{table} {max_bytes}"
                ));
                let groups = match table {
                    "events" => Ok(events()),
                    "empty" => Ok(Vec::new()),
                    "slow" => Err("query failed: timeout".to_owned()),
                    _ => return Err(MonitorError::Source("HTTP 403: denied".into())),
                };
                Ok(Arc::new(FakeUsage {
                    groups,
                    calls: self.calls.clone(),
                }))
            }

            async fn list_objects(
                &self,
                bucket: &str,
                prefix: &str,
            ) -> Result<Vec<String>, MonitorError> {
                let start = format!("{bucket}/");
                Ok(self
                    .objects
                    .keys()
                    .filter_map(|k| k.strip_prefix(&start))
                    .filter(|n| {
                        n.strip_prefix(prefix)
                            .is_some_and(|rest| !rest.contains('/'))
                    })
                    .map(str::to_owned)
                    .collect())
            }

            async fn read_object(
                &self,
                bucket: &str,
                name: &str,
            ) -> Result<Option<String>, MonitorError> {
                Ok(self.objects.get(&format!("{bucket}/{name}")).cloned())
            }

            async fn post_webhook(
                &self,
                url: &str,
                body: &serde_json::Value,
            ) -> Result<u16, String> {
                self.posts
                    .lock()
                    .unwrap()
                    .push((url.to_owned(), body.clone()));
                self.webhook.lock().unwrap().pop_front().unwrap_or(Ok(200))
            }
        }

        /// The rules of the `usage` spec: a cost rule that fires, an error rate and a calls
        /// rule that are ok, and an error rate with too few calls.
        const USAGE_RULES: &str = "\n  - name: daily-cost\n    cost: { max: 0.3 }\n\
              \x20 - error_rate: { max: 0.2 }\n\
              \x20 - name: support-calls\n    where: { agent: support }\n    calls: { max: 200 }\n\
              \x20 - name: new-errors\n    where: { model: m-new }\n    error_rate: { max: 0.5 }\n";

        /// What the `usage` spec prints after its header.
        const USAGE_LINES: &str = "daily-cost: cost $0.36 > $0.30 (6 unpriced calls) — FIRED\n\
             error_rate: error_rate 10.3% (12/116) ≤ 20.0% — ok\n\
             support-calls: calls 110 ≤ 200 — ok\n\
             new-errors: error_rate — insufficient (6 calls < min_calls 20)\n";

        /// The YAML of alert spec `name` on table `table` over `window` with `rules`.
        fn alert_yaml(name: &str, table: &str, window: &str, rules: &str) -> String {
            format!(
                "name: {name}\nsource: {{ bigquery: {{ project: p, dataset: d, table: {table} }} }}\n\
                 window: {window}\nrules: {rules}\n"
            )
        }

        /// Writes `yaml` to `dir/file` and returns its path.
        fn write(dir: &Path, file: &str, yaml: &str) -> String {
            let p = dir.join(file);
            std::fs::write(&p, yaml).unwrap();
            p.display().to_string()
        }

        /// `<dir>/specs` with `usage` (on `events` over 1d, one rule fires) and `quiet` (on
        /// `events` over 1h, ok), and `<dir>/prices.yaml`: `(specs, prices)`.
        fn two_specs(dir: &Path) -> (String, String) {
            let specs = dir.join("specs");
            std::fs::create_dir(&specs).unwrap();
            write(
                &specs,
                "1.yaml",
                &alert_yaml("usage", "events", "1d", USAGE_RULES),
            );
            write(
                &specs,
                "2.yaml",
                &alert_yaml("quiet", "events", "1h", "[tokens: { max: 1000000 }]"),
            );
            let prices = write(dir, "prices.yaml", PRICES);
            (specs.display().to_string(), prices)
        }

        #[tokio::test]
        async fn alerts_runs_each_spec_under_its_header_and_exits_1_when_a_rule_fires() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, prices) = two_specs(dir.path());
            let fake = Alerting::new();
            let (code, stdout, stderr) =
                cli_run(&["alerts", &specs, "--prices", &prices], &fake).await;
            assert_eq!(code, 1, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(
                stdout,
                format!(
                    "== usage ==\n{USAGE_LINES}\
                     == quiet ==\n\
                     tokens: tokens 196000 ≤ 1000000 — ok\n\
                     alerts: 2 specs, 5 rules — 1 fired, 3 ok, 1 insufficient, 0 errors\n"
                )
            );
            assert_eq!(
                fake.calls(),
                [
                    "alert_source p.d.events 1073741824",
                    "usage 86400",
                    "alert_source p.d.events 1073741824",
                    "usage 3600",
                ]
            );
        }

        #[tokio::test]
        async fn alerts_exits_0_when_no_rule_fires() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml(
                    "ok",
                    "events",
                    "30d",
                    "[calls: { max: 116 }, \
                     { where: { model: m-new }, error_rate: { max: 0.01 } }]",
                ),
            );
            let empty = write(
                dir.path(),
                "b.yaml",
                &alert_yaml("none", "empty", "1m", "[tokens: { max: 0 }]"),
            );
            let fake = Alerting::new();
            let (code, stdout, stderr) = cli_run(&["alerts", &file, &empty], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(
                stdout,
                "== ok ==\n\
                 calls: calls 116 ≤ 116 — ok\n\
                 error_rate: error_rate — insufficient (6 calls < min_calls 20)\n\
                 == none ==\n\
                 tokens: tokens 0 ≤ 0 — ok\n\
                 alerts: 2 specs, 3 rules — 0 fired, 2 ok, 1 insufficient, 0 errors\n"
            );
            assert_eq!(
                fake.calls(),
                [
                    "alert_source p.d.events 1073741824",
                    "usage 2592000",
                    "alert_source p.d.empty 1073741824",
                    "usage 60",
                ]
            );
        }

        #[tokio::test]
        async fn a_failing_spec_prints_its_error_and_the_rest_still_run() {
            let dir = tempfile::tempdir().unwrap();
            let specs = dir.path().join("specs");
            std::fs::create_dir(&specs).unwrap();
            write(
                &specs,
                "a.yaml",
                &alert_yaml("broken", "broken", "1h", "[calls: { max: 1 }]"),
            );
            write(
                &specs,
                "b.yaml",
                &alert_yaml(
                    "slow",
                    "slow",
                    "1h",
                    "[calls: { max: 1 }, tokens: { max: 1 }]",
                ),
            );
            write(
                &specs,
                "c.yaml",
                &alert_yaml("busy", "events", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();
            let (code, stdout, stderr) =
                cli_run(&["alerts", &specs.display().to_string()], &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(
                stdout,
                "== broken ==\n\
                 error: transcript source: HTTP 403: denied\n\
                 == slow ==\n\
                 error: transcript source: query failed: timeout\n\
                 == busy ==\n\
                 calls: calls 116 > 1 — FIRED\n\
                 alerts: 3 specs, 1 rules — 1 fired, 0 ok, 0 insufficient, 2 errors\n"
            );
        }

        #[tokio::test]
        async fn backends_without_alert_sources_error_each_spec() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml("usage", "events", "1h", "[calls: { max: 1 }]"),
            );
            let (code, stdout, _) = cli_run(&["alerts", &file], &Fake::new(Vec::new())).await;
            assert_eq!(code, 2, "{stdout}");
            assert_eq!(
                stdout,
                "== usage ==\n\
                 error: transcript source: alerts are not supported by these backends\n\
                 alerts: 1 specs, 0 rules — 0 fired, 0 ok, 0 insufficient, 1 errors\n"
            );
        }

        #[test]
        fn backends_without_alert_source_refuse_it() {
            let Err(err) = Fake::new(Vec::new()).alert_source("p", "d", "t", 1_073_741_824) else {
                panic!("the default alert_source returned a source");
            };
            assert_eq!(
                err.to_string(),
                "transcript source: alerts are not supported by these backends"
            );
        }

        #[tokio::test]
        async fn alerts_reads_specs_and_prices_from_gs_and_passes_max_bytes() {
            let mut fake = Alerting::new();
            fake.objects.insert(
                "evals/alerts/usage.yaml".into(),
                alert_yaml("usage", "events", "1d", USAGE_RULES),
            );
            fake.objects
                .insert("evals/alerts/readme.txt".into(), "not a spec".into());
            fake.objects
                .insert("evals/prices.yaml".into(), PRICES.into());
            let (code, stdout, stderr) = cli_run(
                &[
                    "alerts",
                    "gs://evals/alerts/",
                    "--prices",
                    "gs://evals/prices.yaml",
                    "--max-bytes",
                    "5000",
                ],
                &fake,
            )
            .await;
            assert_eq!(code, 1, "{stdout}{stderr}");
            assert_eq!(
                stdout,
                format!(
                    "== usage ==\n{USAGE_LINES}\
                     alerts: 1 specs, 4 rules — 1 fired, 2 ok, 1 insufficient, 0 errors\n"
                )
            );
            assert_eq!(
                fake.calls(),
                ["alert_source p.d.events 5000", "usage 86400"]
            );
        }

        /// Runs `alerts` with `args` and checks it exits 2 with `stderr` (exactly, or as a
        /// prefix when `prefix`), printing nothing, querying nothing and posting nothing.
        async fn refused(args: &[&str], fake: &Alerting, stderr: &str, prefix: bool) {
            let mut argv = vec!["alerts"];
            argv.extend_from_slice(args);
            let (code, out, err) = cli_run(&argv, fake).await;
            assert_eq!(code, 2, "{args:?}: {err}");
            assert_eq!(out, "", "{args:?}");
            if prefix {
                assert!(err.starts_with(stderr), "{args:?}: {err}");
            } else {
                assert_eq!(err, stderr, "{args:?}");
            }
            assert!(fake.calls().is_empty(), "{args:?}: {:?}", fake.calls());
            assert!(fake.posts().is_empty(), "{args:?}: {:?}", fake.posts());
        }

        #[tokio::test]
        async fn pre_run_checks_exit_2_before_any_query() {
            let dir = tempfile::tempdir().unwrap();
            let a = write(
                dir.path(),
                "a.yaml",
                &alert_yaml("a", "events", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();

            // An invalid second spec.
            let bad = write(dir.path(), "bad.yaml", "name: bad\n");
            let want = format!("gadk-eval: monitor spec: alert spec {bad}: ");
            refused(&[&a, &bad], &fake, &want, true).await;

            // Two specs named `a`.
            let dup = write(
                dir.path(),
                "dup.yaml",
                &alert_yaml("a", "events", "1h", "[tokens: { max: 1 }]"),
            );
            let want = format!(
                "gadk-eval: monitor spec: alerts: duplicate alert name a in {a} and {dup}\n"
            );
            refused(&[&a, &dup], &fake, &want, false).await;

            // 201 specs.
            let many = dir.path().join("many");
            std::fs::create_dir(&many).unwrap();
            for i in 0..=200 {
                let name = format!("a{i:03}");
                write(
                    &many,
                    &format!("{name}.yaml"),
                    &alert_yaml(&name, "events", "1h", "[calls: { max: 1 }]"),
                );
            }
            let many = many.display().to_string();
            refused(
                &[&many],
                &fake,
                "gadk-eval: monitor spec: alerts: more than 200 specs\n",
                false,
            )
            .await;

            // A cost rule without --prices names the spec, and comes before a bad --notify.
            let cost = write(
                dir.path(),
                "cost.yaml",
                &alert_yaml("cost", "events", "1h", "[cost: { max: 1 }]"),
            );
            let want =
                format!("gadk-eval: monitor spec: alert spec {cost}: cost rules need --prices\n");
            refused(&[&a, &cost, "--notify", "env:"], &fake, &want, false).await;

            // A bad --prices, local or gs://.
            let missing = dir.path().join("missing.yaml").display().to_string();
            let want = format!("gadk-eval: monitor spec: price file {missing}: ");
            refused(&[&cost, "--prices", &missing], &fake, &want, true).await;
            refused(
                &[&a, "--prices", "gs://evals/missing.yaml"],
                &fake,
                "gadk-eval: monitor spec: price file gs://evals/missing.yaml: not found\n",
                false,
            )
            .await;

            // A missing source, and a bad gs:// bucket.
            let want = format!("gadk-eval: monitor spec: alert spec {missing}: not found\n");
            refused(&[&a, &missing], &fake, &want, false).await;
            refused(
                &[&a, "gs://Bad/x.yaml"],
                &fake,
                "gadk-eval: monitor spec: alert spec gs://Bad/x.yaml: invalid bucket\n",
                false,
            )
            .await;

            // A bad --notify, checked last.
            refused(
                &[&a, "--notify", "env:GADK_EVAL_TEST_UNSET_WEBHOOK"],
                &fake,
                "gadk-eval: monitor spec: --notify: environment variable \
                 GADK_EVAL_TEST_UNSET_WEBHOOK is not set\n",
                false,
            )
            .await;
            refused(
                &[&a, "--notify", "ftp://hooks.example/secret-token"],
                &fake,
                "gadk-eval: monitor spec: --notify: expected an http:// or https:// URL or \
                 env:NAME\n",
                false,
            )
            .await;
            refused(
                &[&a, "--prices", &missing, "--notify", "env:"],
                &fake,
                &format!("gadk-eval: monitor spec: price file {missing}: "),
                true,
            )
            .await;
        }

        #[tokio::test]
        async fn notify_posts_a_slack_summary_when_a_rule_fires() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, prices) = two_specs(dir.path());
            let (_, plain, _) =
                cli_run(&["alerts", &specs, "--prices", &prices], &Alerting::new()).await;
            let fake = Alerting::new();
            let (code, stdout, stderr) = cli_run(
                &["alerts", &specs, "--prices", &prices, "--notify", HOOK],
                &fake,
            )
            .await;
            assert_eq!(code, 1, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(stdout, format!("{plain}notify: posted\n"));
            let text = "alerts: 2 specs, 5 rules — 1 fired, 3 ok, 1 insufficient, 0 errors\n\
                        • usage/daily-cost: cost $0.36 > $0.30 (6 unpriced calls)";
            assert_eq!(
                fake.posts(),
                [(HOOK.to_owned(), serde_json::json!({ "text": text }))]
            );
            // `--notify-format slack` is the default.
            let explicit = Alerting::new();
            let args = [
                "alerts",
                &specs,
                "--prices",
                &prices,
                "--notify",
                HOOK,
                "--notify-format",
                "slack",
            ];
            assert_eq!(cli_run(&args, &explicit).await.0, 1);
            assert_eq!(explicit.posts(), fake.posts());
        }

        #[tokio::test]
        async fn notify_format_json_posts_the_counts_fired_rules_and_errors() {
            let dir = tempfile::tempdir().unwrap();
            let (specs, prices) = two_specs(dir.path());
            write(
                &dir.path().join("specs"),
                "3.yaml",
                &alert_yaml("broken", "broken", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();
            let args = [
                "alerts",
                &specs,
                "--prices",
                &prices,
                "--notify",
                HOOK,
                "--notify-format",
                "json",
            ];
            let (code, stdout, stderr) = cli_run(&args, &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert!(
                stdout.ends_with(
                    "alerts: 3 specs, 5 rules — 1 fired, 3 ok, 1 insufficient, 1 errors\n\
                     notify: posted\n"
                ),
                "{stdout}"
            );
            let posts = fake.posts();
            assert_eq!(posts.len(), 1);
            let cost = posts[0].1["fired"][0]["value"].as_f64().unwrap();
            assert!((cost - 0.36).abs() < 1e-9, "{cost}");
            let want = serde_json::json!({
                "summary": {
                    "specs": 3, "rules": 5, "fired": 1, "ok": 3, "insufficient": 1, "errors": 1
                },
                "fired": [{
                    "spec": "usage", "rule": "daily-cost", "metric": "cost", "value": cost,
                    "max": 0.3, "where": {}, "unpriced_calls": 6
                }],
                "errors": [{"spec": "broken", "error": "transcript source: HTTP 403: denied"}]
            });
            assert_eq!(posts, [(HOOK.to_owned(), want)]);
        }

        #[tokio::test]
        async fn a_quiet_run_posts_nothing_and_prints_the_same() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml(
                    "quiet",
                    "events",
                    "1h",
                    "[calls: { max: 1000 }, { where: { model: m-new }, error_rate: { max: 0 } }]",
                ),
            );
            let (code, plain, _) = cli_run(&["alerts", &file], &Alerting::new()).await;
            assert_eq!(code, 0, "{plain}");
            let fake = Alerting::new();
            let (code, stdout, stderr) = cli_run(&["alerts", &file, "--notify", HOOK], &fake).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(stdout, plain);
            assert!(
                stdout.ends_with(
                    "alerts: 1 specs, 2 rules — 0 fired, 1 ok, 1 insufficient, 0 errors\n"
                ),
                "{stdout}"
            );
            assert!(fake.posts().is_empty(), "{:?}", fake.posts());
        }

        #[tokio::test]
        async fn an_error_run_posts_and_still_exits_2() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml("broken", "broken", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();
            let (code, stdout, stderr) = cli_run(&["alerts", &file, "--notify", HOOK], &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(
                stdout,
                "== broken ==\n\
                 error: transcript source: HTTP 403: denied\n\
                 alerts: 1 specs, 0 rules — 0 fired, 0 ok, 0 insufficient, 1 errors\n\
                 notify: posted\n"
            );
            let text = "alerts: 1 specs, 0 rules — 0 fired, 0 ok, 0 insufficient, 1 errors\n\
                        • broken: error: transcript source: HTTP 403: denied";
            assert_eq!(
                fake.posts(),
                [(HOOK.to_owned(), serde_json::json!({ "text": text }))]
            );
        }

        #[tokio::test]
        async fn a_failed_post_exits_2_naming_only_the_origin() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml("busy", "events", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();
            fake.webhook.lock().unwrap().push_back(Ok(404));
            let (code, stdout, stderr) = cli_run(&["alerts", &file, "--notify", HOOK], &fake).await;
            assert_eq!(code, 2, "{stdout}{stderr}");
            assert_eq!(stderr, "");
            assert_eq!(
                stdout,
                "== busy ==\n\
                 calls: calls 116 > 1 — FIRED\n\
                 alerts: 1 specs, 1 rules — 1 fired, 0 ok, 0 insufficient, 0 errors\n\
                 notify: POST https://hooks.example failed: http 404\n"
            );
            for secret in ["secret-token", "/T0/"] {
                assert!(!stdout.contains(secret), "{stdout}");
            }
            assert_eq!(fake.posts().len(), 1);
        }

        #[tokio::test]
        async fn a_stdout_failure_ends_alerts_with_exit_2_before_any_query_or_post() {
            let dir = tempfile::tempdir().unwrap();
            let file = write(
                dir.path(),
                "a.yaml",
                &alert_yaml("busy", "events", "1h", "[calls: { max: 1 }]"),
            );
            let fake = Alerting::new();
            let mut err = Vec::new();
            let argv = ["gadk-eval", "alerts", &file, "--notify", HOOK]
                .map(str::to_owned)
                .to_vec();
            let code = run(argv, &fake, &mut BrokenPipe, &mut err).await;
            assert_eq!(code, 2);
            let err = String::from_utf8(err).unwrap();
            assert!(err.starts_with("gadk-eval: "), "{err}");
            assert!(!err.contains("alerts: "), "{err}");
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
            assert!(fake.posts().is_empty(), "{:?}", fake.posts());
        }

        #[tokio::test]
        async fn help_lists_alerts_and_its_flags() {
            let fake = Alerting::new();
            let (code, stdout, _) = cli_run(&["--help"], &fake).await;
            assert_eq!(code, 0);
            assert!(stdout.contains("alerts"), "{stdout}");
            let (code, stdout, _) = cli_run(&["alerts", "--help"], &fake).await;
            assert_eq!(code, 0);
            for text in [
                "<source>...",
                "--prices <prices>",
                "--max-bytes <max-bytes>",
                "--notify <url|env:NAME>",
                "post a summary to this webhook when a rule fires or a spec errors",
                "--notify-format <notify-format>",
                "Exit codes: 0 no rule fired, 1 a rule fired, 2 a spec failed or an error.",
            ] {
                assert!(stdout.contains(text), "{text}: {stdout}");
            }
            assert!(!stdout.contains("--apply"), "{stdout}");
            assert!(!stdout.contains("--out"), "{stdout}");
        }

        #[tokio::test]
        async fn usage_errors_exit_2() {
            let fake = Alerting::new();
            for args in [
                &["alerts"][..],
                &["alerts", "x.yaml", "--max-bytes=0"],
                &["alerts", "x.yaml", "--notify-format", "json"],
                &[
                    "alerts",
                    "x.yaml",
                    "--notify",
                    HOOK,
                    "--notify-format",
                    "xml",
                ],
                &["alerts", "x.yaml", "--out", "reports"],
            ] {
                let (code, out, stderr) = cli_run(args, &fake).await;
                assert_eq!(code, 2, "{args:?}");
                assert_eq!(out, "", "{args:?}");
                assert!(!stderr.is_empty(), "{args:?}");
            }
            assert!(fake.calls().is_empty() && fake.posts().is_empty());
        }
    }

    #[tokio::test]
    async fn list_full_names_puts_the_prefix_back() {
        use crate::gcp::NoAuth;
        use crate::testutil::{exchange, replay};

        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange(
                "GET",
                "https://storage.googleapis.com/storage/v1/b/evals/o?prefix=p%2F&delimiter=%2F",
                "",
                200,
                r#"{"items":[{"name":"p/a.yaml"}]}"#,
            )],
        );
        let gcp = GcpHttp::new(http, Arc::new(NoAuth));
        assert_eq!(
            list_full_names(&gcp, "evals", "p/").await.unwrap(),
            ["p/a.yaml"]
        );
    }

    // ---- judge backends ----

    mod judge_backends {
        use gadk_eval::{JudgeKind, JudgeOutcome, JudgeRequest};

        use crate::testutil::FakeBackend;

        use super::*;

        /// [`Fake`] whose `judge_backend` returns `backend` for the evaluation service,
        /// recording each `judge` and `judge_backend` call.
        struct Serviced {
            fake: Fake,
            backend: Arc<FakeBackend>,
            calls: Mutex<Vec<String>>,
        }

        impl Serviced {
            fn new(runs: Vec<ProductionRun>) -> Self {
                Self {
                    fake: Fake::new(runs),
                    backend: FakeBackend::new(4),
                    calls: Mutex::new(Vec::new()),
                }
            }

            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
            }
        }

        #[async_trait::async_trait]
        impl Backends for Serviced {
            fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
                self.fake.source(spec)
            }

            async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("judge {}", spec.model));
                self.fake.judge(spec).await
            }

            async fn judge_backend(
                &self,
                spec: &JudgeSpec,
                choice: JudgeBackendChoice,
                eval_location: &str,
            ) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
                match choice {
                    JudgeBackendChoice::EvalService => {
                        self.calls.lock().unwrap().push(format!(
                            "eval-service {} {}/{eval_location}",
                            spec.model, spec.project
                        ));
                        Ok(self.backend.clone())
                    }
                    _ => gemini_judge(self.judge(spec).await?, spec),
                }
            }
        }

        /// A monitor spec in `dir` with a judge and an `llm_judge` scorer.
        fn judged_spec(dir: &Path) -> String {
            let p = dir.join("judged.yaml");
            std::fs::write(
                &p,
                "name: support\nsource: { bigquery: { project: p, dataset: d, table: t } }\n\
                 window: 1d\njudge: { model: m-judge }\nscorers: [llm_judge: Polite]\n",
            )
            .unwrap();
            p.display().to_string()
        }

        fn judge_spec() -> JudgeSpec {
            JudgeSpec {
                model: "m-judge".into(),
                project: "p".into(),
                location: "us-central1".into(),
                timeout: Duration::from_secs(5),
            }
        }

        #[test]
        fn judge_flags_parse_with_defaults() {
            for argv in [
                &["gadk-eval", "monitor", "m.yaml"][..],
                &["gadk-eval", "experiment", "e.yaml"],
                &["gadk-eval", "guard", "g.yaml"],
                &["gadk-eval", "guards", "g.yaml"],
            ] {
                let m = cli().try_get_matches_from(argv).unwrap();
                let flags = JudgeFlags::of(m.subcommand().unwrap().1);
                assert_eq!(flags.backend, JudgeBackendChoice::Gemini, "{argv:?}");
                assert_eq!(flags.location, "global", "{argv:?}");
                let mut argv = argv.to_vec();
                argv.extend([
                    "--judge-backend",
                    "eval-service",
                    "--eval-location",
                    "us-central1",
                ]);
                let m = cli().try_get_matches_from(&argv).unwrap();
                let flags = JudgeFlags::of(m.subcommand().unwrap().1);
                assert_eq!(flags.backend, JudgeBackendChoice::EvalService, "{argv:?}");
                assert_eq!(flags.location, "us-central1", "{argv:?}");
            }
            assert_eq!(
                JudgeBackendChoice::from_flag("gemini"),
                Some(JudgeBackendChoice::Gemini)
            );
            assert_eq!(JudgeBackendChoice::from_flag("vertex"), None);
            assert_eq!(JudgeBackendChoice::default(), JudgeBackendChoice::Gemini);
        }

        #[tokio::test]
        async fn an_unknown_judge_backend_is_a_usage_error() {
            let (code, stdout, stderr) = cli_run(
                &["monitor", "m.yaml", "--judge-backend", "vertex"],
                &Fake::new(Vec::new()),
            )
            .await;
            assert_eq!((code, stdout.as_str()), (2, ""));
            assert!(stderr.contains("invalid value 'vertex'"), "{stderr}");
        }

        #[tokio::test]
        async fn the_default_judge_backend_wraps_the_judge_model() {
            let fake = Fake::new(Vec::new());
            let backend = fake
                .judge_backend(&judge_spec(), JudgeBackendChoice::Gemini, "global")
                .await
                .unwrap();
            let req = JudgeRequest::new(
                JudgeKind::Rubric {
                    rubric: "Polite".into(),
                },
                "User: hi\nAgent: hello\n\n",
                "hello",
            );
            assert_eq!(
                backend.grade(&req).await,
                JudgeOutcome::Verdict {
                    pass: true,
                    score: 5,
                    rationale: "ok".into()
                }
            );
            let err = fake
                .judge_backend(&judge_spec(), JudgeBackendChoice::EvalService, "global")
                .await
                .err()
                .unwrap();
            assert_eq!(
                err.to_string(),
                "judge: the evaluation service is not supported by these backends"
            );
            let err = fake
                .pairwise_judge(&judge_spec(), "global")
                .await
                .err()
                .unwrap();
            assert_eq!(
                err.to_string(),
                "judge: the evaluation service is not supported by these backends"
            );
        }

        #[tokio::test]
        async fn monitor_grades_through_the_eval_service() {
            let dir = tempfile::tempdir().unwrap();
            let spec = judged_spec(dir.path());
            let backends = Serviced::new(vec![passing("a"), failing("b")]);
            let (code, stdout, stderr) = cli_run(
                &[
                    "monitor",
                    &spec,
                    "--judge-backend",
                    "eval-service",
                    "--eval-location",
                    "us-central1",
                ],
                &backends,
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(backends.calls(), ["eval-service m-judge p/us-central1"]);
            assert_eq!(backends.backend.calls(), 2);
            assert!(
                stdout.contains("llm_judge      pass 2  fail 0  error 0"),
                "{stdout}"
            );
        }

        #[tokio::test]
        async fn monitor_defaults_to_the_judge_model() {
            let dir = tempfile::tempdir().unwrap();
            let spec = judged_spec(dir.path());
            let backends = Serviced::new(vec![passing("a")]);
            let (code, stdout, stderr) =
                cli_run(&["monitor", &spec, "--eval-location", "x"], &backends).await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert_eq!(backends.calls(), ["judge m-judge"]);
            assert_eq!(backends.backend.calls(), 0);
        }

        #[tokio::test]
        async fn monitor_without_a_judge_builds_no_backend() {
            let dir = tempfile::tempdir().unwrap();
            let spec = spec_file(dir.path(), "");
            let backends = Serviced::new(vec![passing("a")]);
            let (code, stdout, stderr) = cli_run(
                &["monitor", &spec, "--judge-backend", "eval-service"],
                &backends,
            )
            .await;
            assert_eq!(code, 0, "{stdout}{stderr}");
            assert!(backends.calls().is_empty(), "{:?}", backends.calls());
        }

        #[tokio::test]
        async fn monitor_help_lists_the_judge_flags() {
            for command in ["monitor", "experiment"] {
                let (code, stdout, _) = cli_run(&[command, "--help"], &Fake::new(Vec::new())).await;
                assert_eq!(code, 0);
                for flag in ["--judge-backend", "--eval-location"] {
                    assert!(stdout.contains(flag), "{command} {flag}: {stdout}");
                }
            }
        }
    }
}
