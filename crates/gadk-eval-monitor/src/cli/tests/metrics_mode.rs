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

//! `--write-metrics` and `--metrics-project` on `monitor`, and the helpers the `guard`, `guards`
//! and `alerts` metrics tests share.

use futures::future::BoxFuture;

use crate::export::ExportSource;
use crate::guard_source::GuardSource;
use crate::metrics::{MetricPoint, MetricValue, MetricsWriter};

use super::*;

/// A stdout that can be read while the command runs.
#[derive(Clone, Default)]
pub(super) struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl SharedOut {
    pub(super) fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One [`MetricsWriter::write`] call.
#[derive(Debug, Clone)]
pub(super) struct Written {
    pub(super) project: String,
    pub(super) points: Vec<MetricPoint>,
    /// What the command had printed to stdout when the call was made.
    pub(super) stdout: String,
}

impl Written {
    pub(super) fn metrics(&self) -> Vec<&str> {
        self.points.iter().map(|p| p.metric.as_str()).collect()
    }
}

/// A writer recording each call, failing every write with `fail` when set.
struct FakeMetrics {
    stdout: SharedOut,
    fail: Option<String>,
    calls: Mutex<Vec<Written>>,
}

impl MetricsWriter for FakeMetrics {
    fn write<'a>(
        &'a self,
        project: &'a str,
        points: &'a [MetricPoint],
    ) -> BoxFuture<'a, Result<(), MonitorError>> {
        self.calls.lock().unwrap().push(Written {
            project: project.to_owned(),
            points: points.to_vec(),
            stdout: self.stdout.text(),
        });
        let result = match &self.fail {
            Some(e) => Err(MonitorError::Store(e.clone())),
            None => Ok(()),
        };
        Box::pin(async move { result })
    }
}

/// `inner`'s backends with a [`FakeMetrics`] writer (or a writer that fails to build),
/// counting each [`Backends::metrics_writer`] call.
pub(super) struct Metered<'a> {
    inner: &'a dyn Backends,
    out: SharedOut,
    writer: Result<Arc<FakeMetrics>, String>,
    builds: Mutex<usize>,
}

impl<'a> Metered<'a> {
    /// A writer that succeeds.
    pub(super) fn new(inner: &'a dyn Backends) -> Self {
        Self::with(inner, Ok(None))
    }

    /// A writer whose every write fails with `report store: <message>`.
    pub(super) fn failing(inner: &'a dyn Backends, message: &str) -> Self {
        Self::with(inner, Ok(Some(message.to_owned())))
    }

    /// A writer that fails to build with `report store: <message>`.
    pub(super) fn refusing(inner: &'a dyn Backends, message: &str) -> Self {
        Self::with(inner, Err(message.to_owned()))
    }

    fn with(inner: &'a dyn Backends, writer: Result<Option<String>, String>) -> Self {
        let out = SharedOut::default();
        let writer = writer.map(|fail| {
            Arc::new(FakeMetrics {
                stdout: out.clone(),
                fail,
                calls: Mutex::new(Vec::new()),
            })
        });
        Self {
            inner,
            out,
            writer,
            builds: Mutex::new(0),
        }
    }

    /// Runs the CLI with these backends: `(exit code, stdout, stderr)`. Run it once per
    /// `Metered`.
    pub(super) async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let mut err = Vec::new();
        let mut argv = vec!["gadk-eval".to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let code = run(argv, self, &mut self.out.clone(), &mut err).await;
        (code, self.out.text(), String::from_utf8(err).unwrap())
    }

    /// Every write call.
    pub(super) fn writes(&self) -> Vec<Written> {
        match &self.writer {
            Ok(w) => w.calls.lock().unwrap().clone(),
            Err(_) => Vec::new(),
        }
    }

    /// The number of [`Backends::metrics_writer`] calls.
    pub(super) fn builds(&self) -> usize {
        *self.builds.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl Backends for Metered<'_> {
    fn source(&self, spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
        self.inner.source(spec)
    }

    async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
        self.inner.judge(spec).await
    }

