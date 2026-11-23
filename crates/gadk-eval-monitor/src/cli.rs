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
//! gadk-eval monitor <spec.yaml> [--out <url>] [--limit N]
//! gadk-eval report <url> [<name>] [--limit N]
//! gadk-eval export --source bigquery://<project>.<dataset>.<table> --out <file> [--append]
//!     [--name N] [--trace ID]... [--since <n>h|<n>d] [--agent A] [--score F]... [--limit N]
//!     [--rubric T] [--min-good X] [--no-history] [--tool-args]
//! gadk-eval experiment <spec.yaml> [--out <url>]
//! gadk-eval guard <spec.yaml> [--apply] [--prices <file>] [--out <url>] [--max-bytes <n>]
//! ```
//!
//! `monitor` exits 0 on success, 1 when the spec's `min_pass_rate` is set and either the pass
//! rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0), and 2 when the spec cannot be loaded, the runs cannot be
//! listed, the judge cannot be built or the report cannot be stored. `report` exits 0, or 2 on a
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
//! `--max-bytes` bytes (default 1073741824). It exits 0 when the verdict is `advance`, 1 when it
//! is `abort` and 3 when it is `wait` (applied or not), and 2 on an invalid spec, an unreadable
//! price file, a prompt store error (including no rollout on the label, or a store changed since
//! it was read), a query or judge error, or a report store error. Usage errors exit 2.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use gadk_agent::Agent;
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_model::Llm;
use gadk_prompts::{PromptRegistry, Selector, StoreError};

use crate::error::MonitorError;
use crate::experiment::run_experiment;
use crate::experiment_spec::{AgentInstruction, ExperimentSpec};
use crate::export::{
    ExportOptions, ExportSource, ScoreFilter, cases_text, export_runs, write_set_file,
};
use crate::guard::Guard;
use crate::guard_source::GuardSource;
use crate::guard_spec::GuardSpec;
use crate::judge::vertex_judge;
use crate::monitor::Monitor;
use crate::prices::PriceTable;
use crate::source::bigquery::{BigQueryGuardSource, BigQueryTranscriptSource};
use crate::source::cloud_trace::CloudTraceTranscriptSource;
use crate::source::{TranscriptQuery, TranscriptSource};
use crate::spec::{
    DEFAULT_JUDGE_TIMEOUT, JudgeSpec, MAX_LIMIT, MonitorSpec, SourceSpec, amount,
    is_dataset_or_table, is_project_id,
};
use crate::store::ReportStore;

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
                .arg(limit_arg().help("runs to grade, 1-1000 (overrides the spec)")),
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
                ),
        )
        .subcommand(guard_cli())
}

