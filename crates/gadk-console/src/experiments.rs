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

//! Experiment reports for the Experiments pages: a cache of parsed experiments and their links.

use std::sync::Arc;

use gadk_eval_monitor::{ExperimentReport, MonitorError, ReportStore};

use crate::cache::{MAX_RUNS, ParsedCache};
use crate::url::percent_encode;

/// The report store (its `experiments/` part) plus a cache of the `cap` most recently read
/// parsed experiments, holding at most [`MAX_RUNS`] runs in all (the newest experiment always
/// stays).
pub(crate) struct ExperimentCache {
    store: ReportStore,
    cache: ParsedCache<ExperimentReport>,
}

/// An experiment's weight in the cache: the case runs of all its variants.
pub(crate) fn experiment_runs(x: &ExperimentReport) -> usize {
    x.report.variants.iter().map(|v| v.runs.len()).sum()
}

impl ExperimentCache {
    pub(crate) fn new(store: ReportStore, cap: usize) -> Self {
        Self {
            store,
            cache: ParsedCache::new(cap, MAX_RUNS, experiment_runs),
        }
    }

    /// [`ReportStore::experiment_names`].
    pub(crate) async fn names(&self, limit: usize) -> Result<Vec<String>, MonitorError> {
        self.store.experiment_names(limit).await
    }

    /// The experiment stored as `name` from the cache, else from the store (then cached,
    /// evicting the oldest entry beyond the cap). `None` (no such experiment) is not cached.
    pub(crate) async fn find(
        &self,
        name: &str,
    ) -> Result<Option<Arc<ExperimentReport>>, MonitorError> {
        self.cache
            .find(name, || self.store.find_experiment(name))
            .await
    }
}

/// `/experiments/<stored name, percent-encoded>`.
pub(crate) fn experiment_href(name: &str) -> String {
    format!("/experiments/{}", percent_encode(name))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_eval::Outcome;

    use super::{experiment_href, experiment_runs};
    use crate::evals::CACHE_CAP;
    use crate::web::testutil::{experiment, experiment_cache};

    #[test]
    fn hrefs_are_percent_encoded() {
        assert_eq!(
            experiment_href("exp-20270115T080000Z.json"),
            "/experiments/exp-20270115T080000Z.json"
        );
        assert_eq!(experiment_href("a b"), "/experiments/a%20b");
    }

    #[tokio::test]
    async fn names_are_the_stored_experiments_newest_first() {
        let (_dir, cache, names) = experiment_cache(
            &[
                experiment("a", 1_800_000_000, &[("v1", &[("c1", &[Outcome::Pass])])]),
                experiment("b", 1_800_003_600, &[("v1", &[("c1", &[Outcome::Pass])])]),
            ],
            CACHE_CAP,
        )
        .await;
        assert_eq!(
            names,
            ["a-20270115T080000Z.json", "b-20270115T090000Z.json"]
        );
        assert_eq!(cache.names(1).await.unwrap(), ["b-20270115T090000Z.json"]);
        assert_eq!(cache.names(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn find_reads_each_experiment_once() {
        let (dir, cache, names) = experiment_cache(
            &[experiment(
                "a",
                1_800_000_000,
                &[("v1", &[("c1", &[Outcome::Pass])])],
            )],
            CACHE_CAP,
        )
        .await;
        let first = cache.find(&names[0]).await.unwrap().unwrap();
        assert_eq!(first.name, "a");
        std::fs::remove_file(dir.path().join("experiments").join(&names[0])).unwrap();
        let again = cache.find(&names[0]).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[tokio::test]
    async fn a_missing_experiment_is_none_and_an_invalid_name_an_error() {
        let (_dir, cache, _) = experiment_cache(&[], CACHE_CAP).await;
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
    async fn monitor_reports_are_not_experiments() {
        let (dir, cache, _) = experiment_cache(&[], CACHE_CAP).await;
        let store =
            gadk_eval_monitor::ReportStore::open(&dir.path().display().to_string()).unwrap();
        store
            .put(&crate::web::testutil::report("a", 1_800_000_000, &[]))
            .await
            .unwrap();
        assert!(cache.names(10).await.unwrap().is_empty());
    }

    #[test]
    fn experiment_weight_is_the_runs_of_every_variant() {
        let x = experiment(
            "a",
            1_800_000_000,
            &[
                ("v1", &[("c1", &[Outcome::Pass]), ("c1", &[Outcome::Fail])]),
                ("v2", &[("c1", &[Outcome::Pass])]),
                ("v3", &[]),
            ],
        );
        assert_eq!(experiment_runs(&x), 3);
    }
}
