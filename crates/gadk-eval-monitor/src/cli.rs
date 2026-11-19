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
//! ```
//!
//! `monitor` exits 0 on success, 1 when the spec's `min_pass_rate` is set and either the pass
//! rate is below it or there are no runs (without `min_pass_rate`, zero runs exit 0), and 2 when the spec cannot be loaded, the runs cannot be
//! listed, the judge cannot be built or the report cannot be stored. `report` exits 0, or 2 on a
//! store error or an unknown or invalid report name. Usage errors exit 2.

use std::io::Write;
use std::sync::Arc;

use clap::{Arg, Command, value_parser};
use gadk_model::Llm;

use crate::error::MonitorError;
use crate::judge::vertex_judge;
use crate::monitor::Monitor;
use crate::source::TranscriptSource;
use crate::source::bigquery::BigQueryTranscriptSource;
use crate::source::cloud_trace::CloudTraceTranscriptSource;
use crate::spec::{JudgeSpec, MAX_LIMIT, MonitorSpec, SourceSpec};
use crate::store::ReportStore;

/// The default number of reports `report` lists.
const DEFAULT_REPORT_LIMIT: usize = 20;

/// What the `monitor` command builds from a spec: replaced in tests.
#[async_trait::async_trait]
pub trait Backends: Send + Sync {
    /// The transcript source for `spec`.
    fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError>;

    /// The judge model for `spec`.
    async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError>;
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::source::ProductionRun;
    use crate::testutil::{FakeJudge, FakeSource, run as prod_run};

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
}
