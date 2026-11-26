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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gadk_prompts::{FileStore, NewVersion, PromptSnapshot, PromptVersion, RolloutSnapshot};

use crate::guard_report::{SideValue, Unit};
use crate::guard_source::{ModelTokens, VersionCalls, VersionFeedback};
use crate::guard_spec::FeedbackSource;
use crate::prices::TokenUsage;
use crate::source::ProductionRun;
use crate::testutil::{FakeJudge, at, run};

use super::*;

const NOW: i64 = 100_000;

/// A guard source returning fixed data and logging each call.
#[derive(Default)]
struct FakeGuard {
    calls: Vec<VersionCalls>,
    /// Rows by score name; `version_feedback` keeps those of the asked source (`any`: all).
    feedback: Vec<(String, Vec<VersionFeedback>)>,
    runs: Vec<(u32, Vec<ProductionRun>)>,
    log: Mutex<Vec<String>>,
    queries: Mutex<Vec<GuardQuery>>,
}

impl FakeGuard {
    fn new(calls: Vec<VersionCalls>) -> Self {
        Self {
            calls,
            ..Self::default()
        }
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GuardSource for FakeGuard {
    async fn version_calls(&self, q: &GuardQuery) -> Result<Vec<VersionCalls>, MonitorError> {
        self.log.lock().unwrap().push("calls".into());
        self.queries.lock().unwrap().push(q.clone());
        Ok(self.calls.clone())
    }

    async fn version_feedback(
        &self,
        q: &GuardQuery,
        name: &str,
        source: FeedbackSource,
    ) -> Result<Vec<VersionFeedback>, MonitorError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("feedback {name} {}", source.as_str()));
        self.queries.lock().unwrap().push(q.clone());
        let rows = self
            .feedback
            .iter()
            .filter(|(n, _)| n == name)
            .flat_map(|(_, rows)| rows.iter())
            .filter(|r| source == FeedbackSource::Any || r.source == source.as_str())
            .cloned()
            .collect();
        Ok(rows)
    }

    async fn version_runs(
        &self,
        q: &GuardQuery,
        version: u32,
        limit: usize,
    ) -> Result<Vec<ProductionRun>, MonitorError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("runs v{version} {limit}"));
        self.queries.lock().unwrap().push(q.clone());
        Ok(self
            .runs
            .iter()
            .filter(|(v, _)| *v == version)
            .flat_map(|(_, runs)| runs.iter().cloned())
            .collect())
    }
}

/// Calls of v1 and v2 as `(calls, errors)`, p95 1000 ms, no models.
fn calls(b: (u64, u64), r: (u64, u64)) -> Vec<VersionCalls> {
    vec![
        VersionCalls::new(1, b.0, b.1, Some(1000.0), Vec::new()),
        VersionCalls::new(2, r.0, r.1, Some(1000.0), Vec::new()),
    ]
}

/// 1000 calls per side, none failed.
fn plenty() -> Vec<VersionCalls> {
    calls((1000, 0), (1000, 0))
}

/// A guard spec on `support`/`production` (window 1h) with `extra` top-level lines and the
/// YAML list `checks`.
fn spec(extra: &str, checks: &str) -> GuardSpec {
    GuardSpec::from_yaml(&format!(
        "name: g\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
         store: file:///unused\nprompt: support\nlabel: production\nwindow: 1h\n{extra}\
         checks:\n{checks}"
    ))
    .unwrap()
}

