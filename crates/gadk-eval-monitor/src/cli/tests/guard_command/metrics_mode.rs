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

//! `--write-metrics` on `guard` and `guards`.

use crate::cli::tests::metrics_mode::Metered;
use crate::metrics::MetricValue;

use super::*;

const GUARD_METRICS: [&str; 3] = [
    "guard/rollout_percent",
    "guard/verdict",
    "guard/worse_checks",
];

#[tokio::test]
async fn guard_writes_once_after_the_report_is_printed_and_stored() {
    let dir = tempfile::tempdir().unwrap();
    prompts(dir.path(), Some(10)).await;
    let spec = guard_file(dir.path(), ERROR_RATE);
    let out = dir.path().join("reports").display().to_string();
    let fake = healthy();
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&["guard", &spec, "--out", &out, "--write-metrics"])
        .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stderr, "");
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].project, "p");
    assert_eq!(writes[0].metrics(), GUARD_METRICS);
    let verdict = &writes[0].points[1];
    assert_eq!(verdict.labels["verdict"], "advance");
    assert_eq!(verdict.labels["spec"], "g");
    assert_eq!(verdict.labels["prompt"], "support");
    assert_eq!(verdict.labels["label"], "production");
    assert_eq!(writes[0].points[0].value, MetricValue::Int(10));
    assert_eq!(writes[0].stdout, stdout);
    assert!(
        stdout
            .lines()
            .last()
            .unwrap()
            .starts_with("stored report g-"),
        "{stdout}"
    );

    // Without --out, the points follow the printed report.
    let fake = healthy();
    let metered = Metered::new(&fake);
    let (code, stdout, _) = metered.run(&["guard", &spec, "--write-metrics"]).await;
    assert_eq!(code, 0);
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].stdout, stdout);
    assert!(stdout.ends_with("action: would ramp to 25%\n"), "{stdout}");
}

#[tokio::test]
async fn a_waiting_guard_writes_and_a_guard_with_no_rollout_does_not() {
    let dir = tempfile::tempdir().unwrap();
    prompts(dir.path(), Some(10)).await;
    let spec = guard_file(dir.path(), ERROR_RATE);
    let fake = Guarded::new((1000, 0), (50, 0));
    let metered = Metered::new(&fake);
    let (code, _, _) = metered.run(&["guard", &spec, "--write-metrics"]).await;
    assert_eq!(code, 3);
    let writes = metered.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].points[1].labels["verdict"], "wait");

    let idle = tempfile::tempdir().unwrap();
    prompts(idle.path(), None).await;
    let spec = guard_file(idle.path(), ERROR_RATE);
    let fake = healthy();
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered.run(&["guard", &spec, "--write-metrics"]).await;
    assert_eq!(code, 4);
    assert_eq!(stdout, "guard g: support/production has no rollout\n");
    assert_eq!(stderr, "");
    assert!(metered.writes().is_empty());
    assert_eq!(metered.builds(), 0);
}

#[tokio::test]
async fn a_failing_writer_leaves_the_guard_output_and_exit_code_alone() {
    let dir = tempfile::tempdir().unwrap();
    prompts(dir.path(), Some(10)).await;
    let spec = guard_file(dir.path(), ERROR_RATE);
    let failing = || Guarded::new((1000, 0), (1000, 200));
    let (plain_code, plain_stdout, _) = cli_run(&["guard", &spec], &failing()).await;
    assert_eq!(plain_code, 1);
    let fake = failing();
    let metered = Metered::failing(&fake, "HTTP 403: denied");
    let (code, stdout, stderr) = metered.run(&["guard", &spec, "--write-metrics"]).await;
    assert_eq!((code, stdout), (plain_code, plain_stdout));
    assert_eq!(stderr, "metrics: HTTP 403: denied\n");
    assert_eq!(metered.writes()[0].points[2].value, MetricValue::Int(1));
}

#[tokio::test]
async fn a_bad_metrics_project_stops_guard_and_guards_before_any_guard_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, files) = three_specs(dir.path()).await;
    for args in [["guard", files[0].as_str()], ["guards", specs.as_str()]] {
        let fake = healthy();
        let metered = Metered::new(&fake);
        let mut argv = args.to_vec();
        argv.extend(["--write-metrics", "--metrics-project", "a b"]);
        let (code, stdout, stderr) = metered.run(&argv).await;
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(stdout, "", "{args:?}");
        assert_eq!(
            stderr,
            "gadk-eval: monitor spec: --metrics-project: invalid project id a b\n"
        );
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }
}

#[tokio::test]
async fn guards_writes_once_per_report_before_the_summary_and_notify() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, _) = three_specs(dir.path()).await;
    let (_, plain, _) = cli_run(&["guards", &specs, "--notify", HOOK], &healthy()).await;
    let fake = healthy();
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&[
            "guards",
            &specs,
            "--notify",
            HOOK,
            "--write-metrics",
            "--metrics-project",
            "ops-proj",
        ])
        .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stderr, "");
    assert_eq!(stdout, plain);
    assert_eq!(fake.posts().len(), 1);
    let writes = metered.writes();
    let specs: Vec<(&str, &str)> = writes
        .iter()
        .map(|w| {
            (
                w.points[0].labels["spec"].as_str(),
                w.points[1].labels["verdict"].as_str(),
            )
        })
        .collect();
    // `idle` has no rollout, so no report and no points.
    assert_eq!(specs, [("advance", "advance"), ("wait", "wait")]);
    for w in &writes {
        assert_eq!(w.project, "ops-proj");
        assert_eq!(w.metrics(), GUARD_METRICS);
        assert!(!w.stdout.contains("guards: "), "{}", w.stdout);
        assert!(!w.stdout.contains("notify: "), "{}", w.stdout);
    }
    assert!(writes[0].stdout.ends_with("action: would ramp to 25%\n"));
    assert!(!writes[0].stdout.contains("== wait =="));
    assert!(writes[1].stdout.ends_with("action: none\n"));
    assert_eq!(metered.builds(), 1);
}

#[tokio::test]
async fn guards_prints_a_writer_build_failure_once() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, _) = three_specs(dir.path()).await;
    let (plain_code, plain_stdout, _) = cli_run(&["guards", &specs], &healthy()).await;
    let fake = healthy();
    let metered = Metered::refusing(&fake, "Application Default Credentials: none found");
    let (code, stdout, stderr) = metered.run(&["guards", &specs, "--write-metrics"]).await;
    assert_eq!((code, stdout), (plain_code, plain_stdout));
    assert_eq!(
        stderr,
        "metrics: Application Default Credentials: none found\n"
    );
    assert_eq!(metered.builds(), 1);
}

#[tokio::test]
async fn guards_prints_one_line_per_failed_write() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, _) = three_specs(dir.path()).await;
    let (plain_code, plain_stdout, _) = cli_run(&["guards", &specs], &healthy()).await;
    let fake = healthy();
    let metered = Metered::failing(&fake, "HTTP 403: denied");
    let (code, stdout, stderr) = metered.run(&["guards", &specs, "--write-metrics"]).await;
    assert_eq!((code, stdout), (plain_code, plain_stdout));
    assert_eq!(
        stderr,
        "metrics: HTTP 403: denied\nmetrics: HTTP 403: denied\n"
    );
    assert_eq!(metered.writes().len(), 2);
}
