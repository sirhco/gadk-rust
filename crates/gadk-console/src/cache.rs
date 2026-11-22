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

/// Parsed files keyed by stored name. Stored files are never rewritten, so a cached value never
/// goes stale; the cache keeps the `cap` most recently inserted.
pub(crate) struct ParsedCache<T> {
    cap: usize,
    inner: Mutex<Entries<T>>,
}

struct Entries<T> {
    map: HashMap<String, Arc<T>>,
    order: VecDeque<String>,
}

impl<T> ParsedCache<T> {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            inner: Mutex::new(Entries {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    /// The value `name` from the cache, else from `load` (then cached, evicting the oldest entry
    /// beyond the cap). `None` (no such file) is not cached; an error is returned as is.
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
        let value = Arc::new(value);
        let mut entries = self.lock();
        if !entries.map.contains_key(name) {
            entries.map.insert(name.to_owned(), value.clone());
            entries.order.push_back(name.to_owned());
            while entries.order.len() > self.cap {
                if let Some(old) = entries.order.pop_front() {
                    entries.map.remove(&old);
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

    #[tokio::test]
    async fn a_hit_does_not_load_again() {
        let cache = ParsedCache::new(4);
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
        let cache = ParsedCache::new(4);
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
        let cache = ParsedCache::new(1);
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
}