fn prod() -> Label {
    Label::production()
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

/// A store in `dir` with prompt `support` at v1 and v2, `production` at v1 and, when
/// `percent` is given, a rollout of v2 at that percent.
async fn store_in(dir: &Path, percent: Option<u8>) -> FileStore {
    let s = FileStore::new(dir);
    s.create_version(new_version("one")).await.unwrap();
    s.create_version(new_version("two")).await.unwrap();
    s.set_label("support", &prod(), 1, None).await.unwrap();
    if let Some(p) = percent {
        let r = Rollout::new(2, p).unwrap();
        s.set_rollout("support", &prod(), Some(r)).await.unwrap();
    }
    s
}

/// The label's version and rollout in `store`.
async fn state(store: &dyn PromptStore) -> (Option<u32>, Option<Rollout>) {
    let (prompts, rollouts) = store.load_all_with_rollouts().await.unwrap();
    let label = prompts.prompts["support"].labels.get(&prod()).copied();
    (label, rollouts.get("support", &prod()).copied())
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

/// Runs `spec` at `NOW` without prices, judge or apply.
async fn dry(spec: &GuardSpec, store: &dyn PromptStore, source: &FakeGuard) -> GuardReport {
    Guard::new()
        .now(at(NOW))
        .run(spec, store, source, None, None, None, false)
        .await
        .unwrap()
        .expect("a rollout")
}

/// The only check of `report`.
fn only(report: &GuardReport) -> &CheckResult {
    assert_eq!(report.checks.len(), 1, "{report:?}");
    &report.checks[0]
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

const ERROR_RATE: &str = "  - error_rate: { max_increase: 0.02 }\n";

/// The verdict and action of the error-rate guard with `ramp` on a rollout at `percent`
/// over `calls`, without apply (also checks the exit code and that nothing was applied).
async fn decide(ramp: &str, percent: u8, calls: Vec<VersionCalls>) -> (Verdict, Action) {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(percent)).await;
    let report = dry(&spec(ramp, ERROR_RATE), &store, &FakeGuard::new(calls)).await;
    assert!(!report.applied);
    assert_eq!(report.percent, percent);
    let code = match report.verdict {
        Verdict::Advance => 0,
        Verdict::Abort => 1,
        Verdict::Wait => 3,
    };
    assert_eq!(report.exit_code(), code);
    (report.verdict, report.action)
}

#[tokio::test]
async fn decision_table() {
    let steps = "ramp: [10, 25, 50]\n";
    let worse = || calls((1000, 0), (1000, 100));
    let unsure = || calls((100, 0), (100, 2));
    let few = || calls((1000, 0), (50, 0));
    use Action::{Abort, None as Nothing, Promote, Ramp};
    use Verdict::{Abort as Stop, Advance, Wait};
    assert_eq!(decide(steps, 10, worse()).await, (Stop, Abort));
    assert_eq!(decide(steps, 10, plenty()).await, (Advance, Ramp(25)));
    assert_eq!(decide(steps, 12, plenty()).await, (Advance, Ramp(25)));
    assert_eq!(decide(steps, 5, plenty()).await, (Advance, Ramp(10)));
    assert_eq!(decide("", 10, plenty()).await, (Advance, Promote));
    assert_eq!(decide(steps, 50, plenty()).await, (Advance, Promote));
    assert_eq!(decide(steps, 60, plenty()).await, (Advance, Promote));
    assert_eq!(decide(steps, 10, unsure()).await, (Wait, Nothing));
    assert_eq!(decide(steps, 10, few()).await, (Wait, Nothing));
}

#[tokio::test]
async fn a_report_names_the_rollout_window_and_queries_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let source = FakeGuard::new(plenty());
    let report = dry(&spec("", ERROR_RATE), &store, &source).await;
    assert_eq!(
        (
            report.spec.as_str(),
            report.prompt.as_str(),
            report.label.as_str()
        ),
        ("g", "support", "production")
    );
    assert_eq!(
        (report.baseline, report.rollout, report.percent),
        (1, 2, 10)
    );
    assert_eq!(report.source, "bigquery:p.d.t");
    assert_eq!(report.generated_at, at(NOW));
    assert_eq!(
        report.window,
        Window {
            from: at(NOW - 3600),
            to: at(NOW)
        }
    );
    assert_eq!(source.log(), ["calls"]);
    assert_eq!(
        source.queries.lock().unwrap()[0],
        GuardQuery::new("support", "production", 1, 2, at(NOW - 3600), at(NOW))
    );
}

#[tokio::test]
async fn error_rate_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let check = |c: Vec<VersionCalls>| {
        let store = &store;
        async move { only(&dry(&spec("", ERROR_RATE), store, &FakeGuard::new(c)).await).clone() }
    };
    let ok = check(calls((1000, 10), (1000, 10))).await;
    assert_eq!(ok.name, "error_rate");
    assert_eq!(ok.unit, Unit::Rate);
    assert_eq!(
        ok.baseline,
        SideValue {
            value: Some(0.01),
            n: 1000
        }
    );
    assert_eq!(ok.status, Status::Ok);
    assert_eq!(ok.harm, Some(0.0));
    let (lower, upper) = ok.interval.unwrap();
    assert!(
        lower < 0.0 && upper > 0.0 && upper <= 0.02,
        "{lower} {upper}"
    );
    let worse = check(calls((1000, 0), (1000, 100))).await;
    assert_eq!(worse.status, Status::Worse);
    assert!(approx(worse.harm.unwrap(), 0.1));
    assert_eq!(
        check(calls((100, 0), (100, 2))).await.status,
        Status::Unsure
    );
    let few = check(calls((1000, 0), (99, 0))).await;
    assert_eq!(few.status, Status::Insufficient);
    assert_eq!(
        few.reason.as_deref(),
        Some("fewer than 100 calls: v1 1000, v2 99")
    );
    let none = check(vec![VersionCalls::new(1, 1000, 0, None, Vec::new())]).await;
    assert_eq!(none.status, Status::Insufficient);
    assert_eq!(none.rollout, SideValue { value: None, n: 0 });
    assert_eq!(
        none.reason.as_deref(),
        Some("fewer than 100 calls: v1 1000, v2 0")
    );
}

