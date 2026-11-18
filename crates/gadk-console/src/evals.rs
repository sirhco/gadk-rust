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

//! Monitor reports for the Evals pages: a cache of parsed reports, report-name helpers and the
//! per-monitor grouping the list page draws.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gadk_eval_monitor::{MonitorError, MonitorReport, ReportStore};

use crate::url::percent_encode;

/// How many parsed reports the console keeps.
pub(crate) const CACHE_CAP: usize = 256;

/// The report store plus a cache of parsed reports. Stored reports are never rewritten, so a
/// cached report never goes stale; the cache keeps the `cap` most recently inserted.
pub(crate) struct ReportCache {
    store: ReportStore,
    cap: usize,
    inner: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    map: HashMap<String, Arc<MonitorReport>>,
    order: VecDeque<String>,
}

impl ReportCache {
    pub(crate) fn new(store: ReportStore, cap: usize) -> Self {
        Self {
            store,
            cap,
            inner: Mutex::new(Entries::default()),
        }
    }

    /// [`ReportStore::names`].
    pub(crate) async fn names(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        self.store.names(limit).await
    }

    /// The report `name` from the cache, else from the store (then cached, evicting the oldest
    /// entry beyond the cap). `None` (no such report) is not cached.
    pub(crate) async fn find(
        &self,
        name: &str,
    ) -> Result<Option<Arc<MonitorReport>>, MonitorError> {
        let hit = self.lock().map.get(name).cloned();
        if let Some(report) = hit {
            return Ok(Some(report));
        }
        let Some(report) = self.store.find(name).await? else {
            return Ok(None);
        };
        let report = Arc::new(report);
        let mut entries = self.lock();
        if !entries.map.contains_key(name) {
            entries.map.insert(name.to_owned(), report.clone());
            entries.order.push_back(name.to_owned());
            while entries.order.len() > self.cap {
                if let Some(old) = entries.order.pop_front() {
                    entries.map.remove(&old);
                }
            }
        }
        Ok(Some(report))
    }

    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The spec part of a report name (`<spec>-<YYYYMMDDTHHMMSSZ>[-<n>].json`); `None` for anything
/// that is not a report name.
pub(crate) fn spec_of(name: &str) -> Option<&str> {
    if !ReportStore::is_report_name(name) {
        return None;
    }
    let stem = name.strip_suffix(".json")?;
    let is_stamp = |t: &str| {
        let b = t.as_bytes();
        b.len() == 16
            && b[8] == b'T'
            && b[15] == b'Z'
            && b[..8].iter().all(u8::is_ascii_digit)
            && b[9..15].iter().all(u8::is_ascii_digit)
    };
    if let Some((head, n)) = stem.rsplit_once('-')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
        && let Some((spec, stamp)) = head.rsplit_once('-')
        && is_stamp(stamp)
    {
        return Some(spec);
    }
    let (spec, stamp) = stem.rsplit_once('-')?;
    is_stamp(stamp).then_some(spec)
}

/// A pass rate as `NN.N%`, or `—` when there is none.
pub(crate) fn percent(rate: Option<f64>) -> String {
    rate.map_or_else(|| "—".to_owned(), |r| format!("{:.1}%", r * 100.0))
}

/// `/evals?spec=<spec, percent-encoded>`.
pub(crate) fn spec_href(spec: &str) -> String {
    format!("/evals?spec={}", percent_encode(spec))
}

/// `/evals/<name, percent-encoded>`.
pub(crate) fn report_href(name: &str) -> String {
    format!("/evals/{}", percent_encode(name))
}

/// One monitor's reports, for a card on the list page.
pub(crate) struct SpecSummary {
    /// The monitor spec's name (`MonitorReport::spec`).
    pub spec: String,
    /// The newest report's stored name.
    pub latest_name: String,
    /// The newest report.
    pub latest: Arc<MonitorReport>,
    /// The pass rates of the spec's reports, oldest first (`None`: a report without runs).
    pub rates: Vec<Option<f64>>,
}

/// One summary per spec from `reports` (`(stored name, report)`, newest first), ordered by each
/// spec's newest report.
pub(crate) fn group(reports: &[(String, Arc<MonitorReport>)]) -> Vec<SpecSummary> {
    let mut out: Vec<SpecSummary> = Vec::new();
    for (name, report) in reports {
        match out.iter_mut().find(|s| s.spec == report.spec) {
            Some(s) => s.rates.push(report.pass_rate()),
            None => out.push(SpecSummary {
                spec: report.spec.clone(),
                latest_name: name.clone(),
                latest: report.clone(),
                rates: vec![report.pass_rate()],
            }),
        }
    }
    for s in &mut out {
        s.rates.reverse();
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;
    use gadk_eval_monitor::ReportStore;

    use super::{CACHE_CAP, group, percent, report_href, spec_href, spec_of};
    use crate::web::testutil::{report, report_cache};

    #[test]
    fn spec_of_takes_the_name_apart() {
        assert_eq!(spec_of("support-20270115T080000Z.json"), Some("support"));
        assert_eq!(spec_of("a-b_1-20270115T080000Z-12.json"), Some("a-b_1"));
        assert_eq!(
            spec_of("x-20270115T080000Z-20270116T080000Z.json"),
            Some("x-20270115T080000Z")
        );
        for bad in [
            "support.json",
            "../a-20270115T080000Z.json",
            "a-20270115T080000Z.txt",
            "",
        ] {
            assert_eq!(spec_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn percent_and_hrefs() {
        assert_eq!(percent(Some(0.5)), "50.0%");
        assert_eq!(percent(Some(1.0)), "100.0%");
        assert_eq!(percent(None), "—");
        assert_eq!(spec_href("a b"), "/evals?spec=a%20b");
        assert_eq!(
            report_href("a-20270115T080000Z.json"),
            "/evals/a-20270115T080000Z.json"
        );
    }

    #[test]
    fn group_makes_one_summary_per_spec_newest_first() {
        let r = |spec: &str, secs, runs: &[&[Outcome]]| Arc::new(report(spec, secs, runs));
        let reports = vec![
            ("b-3".to_owned(), r("b", 3, &[&[Outcome::Fail]])),
            ("a-2".to_owned(), r("a", 2, &[&[Outcome::Pass]])),
            ("b-1".to_owned(), r("b", 1, &[])),
        ];
        let g = group(&reports);
        let specs: Vec<&str> = g.iter().map(|s| s.spec.as_str()).collect();
        assert_eq!(specs, ["b", "a"]);
        assert_eq!(g[0].latest_name, "b-3");
        assert!(Arc::ptr_eq(&g[0].latest, &reports[0].1));
        assert_eq!(g[0].rates, [None, Some(0.0)]);
        assert_eq!(g[1].rates, [Some(1.0)]);
        assert!(group(&[]).is_empty());
    }

    #[tokio::test]
    async fn names_passes_through_newest_first() {
        let (_dir, cache, _) = report_cache(
            &[
                report("a", 1_800_000_000, &[]),
                report("b", 1_800_003_600, &[]),
            ],
            CACHE_CAP,
        )
        .await;
        assert_eq!(cache.names(1).await.unwrap(), ["b-20270115T090000Z.json"]);
    }

    #[tokio::test]
    async fn find_reads_each_report_once() {
        let (dir, cache, names) = report_cache(
            &[report("a", 1_800_000_000, &[&[Outcome::Pass]])],
            CACHE_CAP,
        )
        .await;
        let first = cache.find(&names[0]).await.unwrap().unwrap();
        std::fs::remove_file(dir.path().join(&names[0])).unwrap();
        let again = cache.find(&names[0]).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[tokio::test]
    async fn the_oldest_entry_is_evicted_at_the_cap() {
        let (dir, cache, names) = report_cache(
            &[
                report("a", 1_800_000_000, &[]),
                report("b", 1_800_000_000, &[]),
            ],
            1,
        )
        .await;
        cache.find(&names[0]).await.unwrap().unwrap();
        cache.find(&names[1]).await.unwrap().unwrap();
        for name in &names {
            std::fs::remove_file(dir.path().join(name)).unwrap();
        }
        assert!(
            cache.find(&names[1]).await.unwrap().is_some(),
            "newest stays"
        );
        assert!(
            cache.find(&names[0]).await.unwrap().is_none(),
            "oldest evicted"
        );
    }

    #[tokio::test]
    async fn a_missing_report_is_none_and_not_cached() {
        let (dir, cache, _) = report_cache(&[], CACHE_CAP).await;
        let name = "a-20270115T080000Z.json";
        assert!(cache.find(name).await.unwrap().is_none());
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        assert_eq!(
            store.put(&report("a", 1_800_000_000, &[])).await.unwrap(),
            name
        );
        assert!(cache.find(name).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn invalid_names_are_errors() {
        let (_dir, cache, _) = report_cache(&[], CACHE_CAP).await;
        assert!(cache.find("../x").await.is_err());
    }
}
