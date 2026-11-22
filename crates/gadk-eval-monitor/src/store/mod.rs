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

//! Report storage: a local directory or a Google Cloud Storage bucket prefix.
//!
//! A report is stored as `<spec>-<generated_at %Y%m%dT%H%M%SZ>.json`, with `-2`, `-3`, … added
//! when that name is taken. An [`ExperimentReport`] is stored the same way, named after the
//! experiment, under `experiments/` (a subdirectory, or the object prefix `<prefix>experiments/`);
//! the report listing never includes it.

mod gcs;
mod local;

use std::sync::{Arc, LazyLock};

use chrono::{DateTime, NaiveDateTime, Utc};
use gadk_model::HttpClient;
use regex::Regex;
use serde::Serialize;

use crate::error::MonitorError;
use crate::experiment::ExperimentReport;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource};
use crate::report::MonitorReport;
use crate::spec::is_monitor_name;

/// The most names tried for one report (the plain name, then `-2` … `-1000`).
const MAX_SUFFIX: u32 = 1000;

/// The subdirectory (local) or object prefix segment (GCS) of experiment reports.
const EXPERIMENTS: &str = "experiments";

/// A stored report's name: `<spec>-<timestamp>[-<n>].json`.
static NAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]+-(\d{8}T\d{6}Z)(?:-(\d+))?\.json$").ok());

/// A stored report, summarised.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportMeta {
    /// The stored name.
    pub name: String,
    /// The monitor spec's name.
    pub spec: String,
    /// When the report was made.
    pub generated_at: DateTime<Utc>,
    /// The number of graded runs.
    pub runs: usize,
    /// [`MonitorReport::pass_rate`].
    pub pass_rate: Option<f64>,
    /// [`MonitorReport::error_count`].
    pub errors: usize,
}

impl ReportMeta {
    fn of(name: String, r: &MonitorReport) -> Self {
        Self {
            name,
            spec: r.spec.clone(),
            generated_at: r.generated_at,
            runs: r.runs.len(),
            pass_rate: r.pass_rate(),
            errors: r.error_count(),
        }
    }
}

enum Backend {
    Local(local::LocalStore),
    Gcs(gcs::GcsStore),
}

impl Backend {
    /// The same kind of store one level down: the subdirectory or object prefix `name`.
    fn child(&self, name: &str) -> Self {
        match self {
            Backend::Local(s) => Backend::Local(s.child(name)),
            Backend::Gcs(s) => Backend::Gcs(s.child(name)),
        }
    }

    /// Writes `body` as `name` unless it exists; `false` when it does.
    async fn create(&self, name: &str, body: &str) -> Result<bool, MonitorError> {
        match self {
            Backend::Local(s) => s.create(name, body),
            Backend::Gcs(s) => s.create(name, body).await,
        }
    }

    /// The file (object) names directly in the store.
    async fn names(&self) -> Result<Vec<String>, MonitorError> {
        match self {
            Backend::Local(s) => s.names(),
            Backend::Gcs(s) => s.names().await,
        }
    }

    /// The contents of `name`, or `None` when it does not exist.
    async fn read(&self, name: &str) -> Result<Option<String>, MonitorError> {
        match self {
            Backend::Local(s) => s.read(name),
            Backend::Gcs(s) => s.read(name).await,
        }
    }

    /// Writes `body` as `<base>.json`, or as `<base>-2.json`, `<base>-3.json`, … when that name
    /// is taken, never overwriting; returns the name written.
    async fn create_unique(&self, base: &str, body: &str) -> Result<String, MonitorError> {
        for n in 1..=MAX_SUFFIX {
            let name = if n == 1 {
                format!("{base}.json")
            } else {
                format!("{base}-{n}.json")
            };
            if self.create(&name, body).await? {
                return Ok(name);
            }
        }
        Err(MonitorError::Store(format!(
            "no free name for {base} after {MAX_SUFFIX} tries"
        )))
    }

    /// The newest `limit` stored names in the store (by [`name_key`]), newest first.
    async fn newest(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        let mut keyed: Vec<((String, u32), String)> = self
            .names()
            .await?
            .into_iter()
            .filter_map(|n| name_key(&n).map(|k| (k, n)))
            .collect();
        keyed.sort_by(|a, b| b.cmp(a));
        Ok(keyed.into_iter().take(limit).map(|(_, n)| n).collect())
    }
}