#[tokio::test]
async fn min_calls_makes_every_check_insufficient() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let checks = "  - error_rate: { max_increase: 0.02 }\n\
                  \x20 - feedback: { name: thumbs_up, max_decrease: 0.05 }\n\
                  \x20 - cost_per_call: { max_increase: 0.2 }\n\
                  \x20 - latency_p95: { max_increase: 0.2 }\n\
                  \x20 - eval: { scorers: [contains: order], max_decrease: 0.1 }\n";
    let source = FakeGuard::new(calls((300, 0), (299, 0)));
    let report = dry(&spec("min_calls: 300\n", checks), &store, &source).await;
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "error_rate",
            "feedback thumbs_up (any)",
            "cost_per_call",
            "latency_p95",
            "eval"
        ]
    );
    for c in &report.checks {
        assert_eq!(c.status, Status::Insufficient, "{}", c.name);
        assert_eq!(
            c.reason.as_deref(),
            Some("fewer than 300 calls: v1 300, v2 299"),
            "{}",
            c.name
        );
    }
    assert_eq!(report.verdict, Verdict::Wait);
    // Feedback is not queried and eval runs are not listed or scored while a side has too
    // few calls.
    assert_eq!(source.log(), ["calls"]);
}

/// A bool row of `version` from `source`.
fn bools(version: u32, source: &str, count: u64, positive: u64) -> VersionFeedback {
    VersionFeedback::new(version, source, "bool", count, positive, None, None)
}

/// A number row of `version` from `source`.
fn numbers(version: u32, source: &str, count: u64, mean: f64, sd: f64) -> VersionFeedback {
    VersionFeedback::new(version, source, "number", count, 0, Some(mean), Some(sd))
}

/// The single feedback check `thumbs_up` (source `source`, max_decrease 0.05 for bool rows
/// and 0.1 for number rows, min_count `min_count`) over `rows`.
async fn feedback_check(source: &str, min_count: u64, rows: Vec<VersionFeedback>) -> CheckResult {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let number = rows.iter().any(|r| r.data_type == "number");
    let checks = format!(
        "  - feedback: {{ name: thumbs_up, source: {source}, max_decrease: {}, min_count: {min_count} }}\n",
        if number { 0.1 } else { 0.05 }
    );
    let mut fake = FakeGuard::new(plenty());
    fake.feedback = vec![
        ("thumbs_up".into(), rows),
        ("other".into(), vec![bools(1, "app", 1000, 0)]),
    ];
    let report = dry(&spec("", &checks), &store, &fake).await;
    assert_eq!(
        fake.log(),
        ["calls".to_owned(), format!("feedback thumbs_up {source}")]
    );
    only(&report).clone()
}

#[tokio::test]
async fn bool_feedback_statuses_and_any_pooling() {
    let ok = feedback_check(
        "any",
        30,
        vec![
            bools(1, "app", 600, 540),
            bools(1, "console", 400, 360),
            bools(2, "app", 1000, 900),
        ],
    )
    .await;
    assert_eq!(ok.name, "feedback thumbs_up (any)");
    assert_eq!(ok.unit, Unit::Rate);
    assert_eq!(
        ok.baseline,
        SideValue {
            value: Some(0.9),
            n: 1000
        }
    );
    assert_eq!(
        ok.rollout,
        SideValue {
            value: Some(0.9),
            n: 1000
        }
    );
    assert_eq!(ok.status, Status::Ok);
    let worse = feedback_check(
        "any",
        30,
        vec![bools(1, "app", 1000, 900), bools(2, "app", 1000, 700)],
    )
    .await;
    assert_eq!(worse.status, Status::Worse);
    assert!(approx(worse.harm.unwrap(), 0.2));
    // `app` keeps only the app rows.
    let app = feedback_check(
        "app",
        30,
        vec![
            bools(1, "app", 100, 90),
            bools(1, "console", 900, 0),
            bools(2, "app", 100, 90),
        ],
    )
    .await;
    assert_eq!(app.name, "feedback thumbs_up (app)");
    assert_eq!(
        app.baseline,
        SideValue {
            value: Some(0.9),
            n: 100
        }
    );
    let few = feedback_check(
        "any",
        30,
        vec![bools(1, "app", 29, 20), bools(2, "app", 40, 30)],
    )
    .await;
    assert_eq!(few.status, Status::Insufficient);
    assert_eq!(
        few.reason.as_deref(),
        Some("fewer than 30 scores: v1 29, v2 40")
    );
    let none = feedback_check("console", 30, vec![bools(1, "app", 100, 90)]).await;
    assert_eq!(none.status, Status::Insufficient);
    assert_eq!(
        none.reason.as_deref(),
        Some("fewer than 30 scores: v1 0, v2 0")
    );
}