    async fn judge_backend(
        &self,
        spec: &JudgeSpec,
        choice: JudgeBackendChoice,
        eval_location: &str,
    ) -> Result<Arc<dyn JudgeBackend>, MonitorError> {
        self.inner.judge_backend(spec, choice, eval_location).await
    }

    async fn pairwise_judge(
        &self,
        spec: &JudgeSpec,
        eval_location: &str,
    ) -> Result<Arc<dyn PairwiseJudge>, MonitorError> {
        self.inner.pairwise_judge(spec, eval_location).await
    }

    async fn batch_predictor(
        &self,
        spec: &JudgeSpec,
        staging: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn BatchPredictor>, MonitorError> {
        self.inner.batch_predictor(spec, staging, timeout).await
    }

    fn score_sink(&self, spec: &SourceSpec) -> Result<Arc<dyn ScoreSink>, MonitorError> {
        self.inner.score_sink(spec)
    }

    fn export_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
    ) -> Result<Arc<dyn ExportSource>, MonitorError> {
        self.inner.export_source(project, dataset, table)
    }

    async fn model(
        &self,
        project: &str,
        location: &str,
        name: &str,
    ) -> Result<Arc<dyn Llm>, MonitorError> {
        self.inner.model(project, location, name).await
    }

    fn guard_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn GuardSource>, MonitorError> {
        self.inner.guard_source(project, dataset, table, max_bytes)
    }

    fn alert_source(
        &self,
        project: &str,
        dataset: &str,
        table: &str,
        max_bytes: i64,
    ) -> Result<Arc<dyn AlertSource>, MonitorError> {
        self.inner.alert_source(project, dataset, table, max_bytes)
    }

    async fn list_objects(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, MonitorError> {
        self.inner.list_objects(bucket, prefix).await
    }

    async fn read_object(&self, bucket: &str, name: &str) -> Result<Option<String>, MonitorError> {
        self.inner.read_object(bucket, name).await
    }

    async fn post_webhook(&self, url: &str, body: &serde_json::Value) -> Result<u16, String> {
        self.inner.post_webhook(url, body).await
    }

    fn metrics_writer(&self) -> Result<Arc<dyn MetricsWriter>, MonitorError> {
        *self.builds.lock().unwrap() += 1;
        match &self.writer {
            Ok(w) => Ok(w.clone()),
            Err(e) => Err(MonitorError::Store(e.clone())),
        }
    }
}

/// `stdout` without its `window … .. …` line, which names the run's own time.
fn timeless(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|l| !l.starts_with("window "))
        .map(|l| format!("{l}\n"))
        .collect()
}

#[test]
fn the_default_metrics_writer_refuses() {
    let Err(e) = Fake::new(Vec::new()).metrics_writer() else {
        panic!("the default built a writer");
    };
    assert_eq!(
        e.to_string(),
        "report store: Cloud Monitoring metrics are not supported by these backends"
    );
}

#[tokio::test]
async fn monitor_writes_once_after_the_report_is_stored_and_printed() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "agent: support-agent\n");
    let out = dir.path().join("reports").display().to_string();
    let fake = Fake::new(vec![passing("a"), failing("b")]);
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&["monitor", &spec, "--out", &out, "--write-metrics"])
        .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stderr, "");
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].project, "p");
    assert_eq!(
        writes[0].metrics(),
        [
            "monitor/runs",
            "monitor/errors",
            "monitor/pass_rate",
            "monitor/scorer_pass_rate",
        ]
    );
    let labels = &writes[0].points[3].labels;
    assert_eq!(labels["spec"], "support");
    assert_eq!(labels["agent"], "support-agent");
    assert_eq!(labels["scorer"], "contains");
    // Written after the `stored report` line, which is the last thing printed.
    assert_eq!(writes[0].stdout, stdout);
    assert!(
        stdout
            .lines()
            .last()
            .unwrap()
            .starts_with("stored report support-"),
        "{stdout}"
    );
    assert_eq!(metered.builds(), 1);
}

