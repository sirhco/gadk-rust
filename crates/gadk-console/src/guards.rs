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

//! Guard reports for the Guards pages: a cache of parsed guard reports and their links.

use std::sync::Arc;

use gadk_eval_monitor::{GuardReport, MonitorError, ReportStore};

use crate::cache::{MAX_RUNS, ParsedCache};
use crate::url::percent_encode;

/// The report store (its `guards/` part) plus a cache of the `cap` most recently read parsed
/// guard reports (each weighs 1, so the cap is the only bound).
pub(crate) struct GuardCache {
    store: ReportStore,
    cache: ParsedCache<GuardReport>,
}

impl GuardCache {
    pub(crate) fn new(store: ReportStore, cap: usize) -> Self {
        Self {
            store,
            cache: ParsedCache::new(cap, MAX_RUNS, |_| 1),
        }
    }

    /// [`ReportStore::guard_names`].
    pub(crate) async fn names(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        self.store.guard_names(limit).await
    }

    /// The guard report stored as `name` from the cache, else from the store (then cached,
    /// evicting the oldest entry beyond the cap). `None` (no such report) is not cached.
    pub(crate) async fn find(&self, name: &str) -> Result<Option<Arc<GuardReport>>, MonitorError> {
        self.cache.find(name, || self.store.find_guard(name)).await
    }
}

/// `/guards/<stored name, percent-encoded>`.
pub(crate) fn guard_href(name: &str) -> String {
    format!("/guards/{}", percent_encode(name))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval_monitor::{Action, ReportStore};

    use super::guard_href;
    use crate::evals::CACHE_CAP;
    use crate::web::testutil::{experiment, guard, guard_cache, report};

    #[test]
    fn hrefs_are_percent_encoded() {
        assert_eq!(
            guard_href("g-20270115T080000Z.json"),
            "/guards/g-20270115T080000Z.json"
        );
        assert_eq!(guard_href("a b"), "/guards/a%20b");
    }

    #[tokio::test]
    async fn names_are_the_stored_guard_reports_newest_first() {
        let (_dir, cache, names) = guard_cache(
            &[
                guard("a", 1_800_000_000, "p", 10, Action::Ramp(25), false),
                guard("b", 1_800_003_600, "p", 10, Action::Ramp(25), false),
            ],
            CACHE_CAP,
        )
        .await;
        assert_eq!(
            names,
            ["a-20270115T080000Z.json", "b-20270115T090000Z.json"]
        );
        assert_eq!(cache.names(1).await.unwrap(), ["b-20270115T090000Z.json"]);
        assert_eq!(
            cache.names(10).await.unwrap(),
            ["b-20270115T090000Z.json", "a-20270115T080000Z.json"]
        );
    }

    #[tokio::test]
    async fn find_reads_each_guard_report_once() {
        let (dir, cache, names) = guard_cache(
            &[guard("a", 1_800_000_000, "p", 10, Action::Ramp(25), false)],
            CACHE_CAP,
        )
        .await;
        let first = cache.find(&names[0]).await.unwrap().unwrap();
        assert_eq!(first.spec, "a");
        std::fs::remove_file(dir.path().join("guards").join(&names[0])).unwrap();
        let again = cache.find(&names[0]).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[tokio::test]
    async fn a_missing_guard_report_is_none_and_an_invalid_name_an_error() {
        let (_dir, cache, _) = guard_cache(&[], CACHE_CAP).await;
        assert!(
            cache
                .find("a-20270115T080000Z.json")
                .await
                .unwrap()
                .is_none()
        );
        assert!(cache.find("../x").await.is_err());
    }

    #[tokio::test]
    async fn monitor_and_experiment_reports_are_not_guard_reports() {
        let (dir, cache, _) = guard_cache(&[], CACHE_CAP).await;
        let store = ReportStore::open(&dir.path().display().to_string()).unwrap();
        store.put(&report("a", 1_800_000_000, &[])).await.unwrap();
        store
            .put_experiment(&experiment("b", 1_800_000_000, &[]))
            .await
            .unwrap();
        assert!(cache.names(10).await.unwrap().is_empty());
    }
}