#[tokio::test]
async fn number_feedback_statuses_and_any_pooling() {
    // v1 pools to mean 2 with sample sd √(4/3) (4 scores); v2 has the same.
    let pooled = feedback_check(
        "any",
        2,
        vec![
            numbers(1, "app", 2, 1.0, 0.0),
            numbers(1, "console", 2, 3.0, 0.0),
            numbers(2, "app", 4, 2.0, (4.0f64 / 3.0).sqrt()),
        ],
    )
    .await;
    assert_eq!(pooled.unit, Unit::Number);
    assert_eq!(
        pooled.baseline,
        SideValue {
            value: Some(2.0),
            n: 4
        }
    );
    assert_eq!(pooled.harm, Some(0.0));
    let (lower, upper) = pooled.interval.unwrap();
    // Welch: z·√(2 × (4/3) / 4) / |mean(B)| = 1.6449 × 0.8165 / 2.
    assert!(
        approx(upper, 0.6715) && approx(lower, -0.6715),
        "{lower} {upper}"
    );
    assert_eq!(pooled.status, Status::Unsure);
    let worse = feedback_check(
        "any",
        30,
        vec![
            numbers(1, "app", 100, 4.0, 0.0),
            numbers(2, "app", 100, 3.0, 0.0),
        ],
    )
    .await;
    assert_eq!(worse.status, Status::Worse);
    assert_eq!(
        (worse.harm, worse.interval),
        (Some(0.25), Some((0.25, 0.25)))
    );
    let ok = feedback_check(
        "any",
        30,
        vec![
            numbers(1, "app", 100, -4.0, 1.0),
            numbers(2, "app", 100, -3.9, 1.0),
        ],
    )
    .await;
    assert_eq!(ok.status, Status::Ok);
    assert!(approx(ok.harm.unwrap(), -0.025), "{:?}", ok.harm);
    let zero = feedback_check(
        "any",
        30,
        vec![
            numbers(1, "app", 100, 0.0, 1.0),
            numbers(2, "app", 100, 1.0, 1.0),
        ],
    )
    .await;
    assert_eq!(zero.status, Status::Unknown);
    assert_eq!(zero.reason.as_deref(), Some("the baseline mean is 0"));
}

#[tokio::test]
async fn category_and_mixed_feedback_are_unknown() {
    let category = feedback_check(
        "any",
        30,
        vec![
            VersionFeedback::new(1, "app", "category", 100, 0, None, None),
            VersionFeedback::new(2, "app", "category", 100, 0, None, None),
        ],
    )
    .await;
    assert_eq!(category.status, Status::Unknown);
    assert_eq!(
        category.reason.as_deref(),
        Some("category scores are not supported")
    );
    let mixed = feedback_check(
        "any",
        30,
        vec![bools(1, "app", 100, 90), numbers(2, "app", 100, 4.0, 1.0)],
    )
    .await;
    assert_eq!(mixed.status, Status::Unknown);
    assert_eq!(
        mixed.reason.as_deref(),
        Some("data types differ: v1 bool, v2 number")
    );
    let sources = feedback_check(
        "any",
        30,
        vec![
            bools(1, "app", 100, 90),
            numbers(1, "console", 100, 4.0, 1.0),
            bools(2, "app", 100, 90),
        ],
    )
    .await;
    assert_eq!(sources.status, Status::Unknown);
    assert_eq!(
        sources.reason.as_deref(),
        Some("data types differ: v1 bool+number, v2 bool")
    );
}

const PRICES: &str = "- model: gemini-a\n  input: 1\n  output: 0\n";