#[tokio::test]
async fn monitor_below_the_minimum_still_writes_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "min_pass_rate: 0.9\n");
    let fake = Fake::new(vec![passing("a"), failing("b")]);
    let metered = Metered::new(&fake);
    let (code, stdout, _) = metered.run(&["monitor", &spec, "--write-metrics"]).await;
    assert_eq!(code, 1, "{stdout}");
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert!(!writes[0].stdout.contains("FAILED"), "{}", writes[0].stdout);
    assert!(stdout.contains("FAILED: pass rate 50.0%"), "{stdout}");
    assert_eq!(writes[0].points[2].value, MetricValue::Double(0.5));
}

#[tokio::test]
async fn without_the_flag_nothing_is_written_and_the_project_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "");
    let fake = Fake::new(vec![passing("a")]);
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&["monitor", &spec, "--metrics-project", "not a project!"])
        .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stderr, "");
    assert_eq!(metered.builds(), 0);
    assert!(metered.writes().is_empty());
}

#[tokio::test]
async fn metrics_project_overrides_the_source_project() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "");
    let fake = Fake::new(vec![passing("a")]);
    let metered = Metered::new(&fake);
    let (code, _, stderr) = metered
        .run(&[
            "monitor",
            &spec,
            "--write-metrics",
            "--metrics-project",
            "ops-proj",
        ])
        .await;
    assert_eq!(code, 0, "{stderr}");
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].project, "ops-proj");
}

#[tokio::test]
async fn a_bad_metrics_project_exits_2_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "");
    let fake = Fake::new(vec![passing("a")]);
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&[
            "monitor",
            &spec,
            "--write-metrics",
            "--metrics-project",
            "bad/proj",
        ])
        .await;
    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "gadk-eval: monitor spec: --metrics-project: invalid project id bad/proj\n"
    );
    assert!(fake.source.queries.lock().unwrap().is_empty());
    assert_eq!(metered.builds(), 0);
}

#[tokio::test]
async fn a_failing_writer_changes_nothing_but_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_file(dir.path(), "min_pass_rate: 0.9\n");
    let runs = || vec![passing("a"), failing("b")];
    let (plain_code, plain_stdout, plain_stderr) =
        cli_run(&["monitor", &spec], &Fake::new(runs())).await;
    assert_eq!(plain_code, 1);
    assert_eq!(plain_stderr, "");

    let fake = Fake::new(runs());
    let metered = Metered::failing(&fake, "4 of 4 series not written: HTTP 403: denied");
    let (code, stdout, stderr) = metered.run(&["monitor", &spec, "--write-metrics"]).await;
    assert_eq!(code, plain_code);
    assert_eq!(timeless(&stdout), timeless(&plain_stdout));
    assert_eq!(
        stderr,
        "metrics: 4 of 4 series not written: HTTP 403: denied\n"
    );

    let fake = Fake::new(runs());
    let metered = Metered::refusing(&fake, "Application Default Credentials: none found");
    let (code, stdout, stderr) = metered.run(&["monitor", &spec, "--write-metrics"]).await;
    assert_eq!(code, plain_code);
    assert_eq!(timeless(&stdout), timeless(&plain_stdout));
    assert_eq!(
        stderr,
        "metrics: Application Default Credentials: none found\n"
    );
    assert_eq!(metered.builds(), 1);
}

#[tokio::test]
async fn the_new_flags_are_in_the_help_of_every_metrics_command() {
    for command in ["monitor", "guard", "guards", "alerts"] {
        let (code, stdout, _) = cli_run(&[command, "--help"], &Fake::new(Vec::new())).await;
        assert_eq!(code, 0, "{command}");
        assert!(stdout.contains("--write-metrics"), "{command}: {stdout}");
        assert!(
            stdout.contains("--metrics-project <id>"),
            "{command}: {stdout}"
        );
    }
}
