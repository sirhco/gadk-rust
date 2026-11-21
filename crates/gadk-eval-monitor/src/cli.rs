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
//! ```
//!
//! `monitor` exits 0 on success, 1 when the spec's `min_pass_rate` is set and either the pass
//! rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0), and 2 when the spec cannot be loaded, the runs cannot be
//! listed, the judge cannot be built or the report cannot be stored. `report` exits 0, or 2 on a
//! store error or an unknown or invalid report name. `export` exits 0 when it added at least one
//! case, 1 when it added none (and wrote nothing), and 2 when the eval-set file cannot be read,
//! extended or written or the source fails. Usage errors exit 2.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use gadk_eval::EvalSet;
use gadk_model::Llm;

use crate::error::MonitorError;
use crate::export::{
    ExportOptions, ExportSource, ScoreFilter, cases_text, export_runs, write_set_file,
};
use crate::judge::vertex_judge;
use crate::monitor::Monitor;
use crate::source::bigquery::BigQueryTranscriptSource;
use crate::source::cloud_trace::CloudTraceTranscriptSource;
use crate::source::{TranscriptQuery, TranscriptSource};
use crate::spec::{
    JudgeSpec, MAX_LIMIT, MonitorSpec, SourceSpec, amount, is_dataset_or_table, is_project_id,
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
        .arg(Arg::new("name").long("name").help("name of the new eval set (default: the file stem)"))
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
    for line in &lines {
        writeln!(stdout, "{line}")?;
    }
    writeln!(stdout, "{} cases added to {}", added.len(), out.display())?;
    Ok(if added.is_empty() { 1 } else { 0 })
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
    async fn cli_run(args: &[&str], backends: &Fake) -> (i32, String, String) {
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
        let cases: [&[&str]; 10] = [
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
}