/// The cost check (max_increase 0.2) with v1 and v2 calling `(model, input tokens)`.
async fn cost_check(b: (&str, u64), r: (&str, u64), prices: Option<&PriceTable>) -> CheckResult {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let tokens = |(model, input): (&str, u64)| {
        vec![ModelTokens::new(
            model,
            1000,
            TokenUsage::new(input, 0, 0, 0),
        )]
    };
    let source = FakeGuard::new(vec![
        VersionCalls::new(1, 1000, 0, None, tokens(b)),
        VersionCalls::new(2, 1000, 0, None, tokens(r)),
    ]);
    let report = Guard::new()
        .now(at(NOW))
        .run(
            &spec("", "  - cost_per_call: { max_increase: 0.2 }\n"),
            &store,
            &source,
            prices,
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    only(&report).clone()
}

#[tokio::test]
async fn cost_per_call_statuses() {
    let prices = PriceTable::from_yaml(PRICES).unwrap();
    let ok = cost_check(
        ("gemini-a", 1_000_000),
        ("gemini-a", 1_100_000),
        Some(&prices),
    )
    .await;
    assert_eq!(ok.name, "cost_per_call");
    assert_eq!(ok.unit, Unit::Usd);
    assert_eq!(
        ok.baseline,
        SideValue {
            value: Some(0.001),
            n: 1000
        }
    );
    assert_eq!(ok.status, Status::Ok);
    assert!(approx(ok.harm.unwrap(), 0.1));
    assert_eq!(ok.interval, None);
    let worse = cost_check(
        ("gemini-a", 1_000_000),
        ("gemini-a", 2_000_000),
        Some(&prices),
    )
    .await;
    assert_eq!(worse.status, Status::Worse);
    assert!(approx(worse.harm.unwrap(), 1.0));
    let no_prices = cost_check(("gemini-a", 1), ("gemini-a", 1), None).await;
    assert_eq!(no_prices.status, Status::Unknown);
    assert_eq!(no_prices.reason.as_deref(), Some("no price file given"));
    assert_eq!(
        no_prices.baseline,
        SideValue {
            value: None,
            n: 1000
        }
    );
    let missing = cost_check(("gemini-a", 1), ("gemini-b", 1), Some(&prices)).await;
    assert_eq!(missing.status, Status::Unknown);
    assert_eq!(
        missing.reason.as_deref(),
        Some("no price for model gemini-b")
    );
    let zero = cost_check(("gemini-a", 0), ("gemini-a", 1), Some(&prices)).await;
    assert_eq!(zero.status, Status::Unknown);
    assert_eq!(zero.reason.as_deref(), Some("the baseline cost is 0"));
}

#[tokio::test]
async fn latency_p95_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let check = |b: Option<f64>, r: Option<f64>| {
        let store = &store;
        async move {
            let source = FakeGuard::new(vec![
                VersionCalls::new(1, 1000, 0, b, Vec::new()),
                VersionCalls::new(2, 1000, 0, r, Vec::new()),
            ]);
            let checks = "  - latency_p95: { max_increase: 0.25 }\n";
            only(&dry(&spec("", checks), store, &source).await).clone()
        }
    };
    let ok = check(Some(1000.0), Some(1100.0)).await;
    assert_eq!((ok.name.as_str(), ok.unit), ("latency_p95", Unit::Millis));
    assert_eq!(
        ok.rollout,
        SideValue {
            value: Some(1100.0),
            n: 1000
        }
    );
    assert_eq!(ok.status, Status::Ok);
    assert!(approx(ok.harm.unwrap(), 0.1));
    assert_eq!(
        check(Some(1000.0), Some(1300.0)).await.status,
        Status::Worse
    );
    let no_b = check(None, Some(1000.0)).await;
    assert_eq!(no_b.status, Status::Unknown);
    assert_eq!(no_b.reason.as_deref(), Some("no latency for v1"));
    let no_r = check(Some(1000.0), None).await;
    assert_eq!(no_r.reason.as_deref(), Some("no latency for v2"));
    let zero = check(Some(0.0), Some(1000.0)).await;
    assert_eq!(
        zero.reason.as_deref(),
        Some("the baseline p95 latency is 0")
    );
}

/// `pass` runs replying `order`, `fail` runs replying `nope` and `errored` runs whose
/// transcript has an error, all of version `version`.
fn runs(version: u32, pass: usize, fail: usize, errored: usize) -> (u32, Vec<ProductionRun>) {
    let mut out = Vec::new();
    for i in 0..pass + fail + errored {
        let reply = if i < pass { "order" } else { "nope" };
        let mut r = run(&format!("v{version}-{i}"), 50_000, &[("q", reply, &[])]);
        if i >= pass + fail {
            r.transcript.error = Some("model failure".into());
        }
        out.push(r);
    }
    (version, out)
}

const EVAL: &str = "  - eval: { scorers: [contains: order], max_decrease: 0.1, limit: 50 }\n";

async fn eval_check(b: (u32, Vec<ProductionRun>), r: (u32, Vec<ProductionRun>)) -> CheckResult {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let mut source = FakeGuard::new(plenty());
    source.runs = vec![b, r];
    let report = Guard::new()
        .now(at(NOW))
        .concurrency(0)
        .run(&spec("", EVAL), &store, &source, None, None, None, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source.log(), ["calls", "runs v1 50", "runs v2 50"]);
    only(&report).clone()
}

#[tokio::test]
async fn eval_excludes_and_counts_errored_runs() {
    let ok = eval_check(runs(1, 40, 0, 2), runs(2, 40, 0, 0)).await;
    assert_eq!((ok.name.as_str(), ok.unit), ("eval", Unit::Rate));
    assert_eq!(
        ok.baseline,
        SideValue {
            value: Some(1.0),
            n: 40
        }
    );
    assert_eq!(
        ok.rollout,
        SideValue {
            value: Some(1.0),
            n: 40
        }
    );
    assert_eq!(ok.status, Status::Ok);
    assert_eq!(
        ok.reason.as_deref(),
        Some("excluded runs with scorer errors: v1 2, v2 0")
    );
    let worse = eval_check(runs(1, 40, 0, 0), runs(2, 10, 30, 0)).await;
    assert_eq!(worse.status, Status::Worse);
    assert_eq!(
        worse.rollout,
        SideValue {
            value: Some(0.25),
            n: 40
        }
    );
    assert!(approx(worse.harm.unwrap(), 0.75));
    assert_eq!(worse.reason, None);
    let few = eval_check(runs(1, 40, 0, 0), runs(2, 5, 4, 3)).await;
    assert_eq!(few.status, Status::Insufficient);
    assert_eq!(
        few.reason.as_deref(),
        Some(
            "fewer than 10 scored runs: v1 40, v2 9; \
             excluded runs with scorer errors: v1 0, v2 3"
        )
    );
}

