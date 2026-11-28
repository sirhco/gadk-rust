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

//! `--write-metrics` on `alerts`.

use crate::cli::tests::metrics_mode::Metered;
use crate::metrics::MetricValue;

use super::*;

/// Each point of a write as `<metric> <rule>`.
fn rule_points(w: &crate::cli::tests::metrics_mode::Written) -> Vec<String> {
    w.points
        .iter()
        .map(|p| format!("{} {}", p.metric, p.labels["rule"]))
        .collect()
}

#[tokio::test]
async fn alerts_write_once_per_spec_before_the_summary_and_notify() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, prices) = two_specs(dir.path());
    let (_, plain, _) = cli_run(
        &["alerts", &specs, "--prices", &prices, "--notify", HOOK],
        &Alerting::new(),
    )
    .await;
    let fake = Alerting::new();
    let metered = Metered::new(&fake);
    let before = Utc::now();
    let (code, stdout, stderr) = metered
        .run(&[
            "alerts",
            &specs,
            "--prices",
            &prices,
            "--notify",
            HOOK,
            "--write-metrics",
        ])
        .await;
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert_eq!(stderr, "");
    assert_eq!(stdout, plain);
    assert_eq!(fake.posts().len(), 1);
    let writes = metered.writes();
    assert_eq!(writes.len(), 2);
    // `new-errors` has too few calls, so it writes nothing.
    assert_eq!(
        rule_points(&writes[0]),
        [
            "alerts/rule_value daily-cost",
            "alerts/rule_fired daily-cost",
            "alerts/rule_value error_rate",
            "alerts/rule_fired error_rate",
            "alerts/rule_value support-calls",
            "alerts/rule_fired support-calls",
        ]
    );
    assert_eq!(writes[0].points[1].value, MetricValue::Int(1));
    assert_eq!(writes[0].points[3].value, MetricValue::Int(0));
    assert_eq!(writes[0].points[4].value, MetricValue::Double(110.0));
    assert_eq!(writes[0].points[0].labels["spec"], "usage");
    assert_eq!(writes[0].stdout, format!("== usage ==\n{USAGE_LINES}"));
    assert_eq!(
        rule_points(&writes[1]),
        ["alerts/rule_value tokens", "alerts/rule_fired tokens"]
    );
    assert!(
        writes[1]
            .stdout
            .ends_with("tokens: tokens 196000 ≤ 1000000 — ok\n")
    );
    for w in &writes {
        assert_eq!(w.project, "p");
        assert!(!w.stdout.contains("alerts: "), "{}", w.stdout);
        assert!(!w.stdout.contains("notify: "), "{}", w.stdout);
        // The time is the end of the spec's window: the moment its query ran.
        assert!(
            w.points
                .iter()
                .all(|p| p.time >= before && p.time <= Utc::now())
        );
        assert!(w.points.iter().all(|p| p.time == w.points[0].time));
    }
}

#[tokio::test]
async fn only_insufficient_rules_or_a_failed_spec_make_no_call() {
    let dir = tempfile::tempdir().unwrap();
    let sparse = write(
        dir.path(),
        "a.yaml",
        &alert_yaml(
            "sparse",
            "events",
            "1d",
            "[{ where: { model: m-new }, error_rate: { max: 0.5 } }]",
        ),
    );
    let broken = write(
        dir.path(),
        "b.yaml",
        &alert_yaml("broken", "broken", "1h", "[calls: { max: 1 }]"),
    );
    let fake = Alerting::new();
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&["alerts", &sparse, &broken, "--write-metrics"])
        .await;
    assert_eq!(code, 2, "{stdout}{stderr}");
    assert!(stdout.contains("insufficient"), "{stdout}");
    assert!(stdout.contains("error: "), "{stdout}");
    assert_eq!(stderr, "");
    assert_eq!(metered.builds(), 0);
    assert!(metered.writes().is_empty());
}

#[tokio::test]
async fn a_failing_writer_leaves_the_alerts_output_and_exit_code_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, prices) = two_specs(dir.path());
    let (plain_code, plain_stdout, _) =
        cli_run(&["alerts", &specs, "--prices", &prices], &Alerting::new()).await;
    let fake = Alerting::new();
    let metered = Metered::failing(&fake, "HTTP 403: denied");
    let (code, stdout, stderr) = metered
        .run(&["alerts", &specs, "--prices", &prices, "--write-metrics"])
        .await;
    assert_eq!((code, stdout), (plain_code, plain_stdout.clone()));
    assert_eq!(
        stderr,
        "metrics: HTTP 403: denied\nmetrics: HTTP 403: denied\n"
    );

    let fake = Alerting::new();
    let metered = Metered::refusing(&fake, "Application Default Credentials: none found");
    let (code, stdout, stderr) = metered
        .run(&["alerts", &specs, "--prices", &prices, "--write-metrics"])
        .await;
    assert_eq!((code, stdout), (plain_code, plain_stdout));
    assert_eq!(
        stderr,
        "metrics: Application Default Credentials: none found\n"
    );
    assert_eq!(metered.builds(), 1);
}

#[tokio::test]
async fn a_bad_metrics_project_stops_alerts_before_any_query() {
    let dir = tempfile::tempdir().unwrap();
    let (specs, prices) = two_specs(dir.path());
    let fake = Alerting::new();
    let metered = Metered::new(&fake);
    let (code, stdout, stderr) = metered
        .run(&[
            "alerts",
            &specs,
            "--prices",
            &prices,
            "--write-metrics",
            "--metrics-project",
            "x.y",
        ])
        .await;
    assert_eq!(code, 2);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "gadk-eval: monitor spec: --metrics-project: invalid project id x.y\n"
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}