fn guard_cli() -> Command {
    Command::new("guard")
        .about("Compare both sides of a prompt rollout and advance, abort or wait")
        .arg(Arg::new("spec").required(true).help("guard spec YAML file"))
        .arg(
            Arg::new("apply")
                .long("apply")
                .action(ArgAction::SetTrue)
                .help("write the decision to the prompt store (ramp, promote or abort)"),
        )
        .arg(
            Arg::new("prices")
                .long("prices")
                .value_parser(value_parser!(PathBuf))
                .help("price file (YAML) for cost_per_call checks"),
        )
        .arg(
            Arg::new("out")
                .long("out")
                .help("store the report: <dir>, file://<dir> or gs://<bucket>[/<prefix>]"),
        )
        .arg(
            Arg::new("max-bytes")
                .long("max-bytes")
                .value_parser(value_parser!(i64).range(1..))
                .help("most bytes billed per BigQuery query (default 1073741824)"),
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
            monitor(spec, out, limit(m), backends, stdout).await
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
            experiment(spec, out, backends, stdout).await
        }
        Some(("guard", m)) => guard(m, backends, stdout).await,
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

/// `monitor`: 0 or 1 (below `min_pass_rate`); errors map to 2.
async fn monitor(
    spec_path: &str,
    out: Option<&str>,
    limit: Option<usize>,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let mut spec = MonitorSpec::load(spec_path)?;
    if let Some(n) = limit {
        spec.limit = n;
    }
    let store = out.map(ReportStore::open).transpose()?;
    let source = backends.source(&spec.source)?;
    let judge = match &spec.judge {
        Some(j) => Some(backends.judge(j).await?),
        None => None,
    };
    let report = Monitor::new().run(&spec, source.as_ref(), judge).await?;
    // Store before printing, so a failing stdout (a closed pipe) does not lose the report.
    let stored = match store {
        Some(store) => Some(store.put(&report).await?),
        None => None,
    };
    write!(stdout, "{}", report.summary())?;
    if let Some(name) = stored {
        writeln!(stdout, "stored report {name}")?;
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

/// `guard`: 0 (advance), 1 (abort) or 3 (wait); errors map to 2.
async fn guard(
    m: &ArgMatches,
    backends: &dyn Backends,
    stdout: &mut dyn Write,
) -> Result<i32, MonitorError> {
    let spec_path = m.get_one::<String>("spec").map_or("", String::as_str);
    let spec = GuardSpec::load(spec_path)?;
    // Opened before the run, so a bad URL fails before anything is written.
    let out = m
        .get_one::<String>("out")
        .map(|url| ReportStore::open(url))
        .transpose()?;
    let store = gadk_prompts_cli::open_store(&spec.store)
        .map_err(|e| MonitorError::Store(format!("prompt store: {e}")))?;
    let prices = m
        .get_one::<PathBuf>("prices")
        .map(PriceTable::load)
        .transpose()?;
    let max_bytes = m
        .get_one::<i64>("max-bytes")
        .copied()
        .unwrap_or(BigQueryGuardSource::DEFAULT_MAX_BYTES_BILLED);
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
    let source = backends.guard_source(project, dataset, table, max_bytes)?;
    let judge = match &spec.judge {
        Some(j) if spec.wants_judge() => Some(backends.judge(j).await?),
        _ => None,
    };
    let report = Guard::new()
        .run(
            &spec,
            store.as_ref(),
            source.as_ref(),
            prices.as_ref(),
            judge,
            m.get_flag("apply"),
        )
        .await?;
    // Store before printing, so a failing stdout (a closed pipe) does not lose the report.
    let stored = match out {
        Some(out) => Some(out.put_guard(&report).await?),
        None => None,
    };
    write!(stdout, "{}", report.summary())?;
    if let Some(name) = stored {
        writeln!(stdout, "stored report {name}")?;
    }
    Ok(report.exit_code())
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
    let result = run_spec(&spec, &set, registry.as_ref(), out, backends, stdout).await;
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
    let judge = backends.judge(&spec.judge).await?;
    let mut models: HashMap<&str, Arc<dyn Llm>> = HashMap::new();
    for name in spec.variants.iter().filter_map(Variant::model) {
        if !models.contains_key(name) {
            let model = backends.model(&spec.project, &spec.location, name).await?;
            models.insert(name, model);
        }
    }
    let evaluator = Evaluator::new(judge).runs_per_case(spec.runs);
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
    use crate::testutil::{FakeJudge, FakeSource, at, run as prod_run};

    use super::*;

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
    }

    // ---- guard ----

    mod guard_command {
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        use gadk_prompts::{FileStore, Label, NewVersion, PromptStore, Rollout};

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
                }
            }

            fn calls(&self) -> Vec<String> {
                self.calls.lock().unwrap().clone()
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
            let p = dir.join("g.yaml");
            std::fs::write(
                &p,
                format!(
                    "name: g\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
                     store: file://{}\nprompt: support\nlabel: production\nwindow: 1h\n\
                     ramp: [10, 25, 50]\nchecks:\n{checks}",
                    dir.join("prompts").display()
                ),
            )
            .unwrap();
            p.display().to_string()
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
        async fn a_missing_rollout_prompt_or_label_exits_2_naming_them() {
            let dir = tempfile::tempdir().unwrap();
            prompts(dir.path(), None).await;
            let spec = guard_file(dir.path(), ERROR_RATE);
            let (code, _, stderr) = cli_run(&["guard", &spec], &healthy()).await;
            assert_eq!(code, 2);
            assert_eq!(
                stderr,
                "gadk-eval: report store: prompt store: support/production: the label has no \
                 rollout\n"
            );
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
            let (code, _, stderr) = cli_run(&["guard"], &healthy()).await;
            assert_eq!(code, 2);
            assert!(stderr.contains("<spec>"), "{stderr}");
        }
    }
}