#[tokio::test]
async fn eval_uses_the_judge_and_requires_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let checks = "  - eval: { scorers: [llm_judge: Polite], max_decrease: 0.1, limit: 10 }\n";
    let spec = spec("judge: { model: m, timeout: 5s }\n", checks);
    let mut source = FakeGuard::new(plenty());
    source.runs = vec![runs(1, 10, 0, 0), runs(2, 10, 0, 0)];
    let err = Guard::new()
        .run(&spec, &store, &source, None, None, None, false)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "judge: the spec has llm_judge scorers but no judge model was given"
    );
    assert!(source.log().is_empty());
    let verdict = r#"{"pass": true, "score": 5, "rationale": "polite"}"#;
    let judge = FakeJudge::new(&[verdict; 20]);
    let report = Guard::new()
        .now(at(NOW))
        .run(
            &spec,
            &store,
            &source,
            None,
            Some(judge.clone()),
            None,
            false,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(judge.calls(), 20);
    assert_eq!(
        only(&report).baseline,
        SideValue {
            value: Some(1.0),
            n: 10
        }
    );
}

const ADVANCE: &str = "ramp: [10, 50]\n";

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let before = files(dir.path());
    for c in [plenty(), calls((1000, 0), (1000, 100))] {
        let report = dry(&spec(ADVANCE, ERROR_RATE), &store, &FakeGuard::new(c)).await;
        assert!(!report.applied);
        assert_ne!(report.action, Action::None);
    }
    assert_eq!(files(dir.path()), before);
}

/// Runs `spec` with apply.
async fn apply(
    spec: &GuardSpec,
    store: &dyn PromptStore,
    c: Vec<VersionCalls>,
) -> Result<GuardReport, MonitorError> {
    Guard::new()
        .now(at(NOW))
        .run(spec, store, &FakeGuard::new(c), None, None, None, true)
        .await
        .map(|r| r.expect("a rollout"))
}

#[tokio::test]
async fn apply_ramps_promotes_and_aborts() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir.path().join("ramp"), Some(10)).await;
    let report = apply(&spec(ADVANCE, ERROR_RATE), &store, plenty())
        .await
        .unwrap();
    assert_eq!((report.action, report.applied), (Action::Ramp(50), true));
    assert_eq!(
        state(&store).await,
        (Some(1), Some(Rollout::new(2, 50).unwrap()))
    );

    let report = apply(&spec(ADVANCE, ERROR_RATE), &store, plenty())
        .await
        .unwrap();
    assert_eq!((report.action, report.applied), (Action::Promote, true));
    assert_eq!(state(&store).await, (Some(2), None));

    let store = store_in(&dir.path().join("abort"), Some(10)).await;
    let worse = calls((1000, 0), (1000, 100));
    let report = apply(&spec(ADVANCE, ERROR_RATE), &store, worse)
        .await
        .unwrap();
    assert_eq!((report.action, report.applied), (Action::Abort, true));
    assert_eq!(state(&store).await, (Some(1), None));

    let store = store_in(&dir.path().join("wait"), Some(10)).await;
    let before = files(&dir.path().join("wait"));
    let few = calls((1000, 0), (10, 0));
    let report = apply(&spec(ADVANCE, ERROR_RATE), &store, few)
        .await
        .unwrap();
    assert_eq!((report.action, report.applied), (Action::None, false));
    assert_eq!(files(&dir.path().join("wait")), before);
}

/// A file store that replaces the rollout with `change` right after the first
/// `load_all_with_rollouts`, which the default `load_prompt` makes (the guard's first
/// `load_prompt`), as a concurrent writer would. With `at_promote` it writes `change` inside
/// `promote_rollout_if` instead, just before the file store's atomic write, so the change lands
/// after every read the guard makes.
struct Racing {
    inner: FileStore,
    change: Option<Rollout>,
    loads: AtomicUsize,
    at_promote: bool,
}

#[async_trait::async_trait]
impl PromptStore for Racing {
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
        self.inner.load_all().await
    }

    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
        self.inner.create_version(new).await
    }

    async fn set_label(
        &self,
        name: &str,
        label: &Label,
        version: u32,
        expected: Option<Option<u32>>,
    ) -> Result<(), StoreError> {
        self.inner.set_label(name, label, version, expected).await
    }

    async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
        self.inner.remove_label(name, label).await
    }

    async fn load_all_with_rollouts(
        &self,
    ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
        let out = self.inner.load_all_with_rollouts().await;
        if !self.at_promote && self.loads.fetch_add(1, Ordering::SeqCst) == 0 {
            self.inner
                .set_rollout("support", &prod(), self.change)
                .await
                .unwrap();
        }
        out
    }

    async fn set_rollout(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        self.inner.set_rollout(name, label, rollout).await
    }

    async fn set_rollout_if(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
        expected_version: u32,
        expected_rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        self.inner
            .set_rollout_if(name, label, rollout, expected_version, expected_rollout)
            .await
    }

    async fn promote_rollout_if(
        &self,
        name: &str,
        label: &Label,
        expected_version: u32,
        expected_rollout: Rollout,
    ) -> Result<(), StoreError> {
        if self.at_promote {
            self.inner.set_rollout(name, label, self.change).await?;
        }
        self.inner
            .promote_rollout_if(name, label, expected_version, expected_rollout)
            .await
    }
}