/// Where reports are kept.
pub struct ReportStore {
    backend: Backend,
    /// `backend`'s `experiments` child.
    experiments: Backend,
}

impl ReportStore {
    /// Opens `url`: `gs://<bucket>[/<prefix>]` (Application Default Credentials; must be called
    /// inside a Tokio runtime), `file://<dir>` or a plain directory path. A local directory is
    /// created on the first `put`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] for an empty URL, a URL with another scheme (such as `s3://`), a
    /// `gs://` URL without a valid bucket, or when no credentials can be built; some credential
    /// failures (such as metadata-server credentials with no metadata server) only show on the
    /// first request.
    pub fn open(url: &str) -> Result<Self, MonitorError> {
        if url.starts_with("gs://") {
            let auth = AdcTokenSource::adc().map_err(|e| {
                MonitorError::Store(format!("Application Default Credentials: {e}"))
            })?;
            return Self::gcs_with_client(url, GcpHttp::default_http(), Arc::new(auth));
        }
        if let Some((scheme, _)) = url.split_once("://")
            && scheme != "file"
        {
            return Err(MonitorError::Store(format!(
                "unsupported store URL scheme {scheme:?} in {url:?} (expected gs://, file:// or a \
                 directory path)"
            )));
        }
        let dir = url.strip_prefix("file://").unwrap_or(url);
        if dir.is_empty() {
            return Err(MonitorError::Store(format!("invalid store URL {url:?}")));
        }
        Ok(Self::with_backend(Backend::Local(local::LocalStore::new(
            dir,
        ))))
    }

    /// A store over `backend`, with experiments in its `experiments` child.
    fn with_backend(backend: Backend) -> Self {
        let experiments = backend.child(EXPERIMENTS);
        Self {
            backend,
            experiments,
        }
    }

    /// A `gs://<bucket>[/<prefix>]` store with an explicit HTTP client and credentials.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when `url` is not `gs://` with a bucket of `[a-z0-9_.-]`.
    pub fn gcs_with_client(
        url: &str,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn TokenSource>,
    ) -> Result<Self, MonitorError> {
        let invalid = || MonitorError::Store(format!("invalid store URL {url:?}"));
        let rest = url.strip_prefix("gs://").ok_or_else(invalid)?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        let valid_bucket = !bucket.is_empty()
            && bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
        if !valid_bucket {
            return Err(invalid());
        }
        Ok(Self::with_backend(Backend::Gcs(gcs::GcsStore::new(
            bucket,
            prefix,
            GcpHttp::new(http, auth),
        ))))
    }

    /// Stores `report` and returns its name.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when the spec name is invalid, no free name is found, or the write
    /// fails.
    pub async fn put(&self, report: &MonitorReport) -> Result<String, MonitorError> {
        if !is_monitor_name(&report.spec) {
            return Err(MonitorError::Store(format!(
                "invalid spec name {:?} (expected [A-Za-z0-9_-]{{1,64}})",
                report.spec
            )));
        }
        let base = format!(
            "{}-{}",
            report.spec,
            report.generated_at.format("%Y%m%dT%H%M%SZ")
        );
        let json = report.to_json()?;
        self.backend.create_unique(&base, &json).await
    }

    /// Whether `name` is a stored report's name (`<spec>-<YYYYMMDDTHHMMSSZ>[-<n>].json`).
    pub fn is_report_name(name: &str) -> bool {
        name_key(name).is_some()
    }

    /// The names of the newest `limit` reports (by the timestamp in their names, then the
    /// suffix), newest first, reading no report. Other files in the store are ignored.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when listing fails.
    pub async fn names(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        self.backend.newest(limit).await
    }

    /// The newest `limit` reports (by the timestamp in their names), reading only those.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when listing or reading fails or a listed report is not valid.
    pub async fn list(&self, limit: usize) -> Result<Vec<ReportMeta>, MonitorError> {
        let mut out = Vec::new();
        for name in self.names(limit).await? {
            let report = self.get(&name).await?;
            out.push(ReportMeta::of(name, &report));
        }
        Ok(out)
    }

