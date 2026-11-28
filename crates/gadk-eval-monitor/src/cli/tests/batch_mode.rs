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

//! `--judge-mode batch` on `monitor` and `experiment`.

use crate::experiment::ExperimentReport;
use crate::testutil::{FAKE_JOB, FakePredictor};

use super::*;

/// Backends serving `source`, a judge model that always passes, agent models that reply
/// `concise reply`, and `predictor` as the batch predictor, recording each judge, model and
/// predictor built.
struct Batched {
    source: Arc<FakeSource>,
    predictor: Arc<FakePredictor>,
    calls: Mutex<Vec<String>>,
}

impl Batched {
    fn new(runs: Vec<ProductionRun>, predictor: Arc<FakePredictor>) -> Self {
        Self {
            source: Arc::new(FakeSource::new(runs)),
            predictor,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Backends for Batched {
    fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
        Ok(self.source.clone())
    }

    async fn judge(&self, spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("judge {}", spec.model));
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
        Ok(FakeJudge::new(&["concise reply"; 10]))
    }

    async fn batch_predictor(
        &self,
        spec: &JudgeSpec,
        staging: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn BatchPredictor>, MonitorError> {
        self.calls.lock().unwrap().push(format!(
            "batch_predictor {} {}/{} {staging} {timeout:?}",
            spec.model, spec.project, spec.location
        ));
        Ok(self.predictor.clone())
    }
}

/// `stdout` without its `window …` line, which names the current time.
fn without_window(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|l| !l.starts_with("window "))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The batch flags with staging `gs://bkt/stage/`.
const BATCH: [&str; 4] = [
    "--judge-mode",
    "batch",
    "--batch-staging",
    "gs://bkt/stage/",
];

/// `args` followed by [`BATCH`].
fn batch_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().chain(BATCH).collect()
}

#[test]
fn batch_flags_are_none_when_live_and_normalized_when_batch() {
    let read = |args: &[&str]| {
        let m = cli()
            .try_get_matches_from(["gadk-eval", "monitor", "m.yaml"].iter().chain(args))
            .unwrap();
        let (_, m) = m.subcommand().unwrap();
        BatchFlags::read(m, &JudgeFlags::of(m), false).unwrap()
    };
    assert_eq!(read(&[]), None);
    assert_eq!(
        read(&["--batch-staging", "nope", "--batch-timeout", "x"]),
        None
    );
    assert_eq!(
        read(&BATCH),
        Some(BatchFlags {
            staging: "gs://bkt/stage".into(),
            timeout: Duration::from_secs(6 * 3600),
        })
    );
    assert_eq!(
        read(&batch_args(&["--batch-timeout", "45m"]))
            .unwrap()
            .timeout,
        Duration::from_secs(45 * 60)
    );
}

#[tokio::test]
async fn bad_batch_flags_exit_2_in_order_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let spec = judged_spec(dir.path(), "");
    let monitor = |extra: &[&'static str]| -> Vec<String> {
        ["monitor", spec.as_str()]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    let cases: [(Vec<String>, &str); 6] = [
        (
            monitor(&["--judge-mode", "batch", "--batch-timeout", "0h"]),
            "--judge-mode batch needs --batch-staging gs://<bucket>/<prefix>",
        ),
        (
            monitor(&[
                "--judge-mode",
                "batch",
                "--batch-staging",
                "gs://bkt",
                "--batch-timeout",
                "0h",
            ]),
            "--batch-staging must be gs://<bucket>/<prefix>, got gs://bkt",
        ),
        (
            monitor(&[
                "--judge-mode",
                "batch",
                "--batch-staging",
                "gs://bkt/stage",
                "--batch-timeout",
                "6x",
                "--judge-backend",
                "eval-service",
            ]),
            "--batch-timeout must be <n>m, <n>h or <n>d with n >= 1, got 6x",
        ),
        (
            monitor(&[
                "--judge-mode",
                "batch",
                "--batch-staging",
                "gs://bkt/stage",
                "--judge-backend",
                "eval-service",
            ]),
            "--judge-mode batch needs --judge-backend gemini",
        ),
        (
            monitor(&[
                "--judge-mode",
                "batch",
                "--judge-backend",
                "eval-service",
                "--eval-location",
                "us",
            ]),
            "--eval-location us is a multi-region; use global or a region such as us-central1",
        ),
        (
            monitor(&["--judge-mode", "batch", "--batch-staging", "s3://bkt/stage"]),
            "--batch-staging must be gs://<bucket>/<prefix>, got s3://bkt/stage",
        ),
    ];
    for (args, message) in cases {
        let backends = Batched::new(vec![passing("a")], FakePredictor::new(VERDICT));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, stdout, stderr) = cli_run(&args, &backends).await;
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(stdout, "");
        assert_eq!(stderr, format!("gadk-eval: monitor spec: {message}\n"));
        assert!(backends.calls().is_empty(), "{args:?}");
        assert!(backends.source.queries.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn batch_with_pairwise_exits_2_after_the_backend_check() {
    let backends = Batched::new(Vec::new(), FakePredictor::new(VERDICT));
    let with =
        |extra: &[&'static str]| batch_args(&[&["experiment", "missing.yaml"][..], extra].concat());
    for (args, message) in [
        (
            with(&["--pairwise", "--judge-backend", "eval-service"]),
            "--judge-mode batch needs --judge-backend gemini",
        ),
        (
            with(&["--pairwise"]),
            "--judge-mode batch cannot be used with --pairwise",
        ),
        (
            vec!["experiment", "missing.yaml", "--pairwise"],
            "--pairwise needs --judge-backend eval-service",
        ),
    ] {
        let (code, _, stderr) = cli_run(&args, &backends).await;
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(stderr, format!("gadk-eval: monitor spec: {message}\n"));
    }
    assert!(backends.calls().is_empty());
}

#[tokio::test]
async fn live_mode_ignores_the_batch_flags() {
    let dir = tempfile::tempdir().unwrap();
    let spec = judged_spec(dir.path(), "");
    let plain = Batched::new(vec![passing("a")], FakePredictor::new(VERDICT));
    let (code, stdout, stderr) = cli_run(&["monitor", &spec], &plain).await;
    let flagged = Batched::new(vec![passing("a")], FakePredictor::new(VERDICT));
    let (code2, stdout2, stderr2) = cli_run(
        &[
            "monitor",
            &spec,
            "--judge-mode",
            "live",
            "--batch-staging",
            "nope",
            "--batch-timeout",
            "x",
        ],
        &flagged,
    )
    .await;
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(
        (code2, without_window(&stdout2), stderr2),
        (code, without_window(&stdout), stderr)
    );
    assert_eq!(flagged.calls(), ["judge gemini-3.8-flash"]);
    assert!(flagged.predictor.calls().is_empty());
}

#[tokio::test]
async fn monitor_in_batch_mode_matches_live_and_names_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let spec = judged_spec(dir.path(), "");
    let out = dir.path().join("reports");
    let runs = vec![passing("a"), failing("b")];
    let live = Batched::new(runs.clone(), FakePredictor::new(VERDICT));
    let (code, stdout, stderr) = cli_run(&["monitor", &spec], &live).await;
    let batched = Batched::new(runs, FakePredictor::new(VERDICT));
    let (code2, stdout2, stderr2) = cli_run(
        &batch_args(&[
            "monitor",
            &spec,
            "--batch-timeout",
            "2h",
            "--out",
            &out.display().to_string(),
        ]),
        &batched,
    )
    .await;
    assert_eq!((code, stderr.as_str()), (0, ""), "{stdout}");
    assert_eq!(code2, code);
    let stored = stdout2.lines().last().unwrap().to_owned();
    assert!(stored.starts_with("stored report support-"), "{stdout2}");
    assert_eq!(
        without_window(&stdout2),
        format!("{}{stored}\n", without_window(&stdout))
    );
    assert_eq!(stderr2, format!("batch job: {FAKE_JOB}\n"));
    assert_eq!(
        batched.calls(),
        ["batch_predictor gemini-3.8-flash p/us gs://bkt/stage 7200s"]
    );
    let calls = batched.predictor.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 2);
}

#[tokio::test]
async fn nothing_to_judge_creates_no_job() {
    let dir = tempfile::tempdir().unwrap();
    // No judge section: no predictor is built.
    let plain = spec_file(dir.path(), "");
    let backends = Batched::new(vec![passing("a")], FakePredictor::new(VERDICT));
    let (code, _, stderr) = cli_run(&batch_args(&["monitor", &plain]), &backends).await;
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(backends.calls().is_empty());
    // A judge section but no runs: the predictor is built, no job is created.
    let judged = judged_spec(dir.path(), "");
    let backends = Batched::new(Vec::new(), FakePredictor::new(VERDICT));
    let (code, _, stderr) = cli_run(&batch_args(&["monitor", &judged]), &backends).await;
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(backends.calls().len(), 1);
    assert!(backends.predictor.calls().is_empty());
}

#[tokio::test]
async fn a_failed_batch_exits_2_and_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let spec = judged_spec(dir.path(), "");
    let out = dir.path().join("reports");
    let backends = Batched::new(
        vec![passing("a")],
        FakePredictor::failing("batch: HTTP 403: denied"),
    );
    let (code, stdout, stderr) = cli_run(
        &batch_args(&["monitor", &spec, "--out", &out.display().to_string()]),
        &backends,
    )
    .await;
    assert_eq!((code, stdout.as_str()), (2, ""));
    assert_eq!(stderr, "gadk-eval: judge: batch: HTTP 403: denied\n");
    assert!(!out.exists() || std::fs::read_dir(&out).unwrap().count() == 0);
}

#[tokio::test]
async fn backends_without_batch_prediction_refuse_it() {
    let dir = tempfile::tempdir().unwrap();
    let spec = judged_spec(dir.path(), "");
    let (code, _, stderr) = cli_run(
        &batch_args(&["monitor", &spec]),
        &Fake::new(vec![passing("a")]),
    )
    .await;
    assert_eq!(code, 2);
    assert_eq!(
        stderr,
        "gadk-eval: judge: batch prediction is not supported by these backends\n"
    );
}

#[tokio::test]
async fn cloud_backends_check_the_staging_before_credentials() {
    let spec = JudgeSpec {
        model: "gemini-3.8-flash".into(),
        project: "p".into(),
        location: "us".into(),
        timeout: Duration::from_secs(120),
    };
    let err = CloudBackends
        .batch_predictor(&spec, "gs://bkt", Duration::from_secs(60))
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "monitor spec: --batch-staging must be gs://<bucket>/<prefix>, got gs://bkt"
    );
}