#[tokio::test]
async fn a_rollout_changed_after_the_read_writes_nothing() {
    let cases = [
        // (ramp, calls, rollout written meanwhile, error)
        (
            ADVANCE,
            plenty(),
            Some(Rollout::new(2, 20).unwrap()),
            "report store: prompt store changed since it was read (v1, v2 at 10%): \
             conflict: expected v2, found v2",
        ),
        (
            ADVANCE,
            calls((1000, 0), (1000, 100)),
            None,
            "report store: prompt store changed since it was read (v1, v2 at 10%): \
             conflict: expected v2, found unset",
        ),
        (
            "",
            plenty(),
            Some(Rollout::new(2, 20).unwrap()),
            "report store: prompt store changed since it was read (v1, v2 at 10%): \
             conflict: expected v2, found v2",
        ),
        (
            "",
            plenty(),
            None,
            "report store: prompt store changed since it was read (v1, v2 at 10%): \
             conflict: expected v2, found unset",
        ),
    ];
    for (i, (ramp, c, change, want)) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let store = Racing {
            inner: store_in(dir.path(), Some(10)).await,
            change,
            loads: AtomicUsize::new(0),
            at_promote: false,
        };
        let err = apply(&spec(ramp, ERROR_RATE), &store, c).await.unwrap_err();
        assert_eq!(err.to_string(), want, "case {i}");
        assert_eq!(state(&store.inner).await, (Some(1), change), "case {i}");
    }
}

#[tokio::test]
async fn a_rollout_changed_inside_the_promote_writes_nothing() {
    for (change, found) in [(Some(Rollout::new(2, 20).unwrap()), "v2"), (None, "unset")] {
        let dir = tempfile::tempdir().unwrap();
        let store = Racing {
            inner: store_in(dir.path(), Some(10)).await,
            change,
            loads: AtomicUsize::new(0),
            at_promote: true,
        };
        let err = apply(&spec("", ERROR_RATE), &store, plenty())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "report store: prompt store changed since it was read (v1, v2 at 10%): \
                 conflict: expected v2, found {found}"
            ),
            "{change:?}"
        );
        assert_eq!(state(&store.inner).await, (Some(1), change), "{change:?}");
    }
}

#[tokio::test]
async fn promote_after_rewriting_the_same_rollout_goes_through() {
    let dir = tempfile::tempdir().unwrap();
    let store = Racing {
        inner: store_in(dir.path(), Some(10)).await,
        change: Some(Rollout::new(2, 10).unwrap()),
        loads: AtomicUsize::new(0),
        at_promote: false,
    };
    let report = apply(&spec("", ERROR_RATE), &store, plenty())
        .await
        .unwrap();
    assert_eq!((report.action, report.applied), (Action::Promote, true));
    assert_eq!(state(&store.inner).await, (Some(2), None));
}

#[tokio::test]
async fn a_label_without_a_rollout_is_nothing_to_guard() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), None).await;
    let before = files(dir.path());
    let source = FakeGuard::new(plenty());
    for apply in [false, true] {
        let out = Guard::new()
            .run(
                &spec("", ERROR_RATE),
                &store,
                &source,
                None,
                None,
                None,
                apply,
            )
            .await
            .unwrap();
        assert_eq!(out, None);
    }
    assert!(source.log().is_empty());
    assert_eq!(files(dir.path()), before);
}

#[tokio::test]
async fn store_errors_name_the_prompt_and_label() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), None).await;
    let run = |yaml_prompt: &'static str, label: &'static str| {
        let store = &store;
        async move {
            let mut s = spec("", ERROR_RATE);
            s.prompt = yaml_prompt.into();
            s.label = Label::new(label).unwrap();
            Guard::new()
                .run(
                    &s,
                    store,
                    &FakeGuard::new(plenty()),
                    None,
                    None,
                    None,
                    false,
                )
                .await
                .unwrap_err()
                .to_string()
        }
    };
    assert_eq!(
        run("missing", "production").await,
        "report store: prompt store: missing/production: no such prompt"
    );
    assert_eq!(
        run("support", "staging").await,
        "report store: prompt store: support/staging: no such label"
    );
}

#[tokio::test]
async fn other_store_errors_are_prompt_store_errors() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("support.yaml"), "versions: [").unwrap();
    let err = Guard::new()
        .run(
            &spec("", ERROR_RATE),
            &FileStore::new(dir.path()),
            &FakeGuard::new(plenty()),
            None,
            None,
            None,
            false,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("report store: prompt store: decode: "),
        "{err}"
    );
}

