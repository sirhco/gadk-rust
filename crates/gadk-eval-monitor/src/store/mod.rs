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
//! when that name is taken.

mod gcs;
mod local;

use std::sync::{Arc, LazyLock};

use chrono::{DateTime, NaiveDateTime, Utc};
use gadk_model::HttpClient;
use regex::Regex;
use serde::Serialize;

use crate::error::MonitorError;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource};
use crate::report::MonitorReport;
use crate::spec::is_monitor_name;

/// The most names tried for one report (the plain name, then `-2` … `-1000`).
const MAX_SUFFIX: u32 = 1000;

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

/// Where reports are kept.
pub struct ReportStore {
    backend: Backend,
}

impl ReportStore {
    /// Opens `url`: `gs://<bucket>[/<prefix>]` (Application Default Credentials; must be called
    /// inside a Tokio runtime), `file://<dir>` or a plain directory path. A local directory is
    /// created on the first `put`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] for an empty URL, a `gs://` URL without a valid bucket, or when no
    /// credentials are found.
    pub fn open(url: &str) -> Result<Self, MonitorError> {
        if url.starts_with("gs://") {
            let auth = AdcTokenSource::adc().map_err(|e| {
                MonitorError::Store(format!("Application Default Credentials: {e}"))
            })?;
            return Self::gcs_with_client(url, GcpHttp::default_http(), Arc::new(auth));
        }
        let dir = url.strip_prefix("file://").unwrap_or(url);
        if dir.is_empty() {
            return Err(MonitorError::Store(format!("invalid store URL {url:?}")));
        }
        Ok(Self {
            backend: Backend::Local(local::LocalStore::new(dir)),
        })
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
        Ok(Self {
            backend: Backend::Gcs(gcs::GcsStore::new(bucket, prefix, GcpHttp::new(http, auth))),
        })
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
        for n in 1..=MAX_SUFFIX {
            let name = if n == 1 {
                format!("{base}.json")
            } else {
                format!("{base}-{n}.json")
            };
            let created = match &self.backend {
                Backend::Local(s) => s.create(&name, &json)?,
                Backend::Gcs(s) => s.create(&name, &json).await?,
            };
            if created {
                return Ok(name);
            }
        }
        Err(MonitorError::Store(format!(
            "no free name for {base} after {MAX_SUFFIX} tries"
        )))
    }

    /// The newest `limit` reports (by the timestamp in their names), reading only those.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when listing or reading fails or a listed report is not valid.
    pub async fn list(&self, limit: usize) -> Result<Vec<ReportMeta>, MonitorError> {
        let names = match &self.backend {
            Backend::Local(s) => s.names()?,
            Backend::Gcs(s) => s.names().await?,
        };
        let mut keyed: Vec<((String, u32), String)> = names
            .into_iter()
            .filter_map(|n| name_key(&n).map(|k| (k, n)))
            .collect();
        keyed.sort_by(|a, b| b.cmp(a));
        let mut out = Vec::new();
        for (_, name) in keyed.into_iter().take(limit) {
            let report = self.get(&name).await?;
            out.push(ReportMeta::of(name, &report));
        }
        Ok(out)
    }

    /// The report stored as `name`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when `name` is not a report name
    /// (`<spec>-<YYYYMMDDTHHMMSSZ>[-<n>].json`), does not exist, cannot be read or is not a
    /// report.
    pub async fn get(&self, name: &str) -> Result<MonitorReport, MonitorError> {
        if name_key(name).is_none() {
            return Err(MonitorError::Store(format!("invalid report name {name:?}")));
        }
        let text = match &self.backend {
            Backend::Local(s) => s.read(name)?,
            Backend::Gcs(s) => s.read(name).await?,
        };
        let text = text.ok_or_else(|| MonitorError::Store(format!("report {name} not found")))?;
        MonitorReport::from_json(&text)
            .map_err(|e| MonitorError::Store(format!("report {name}: {e}")))
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
    use gadk_eval::ScoreResult;

    use crate::report::{MonitoredRun, Window};
    use crate::testutil::{at, run};

    use super::*;

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

    #[tokio::test]
    async fn put_rejects_an_invalid_spec_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        let err = store.put(&report("../x", 0, true)).await.unwrap_err();
        assert!(matches!(err, MonitorError::Store(_)), "{err}");
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
}