    /// The report stored as `name`, or `None` when no report has that name.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when `name` is not a report name
    /// (`<spec>-<YYYYMMDDTHHMMSSZ>[-<n>].json`), or the report cannot be read or is not a report.
    pub async fn find(&self, name: &str) -> Result<Option<MonitorReport>, MonitorError> {
        if name_key(name).is_none() {
            return Err(MonitorError::Store(format!("invalid report name {name:?}")));
        }
        let Some(text) = self.backend.read(name).await? else {
            return Ok(None);
        };
        MonitorReport::from_json(&text)
            .map(Some)
            .map_err(|e| MonitorError::Store(format!("report {name}: {e}")))
    }

    /// The report stored as `name`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when `name` is not a report name
    /// (`<spec>-<YYYYMMDDTHHMMSSZ>[-<n>].json`), does not exist, cannot be read or is not a
    /// report.
    pub async fn get(&self, name: &str) -> Result<MonitorReport, MonitorError> {
        self.find(name)
            .await?
            .ok_or_else(|| MonitorError::Store(format!("report {name} not found")))
    }

    /// Stores the experiment `report` under `experiments/` as
    /// `<name>-<generated_at %Y%m%dT%H%M%SZ>.json` (with `-2`, `-3`, … when taken), never
    /// overwriting, and returns that file name without the `experiments/` part (the stored
    /// name). A local `experiments` directory is created when missing.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when the experiment name is not `[A-Za-z0-9_-]{1,64}`, no free
    /// name is found, or the write fails.
    pub async fn put_experiment(&self, report: &ExperimentReport) -> Result<String, MonitorError> {
        if !is_monitor_name(&report.name) {
            return Err(MonitorError::Store(format!(
                "invalid experiment name {:?} (expected [A-Za-z0-9_-]{{1,64}})",
                report.name
            )));
        }
        let base = format!(
            "{}-{}",
            report.name,
            report.generated_at.format("%Y%m%dT%H%M%SZ")
        );
        let json = report.to_json()?;
        self.experiments.create_unique(&base, &json).await
    }

    /// The stored names of the newest `limit` experiments under `experiments/` (by the
    /// timestamp in their names, then the suffix), newest first, reading none of them. Other
    /// files there are ignored.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when listing fails.
    pub async fn experiment_names(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        self.experiments.newest(limit).await
    }

    /// The experiment stored as `name` (a stored name, without `experiments/`), or `None` when
    /// there is none.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when `name` is not a report name
    /// (`<name>-<YYYYMMDDTHHMMSSZ>[-<n>].json`), or the file cannot be read or is not an
    /// experiment report.
    pub async fn find_experiment(
        &self,
        name: &str,
    ) -> Result<Option<ExperimentReport>, MonitorError> {
        if name_key(name).is_none() {
            return Err(MonitorError::Store(format!(
                "invalid experiment report name {name:?}"
            )));
        }
        let Some(text) = self.experiments.read(name).await? else {
            return Ok(None);
        };
        ExperimentReport::from_json(&text)
            .map(Some)
            .map_err(|e| MonitorError::Store(format!("experiment {name}: {e}")))
    }
}