const STEPS: &str = "ramp: [10, 25, 50]\nmin_step_age: 1d\n";

/// Ramps a rollout at 10% to 25% with apply, `age_secs` before `NOW`; changes that report
/// with `edit` and the store's rollout to `percent` when given; then runs `spec_extra` at
/// `NOW` with apply over `c`, passing the report as the previous one.
async fn after_step(
    spec_extra: &str,
    age_secs: i64,
    edit: impl FnOnce(&mut GuardReport),
    percent: Option<u8>,
    c: Vec<VersionCalls>,
) -> (GuardReport, (Option<u32>, Option<Rollout>)) {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path(), Some(10)).await;
    let s = spec(spec_extra, ERROR_RATE);
    let mut previous = Guard::new()
        .now(at(NOW - age_secs))
        .run(
            &s,
            &store,
            &FakeGuard::new(plenty()),
            None,
            None,
            None,
            true,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (previous.action, previous.applied),
        (Action::Ramp(25), true)
    );
    edit(&mut previous);
    if let Some(p) = percent {
        let r = Rollout::new(2, p).unwrap();
        store
            .set_rollout("support", &prod(), Some(r))
            .await
            .unwrap();
    }
    let report = Guard::new()
        .now(at(NOW))
        .run(
            &s,
            &store,
            &FakeGuard::new(c),
            None,
            None,
            Some(&previous),
            true,
        )
        .await
        .unwrap()
        .unwrap();
    (report, state(&store).await)
}

#[tokio::test]
async fn a_young_step_holds_an_advance() {
    let (report, now) = after_step(STEPS, 3 * 3600, |_| {}, None, plenty()).await;
    assert_eq!(report.verdict, Verdict::Wait);
    assert_eq!((report.action, report.applied), (Action::None, false));
    assert_eq!(
        report.note.as_deref(),
        Some("step at 25% is 3h old (< min_step_age 1d)")
    );
    assert_eq!(report.exit_code(), 3);
    assert_eq!(report.checks[0].status, Status::Ok);
    assert_eq!(now, (Some(1), Some(Rollout::new(2, 25).unwrap())));
    // The age is rounded down to minutes.
    let (report, _) = after_step(STEPS, 90 * 60 + 59, |_| {}, None, plenty()).await;
    assert_eq!(
        report.note.as_deref(),
        Some("step at 25% is 90m old (< min_step_age 1d)")
    );
}

/// A change to the previous report.
type Edit = Box<dyn FnOnce(&mut GuardReport)>;

#[tokio::test]
async fn a_step_is_not_held_when_any_condition_fails() {
    let ramped = || (Some(1), Some(Rollout::new(2, 50).unwrap()));
    let cases: Vec<(&str, i64, Edit, Option<u8>)> = vec![
        ("old enough", 24 * 3600, Box::new(|_| {}), None),
        ("other percent", 3600, Box::new(|_| {}), Some(30)),
        ("other rollout", 3600, Box::new(|r| r.rollout = 3), None),
        (
            "other prompt",
            3600,
            Box::new(|r| r.prompt = "billing".into()),
            None,
        ),
        (
            "other label",
            3600,
            Box::new(|r| r.label = "staging".into()),
            None,
        ),
        ("not applied", 3600, Box::new(|r| r.applied = false), None),
        (
            "not a ramp",
            3600,
            Box::new(|r| r.action = Action::Promote),
            None,
        ),
    ];
    for (what, age, edit, percent) in cases {
        let (report, now) = after_step(STEPS, age, edit, percent, plenty()).await;
        assert_eq!(report.verdict, Verdict::Advance, "{what}");
        assert_eq!(
            (report.action, report.applied),
            (Action::Ramp(50), true),
            "{what}"
        );
        assert_eq!(report.note, None, "{what}");
        assert_eq!(now, ramped(), "{what}");
    }
    // Without min_step_age nothing is held.
    let (report, now) = after_step("ramp: [10, 25, 50]\n", 60, |_| {}, None, plenty()).await;
    assert_eq!((report.action, report.note), (Action::Ramp(50), None));
    assert_eq!(now, ramped());
}

#[tokio::test]
async fn a_previous_report_from_the_future_counts_as_age_0() {
    let (report, now) = after_step(STEPS, -300, |_| {}, None, plenty()).await;
    assert_eq!(report.verdict, Verdict::Wait);
    assert_eq!(
        report.note.as_deref(),
        Some("step at 25% is 0m old (< min_step_age 1d)")
    );
    assert_eq!(now, (Some(1), Some(Rollout::new(2, 25).unwrap())));
}

#[tokio::test]
async fn an_abort_is_never_held() {
    let worse = calls((1000, 0), (1000, 100));
    let (report, now) = after_step(STEPS, 60, |_| {}, None, worse).await;
    assert_eq!(report.verdict, Verdict::Abort);
    assert_eq!((report.action, report.applied), (Action::Abort, true));
    assert_eq!(report.note, None);
    assert_eq!(now, (Some(1), None));
}