/// The set of the experiment tests: one case with a `contains` and an `llm_judge` scorer (or
/// only `contains` when `judged` is false).
fn experiment_spec(dir: &Path, judged: bool) -> String {
    let expect = if judged {
        "[contains: concise, llm_judge: Polite]"
    } else {
        "[contains: concise]"
    };
    std::fs::create_dir_all(dir.join("sets")).unwrap();
    std::fs::write(
        dir.join("sets/smoke.yaml"),
        format!("name: smoke\ncases:\n  - id: brief\n    turns: [\"Say something.\"]\n    expect: {expect}\n"),
    )
    .unwrap();
    let p = dir.join("exp.yaml");
    std::fs::write(
        &p,
        "name: exp\nset: sets/smoke.yaml\nproject: p\nmodel: m-flash\nruns: 2\n\
         judge: { model: m-judge }\nagent: { name: assistant, instruction: Be concise. }\n\
         variants: [{ name: a }, { name: b }]\n",
    )
    .unwrap();
    p.display().to_string()
}

#[tokio::test]
async fn experiment_in_batch_mode_grades_from_one_job() {
    let dir = tempfile::tempdir().unwrap();
    let spec = experiment_spec(dir.path(), true);
    let out = dir.path().join("reports");
    let backends = Batched::new(Vec::new(), FakePredictor::new(VERDICT));
    let (code, stdout, stderr) = cli_run(
        &batch_args(&["experiment", &spec, "--out", &out.display().to_string()]),
        &backends,
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stderr, format!("batch job: {FAKE_JOB}\n"));
    assert_eq!(stdout.matches("2/2 (100%)").count(), 2, "{stdout}");
    // The judge model is never built; the predictor is built before the agents' models.
    assert_eq!(
        backends.calls(),
        [
            "batch_predictor m-judge p/us gs://bkt/stage 21600s",
            "model p/us/m-flash"
        ]
    );
    // Four runs with the same conversation: one judge request.
    let calls = backends.predictor.calls();
    assert_eq!((calls.len(), calls[0].len()), (1, 1));
    let name = stdout
        .lines()
        .last()
        .unwrap()
        .strip_prefix("stored report ")
        .unwrap();
    let json = std::fs::read_to_string(out.join("experiments").join(name)).unwrap();
    let report = ExperimentReport::from_json(&json).unwrap();
    for run in report.report.variants.iter().flat_map(|v| &v.runs) {
        let judge = &run.scores[1];
        assert_eq!(
            (judge.kind.as_str(), judge.score, judge.rationale.as_str()),
            ("llm_judge", Some(5), "ok")
        );
    }
}

#[tokio::test]
async fn an_experiment_without_judge_scorers_creates_no_job() {
    let dir = tempfile::tempdir().unwrap();
    let spec = experiment_spec(dir.path(), false);
    let backends = Batched::new(Vec::new(), FakePredictor::new(VERDICT));
    let (code, stdout, stderr) = cli_run(&batch_args(&["experiment", &spec]), &backends).await;
    assert_eq!((code, stderr.as_str()), (0, ""), "{stdout}");
    assert!(backends.predictor.calls().is_empty());
}