/// The sort key of a valid report name: its timestamp text and suffix (1 for none).
fn name_key(name: &str) -> Option<(String, u32)> {
    let caps = NAME.as_ref()?.captures(name)?;
    let stamp = caps.get(1)?.as_str();
    NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()?;
    let suffix = match caps.get(2) {
        Some(n) => n.as_str().parse().ok()?,
        None => 1,
    };
    Some((stamp.to_owned(), suffix))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gadk_eval::{
        CaseRun, EvalReport, MeasuredReport, RunMetrics, RunTranscript, ScoreResult, TokenCounts,
        Variant, VariantReport,
    };

    use crate::report::{MonitoredRun, Window};
    use crate::testutil::{at, run};

    use super::*;

    /// A one-variant, one-run experiment `name` made at `secs`.
    pub(crate) fn experiment(name: &str, secs: i64) -> ExperimentReport {
        let report = EvalReport {
            set: "smoke".into(),
            variants: vec![VariantReport {
                name: "v1".into(),
                runs: vec![CaseRun {
                    case: "capital".into(),
                    run: 1,
                    transcript: RunTranscript::default(),
                    scores: vec![ScoreResult::check("contains", true, "r")],
                }],
            }],
        };
        let metrics = vec![vec![RunMetrics::new(
            1_200,
            BTreeMap::from([("gemini-3.8-flash".to_owned(), TokenCounts::new(10, 0, 5, 0))]),
        )]];
        ExperimentReport::new(
            name,
            at(secs),
            &[Variant::new("v1")],
            MeasuredReport::new(report, metrics),
        )
    }

    /// A one-run report of `spec` made at `secs`; the run passes when `pass`.
    pub(crate) fn report(spec: &str, secs: i64, pass: bool) -> MonitorReport {
        MonitorReport {
            spec: spec.into(),
            generated_at: at(secs),
            window: Window {
                from: at(secs - 3_600),
                to: at(secs),
            },
            source: "bigquery:p.d.t".into(),
            runs: vec![MonitoredRun {
                run: run("inv", secs - 60, &[("hi", "hello", &[])]),
                scores: vec![ScoreResult::check("contains", pass, "r")],
            }],
        }
    }

    #[test]
    fn names_are_validated_and_keyed() {
        assert_eq!(
            name_key("support-20261117T080000Z.json"),
            Some(("20261117T080000Z".into(), 1))
        );
        assert_eq!(
            name_key("a-b_1-20261117T080000Z-12.json"),
            Some(("20261117T080000Z".into(), 12))
        );
        for bad in [
            "support-20261117T080000Z.txt",
            "../support-20261117T080000Z.json",
            "sub/support-20261117T080000Z.json",
            "support-20261317T080000Z.json",
            "-20261117T080000Z.json",
            "support.json",
            "notes.txt",
        ] {
            assert_eq!(name_key(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn local_round_trip_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("file://{}", dir.path().join("reports").display());
        let store = ReportStore::open(&url).unwrap();
        let r = report("support", 1_800_000_000, true);
        assert_eq!(
            store.put(&r).await.unwrap(),
            "support-20270115T080000Z.json"
        );
        assert_eq!(
            store.put(&r).await.unwrap(),
            "support-20270115T080000Z-2.json"
        );
        assert_eq!(
            store.put(&r).await.unwrap(),
            "support-20270115T080000Z-3.json"
        );
        assert_eq!(
            store.get("support-20270115T080000Z-2.json").await.unwrap(),
            r
        );
    }

    #[tokio::test]
    async fn local_list_is_newest_first_and_reads_only_the_listed_reports() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        assert!(store.list(10).await.unwrap().is_empty());
        store.put(&report("a", 1_800_000_000, true)).await.unwrap();
        store.put(&report("b", 1_800_003_600, false)).await.unwrap();
        store.put(&report("b", 1_800_003_600, true)).await.unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a report").unwrap();
        // An unreadable old report: listing the newest two never opens it.
        std::fs::write(dir.path().join("a-20200101T000000Z.json"), "{").unwrap();
        let list = store.list(2).await.unwrap();
        assert_eq!(
            list,
            [
                ReportMeta {
                    name: "b-20270115T090000Z-2.json".into(),
                    spec: "b".into(),
                    generated_at: at(1_800_003_600),
                    runs: 1,
                    pass_rate: Some(1.0),
                    errors: 0,
                },
                ReportMeta {
                    name: "b-20270115T090000Z.json".into(),
                    spec: "b".into(),
                    generated_at: at(1_800_003_600),
                    runs: 1,
                    pass_rate: Some(0.0),
                    errors: 0,
                },
            ]
        );
        assert_eq!(
            store.list(3).await.unwrap()[2].name,
            "a-20270115T080000Z.json"
        );
        assert!(matches!(store.list(4).await, Err(MonitorError::Store(_))));
    }

    #[tokio::test]
    async fn get_rejects_invalid_and_missing_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        for bad in ["../etc/passwd", "a.json", "a-20270115T080000Z.json/../x"] {
            let err = store.get(bad).await.unwrap_err();
            assert!(
                matches!(&err, MonitorError::Store(m) if m.starts_with("invalid report name")),
                "{bad}: {err}"
            );
        }
        let err = store.get("a-20270115T080000Z.json").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "report store: report a-20270115T080000Z.json not found"
        );
    }

    #[test]
    fn is_report_name_matches_the_name_check() {
        assert!(ReportStore::is_report_name("support-20261117T080000Z.json"));
        assert!(ReportStore::is_report_name(
            "a-b_1-20261117T080000Z-12.json"
        ));
        for bad in [
            "../support-20261117T080000Z.json",
            "support-20261317T080000Z.json",
            "support.json",
            "",
        ] {
            assert!(!ReportStore::is_report_name(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn local_names_are_newest_first_and_read_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = ReportStore::open(&dir.path().join("missing").display().to_string()).unwrap();
        assert!(missing.names(10).await.unwrap().is_empty());

        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        store.put(&report("a", 1_800_000_000, true)).await.unwrap();
        store.put(&report("b", 1_800_003_600, false)).await.unwrap();
        store.put(&report("b", 1_800_003_600, true)).await.unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a report").unwrap();
        // An unreadable report is still named: names() opens no report.
        std::fs::write(dir.path().join("c-20200101T000000Z.json"), "{").unwrap();
        assert_eq!(
            store.names(10).await.unwrap(),
            [
                "b-20270115T090000Z-2.json",
                "b-20270115T090000Z.json",
                "a-20270115T080000Z.json",
                "c-20200101T000000Z.json",
            ]
        );
        assert_eq!(
            store.names(2).await.unwrap(),
            ["b-20270115T090000Z-2.json", "b-20270115T090000Z.json"]
        );
        assert!(store.names(0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn find_is_none_for_a_missing_report_and_errs_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        assert_eq!(store.find("a-20270115T080000Z.json").await.unwrap(), None);

        let r = report("a", 1_800_000_000, true);
        let name = store.put(&r).await.unwrap();
        assert_eq!(store.find(&name).await.unwrap(), Some(r));

        let err = store.find("../x").await.unwrap_err();
        assert!(
            matches!(&err, MonitorError::Store(m) if m.starts_with("invalid report name")),
            "{err}"
        );
        std::fs::write(dir.path().join("b-20270115T080000Z.json"), "{").unwrap();
        let err = store.find("b-20270115T080000Z.json").await.unwrap_err();
        assert!(
            matches!(&err, MonitorError::Store(m) if m.starts_with("report b-20270115T080000Z.json: ")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn put_rejects_an_invalid_spec_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        let err = store.put(&report("../x", 0, true)).await.unwrap_err();
        assert!(matches!(err, MonitorError::Store(_)), "{err}");
    }

    #[test]
    fn open_rejects_other_schemes() {
        for bad in ["gcs://b", "s3://b/p", "https://example.com/r"] {
            match ReportStore::open(bad) {
                Err(MonitorError::Store(e)) => assert!(
                    e.contains("gs://, file:// or a directory path"),
                    "{bad}: {e}"
                ),
                Err(e) => panic!("{bad}: {e}"),
                Ok(_) => panic!("{bad} opened"),
            }
        }
        assert!(ReportStore::open("file:///tmp/reports").is_ok());
        assert!(ReportStore::open("reports/a").is_ok());
    }

    #[test]
    fn open_rejects_bad_urls() {
        assert!(matches!(ReportStore::open(""), Err(MonitorError::Store(_))));
        assert!(matches!(
            ReportStore::open("file://"),
            Err(MonitorError::Store(_))
        ));
        let http: Arc<dyn HttpClient> = Arc::new(gadk_model::ReqwestClient::default());
        for bad in ["gs://", "gs://Bad_Bucket", "s3://b"] {
            assert!(
                matches!(
                    ReportStore::gcs_with_client(bad, http.clone(), Arc::new(crate::gcp::NoAuth)),
                    Err(MonitorError::Store(_))
                ),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn local_experiments_go_under_experiments_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("reports");
        let store = ReportStore::open(&root.display().to_string()).unwrap();
        let r = experiment("exp-e2e", 1_800_000_000);
        assert_eq!(
            store.put_experiment(&r).await.unwrap(),
            "exp-e2e-20270115T080000Z.json"
        );
        assert_eq!(
            store.put_experiment(&r).await.unwrap(),
            "exp-e2e-20270115T080000Z-2.json"
        );
        let file = root
            .join("experiments")
            .join("exp-e2e-20270115T080000Z-2.json");
        assert_eq!(
            ExperimentReport::from_json(&std::fs::read_to_string(file).unwrap()).unwrap(),
            r
        );
        assert_eq!(
            store
                .find_experiment("exp-e2e-20270115T080000Z.json")
                .await
                .unwrap(),
            Some(r)
        );
    }

    #[tokio::test]
    async fn local_experiment_names_are_newest_first_and_read_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = ReportStore::open(&dir.path().join("missing").display().to_string()).unwrap();
        assert!(missing.experiment_names(10).await.unwrap().is_empty());

        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        store
            .put_experiment(&experiment("a", 1_800_000_000))
            .await
            .unwrap();
        store
            .put_experiment(&experiment("b", 1_800_003_600))
            .await
            .unwrap();
        store
            .put_experiment(&experiment("b", 1_800_003_600))
            .await
            .unwrap();
        let exps = dir.path().join("experiments");
        std::fs::write(exps.join("notes.txt"), "not a report").unwrap();
        // An unreadable experiment is still named: experiment_names() opens none.
        std::fs::write(exps.join("c-20200101T000000Z.json"), "{").unwrap();
        assert_eq!(
            store.experiment_names(10).await.unwrap(),
            [
                "b-20270115T090000Z-2.json",
                "b-20270115T090000Z.json",
                "a-20270115T080000Z.json",
                "c-20200101T000000Z.json",
            ]
        );
        assert_eq!(
            store.experiment_names(1).await.unwrap(),
            ["b-20270115T090000Z-2.json"]
        );
        assert!(store.experiment_names(0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn find_experiment_is_none_when_missing_and_errs_for_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        assert_eq!(
            store
                .find_experiment("exp-20270115T080000Z.json")
                .await
                .unwrap(),
            None
        );
        for bad in [
            "../x",
            "a.json",
            "experiments/exp-20270115T080000Z.json",
            "../exp-20270115T080000Z.json",
        ] {
            let err = store.find_experiment(bad).await.unwrap_err();
            assert!(
                matches!(&err, MonitorError::Store(m) if m.starts_with("invalid experiment report name")),
                "{bad}: {err}"
            );
        }
        std::fs::create_dir_all(dir.path().join("experiments")).unwrap();
        std::fs::write(
            dir.path()
                .join("experiments")
                .join("b-20270115T080000Z.json"),
            "{",
        )
        .unwrap();
        let err = store
            .find_experiment("b-20270115T080000Z.json")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, MonitorError::Store(m) if m.starts_with("experiment b-20270115T080000Z.json: ")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn put_experiment_rejects_an_invalid_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        for bad in ["a b", "../x", ""] {
            let err = store
                .put_experiment(&experiment(bad, 1_800_000_000))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, MonitorError::Store(m) if m.starts_with("invalid experiment name")),
                "{bad:?}: {err}"
            );
        }
        assert!(!dir.path().join("experiments").exists());
    }

    #[tokio::test]
    async fn local_report_listing_ignores_experiments() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        let r = report("support", 1_800_000_000, true);
        store.put(&r).await.unwrap();
        // Named like a monitor report, but under experiments/.
        let exp = store
            .put_experiment(&experiment("exp-e2e", 1_800_003_600))
            .await
            .unwrap();
        assert_eq!(exp, "exp-e2e-20270115T090000Z.json");
        assert_eq!(
            store.names(10).await.unwrap(),
            ["support-20270115T080000Z.json"]
        );
        let list = store.list(10).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "support-20270115T080000Z.json");
        assert_eq!(store.find(&exp).await.unwrap(), None);
        assert_eq!(store.experiment_names(10).await.unwrap(), [exp]);
        assert_eq!(
            store
                .find_experiment("support-20270115T080000Z.json")
                .await
                .unwrap(),
            None
        );
    }
}
