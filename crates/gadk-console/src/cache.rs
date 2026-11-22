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

//! A capped cache of parsed stored files, shared by the Evals and Experiments pages.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// How many runs (monitor runs, or experiment case runs over all variants) one cache holds in
/// all before it evicts its oldest entries.
pub(crate) const MAX_RUNS: usize = 20_000;

/// Parsed files keyed by stored name. Stored files are never rewritten, so a cached value never
/// goes stale. The cache keeps the most recently inserted entries: at most `cap` of them and at
/// most `max_runs` total weight (each entry's `weight`, its run count), except that the entry
/// just inserted always stays.
pub(crate) struct ParsedCache<T> {
    cap: usize,
    max_runs: usize,
    weight: fn(&T) -> usize,
    inner: Mutex<Entries<T>>,
}

struct Entries<T> {
    map: HashMap<String, Arc<T>>,
    /// Stored names, oldest first, each with its entry's weight.
    order: VecDeque<(String, usize)>,
    /// The sum of the weights in `order`.
    total: usize,
}

impl<T> ParsedCache<T> {
    /// An empty cache of at most `cap` entries and `max_runs` total `weight`.
    pub(crate) fn new(cap: usize, max_runs: usize, weight: fn(&T) -> usize) -> Self {
        Self {
            cap,
            max_runs,
            weight,
            inner: Mutex::new(Entries {
                map: HashMap::new(),
                order: VecDeque::new(),
                total: 0,
            }),
        }
    }

    /// The value `name` from the cache, else from `load` (then cached, evicting the oldest
    /// entries while more than `cap` entries or `max_runs` runs are held, never the new one).
    /// `None` (no such file) is not cached; an error is returned as is.
    pub(crate) async fn find<E, F, Fut>(&self, name: &str, load: F) -> Result<Option<Arc<T>>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, E>>,
    {
        let hit = self.lock().map.get(name).cloned();
        if let Some(value) = hit {
            return Ok(Some(value));
        }
        let Some(value) = load().await? else {
            return Ok(None);
        };
        let weight = (self.weight)(&value);
        let value = Arc::new(value);
        let mut entries = self.lock();
        if !entries.map.contains_key(name) {
            entries.map.insert(name.to_owned(), value.clone());
            entries.order.push_back((name.to_owned(), weight));
            entries.total = entries.total.saturating_add(weight);
            while entries.order.len() > 1
                && (entries.order.len() > self.cap || entries.total > self.max_runs)
            {
                if let Some((old, w)) = entries.order.pop_front() {
                    entries.map.remove(&old);
                    entries.total -= w;
                }
            }
        }
        Ok(Some(value))
    }

    fn lock(&self) -> MutexGuard<'_, Entries<T>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::ParsedCache;

    /// A loader that counts its calls and answers `value`.
    fn load<'a>(
        calls: &'a Cell<usize>,
        value: Option<u32>,
    ) -> impl FnOnce() -> std::future::Ready<Result<Option<u32>, String>> + 'a {
        move || {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(value))
        }
    }

    /// Every value weighs 1.
    fn one(_: &u32) -> usize {
        1
    }

    /// A value weighs itself.
    fn itself(v: &u32) -> usize {
        *v as usize
    }

    /// Whether `name` is cached (a loader answering `None` leaves the cache unchanged).
    async fn cached(cache: &ParsedCache<u32>, name: &str) -> Option<u32> {
        let calls = Cell::new(0);
        let found = cache.find(name, load(&calls, None)).await.unwrap();
        found.as_deref().copied()
    }

    #[tokio::test]
    async fn a_hit_does_not_load_again() {
        let cache = ParsedCache::new(4, usize::MAX, one);
        let calls = Cell::new(0);
        let first = cache
            .find("a", load(&calls, Some(1)))
            .await
            .unwrap()
            .unwrap();
        let again = cache
            .find("a", load(&calls, Some(2)))
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!((*again, calls.get()), (1, 1));
    }

    #[tokio::test]
    async fn none_and_errors_are_not_cached() {
        let cache = ParsedCache::new(4, usize::MAX, one);
        let calls = Cell::new(0);
        assert!(cache.find("a", load(&calls, None)).await.unwrap().is_none());
        let err = cache
            .find("a", || {
                std::future::ready(Err::<Option<u32>, _>("boom".to_owned()))
            })
            .await
            .unwrap_err();
        assert_eq!(err, "boom");
        assert_eq!(
            cache
                .find("a", load(&calls, Some(3)))
                .await
                .unwrap()
                .as_deref(),
            Some(&3)
        );
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn the_oldest_entry_is_evicted_at_the_cap() {
        let cache = ParsedCache::new(1, usize::MAX, one);
        let calls = Cell::new(0);
        cache.find("a", load(&calls, Some(1))).await.unwrap();
        cache.find("b", load(&calls, Some(2))).await.unwrap();
        assert_eq!(
            cache
                .find("b", load(&calls, None))
                .await
                .unwrap()
                .as_deref(),
            Some(&2),
            "newest stays"
        );
        assert!(
            cache.find("a", load(&calls, None)).await.unwrap().is_none(),
            "oldest evicted"
        );
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn the_cap_by_count_holds_with_a_large_run_bound() {
        let cache = ParsedCache::new(2, 1_000, itself);
        let calls = Cell::new(0);
        for (name, v) in [("a", 1), ("b", 1), ("c", 1)] {
            cache.find(name, load(&calls, Some(v))).await.unwrap();
        }
        assert_eq!(cached(&cache, "a").await, None, "oldest evicted by count");
        assert_eq!(cached(&cache, "b").await, Some(1));
        assert_eq!(cached(&cache, "c").await, Some(1));
    }

    #[tokio::test]
    async fn the_oldest_entries_are_evicted_beyond_the_run_bound() {
        let cache = ParsedCache::new(10, 5, itself);
        let calls = Cell::new(0);
        for (name, v) in [("a", 2), ("b", 2), ("c", 2)] {
            cache.find(name, load(&calls, Some(v))).await.unwrap();
        }
        // 2 + 2 + 2 = 6 runs > 5: `a` goes, leaving 4.
        assert_eq!(cached(&cache, "a").await, None, "oldest evicted by runs");
        assert_eq!(cached(&cache, "b").await, Some(2));
        assert_eq!(cached(&cache, "c").await, Some(2));
        cache.find("d", load(&calls, Some(1))).await.unwrap();
        // 2 + 2 + 1 = 5 runs: nothing more goes.
        assert_eq!(cached(&cache, "b").await, Some(2));
        assert_eq!(cached(&cache, "d").await, Some(1));
    }

    #[tokio::test]
    async fn an_oversized_newest_entry_stays() {
        let cache = ParsedCache::new(4, 5, itself);
        let calls = Cell::new(0);
        cache.find("a", load(&calls, Some(1))).await.unwrap();
        let big = cache.find("b", load(&calls, Some(9))).await.unwrap();
        assert_eq!(big.as_deref(), Some(&9));
        assert_eq!(cached(&cache, "a").await, None, "older entries go first");
        assert_eq!(cached(&cache, "b").await, Some(9), "the newest stays");
        cache.find("c", load(&calls, Some(1))).await.unwrap();
        assert_eq!(
            cached(&cache, "b").await,
            None,
            "evicted once it is no longer newest"
        );
        assert_eq!(cached(&cache, "c").await, Some(1));
    }
}
