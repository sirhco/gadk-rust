// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! The credential cache seam (port of ADK-Go `auth/store.go`): the [`CredentialStore`] trait,
//! its [`CredentialKey`], the [`EXPIRY_SKEW`] margin and the process-local
//! [`InMemoryCredentialStore`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::credential::Credential;
use crate::error::Error;

/// Go `ExpirySkew` (store.go:39): an entry expiring within this much of now counts as expired.
pub const EXPIRY_SKEW: Duration = Duration::from_secs(10);

/// Go `CredentialKey` (store.go:46): one store entry, per app, per end user and per whatever
/// else (`key`) decides which credential comes back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CredentialKey {
    /// The app the credential belongs to.
    pub app_name: String,
    /// The end user the credential acts for.
    pub user_id: String,
    /// The provider's own discriminator.
    pub key: String,
}

/// Go `CredentialStore` (store.go:83). Go's `(cred, ok, err)` is `Result<Option<_>>`, so a hit
/// with an error and a hit with no credential are unrepresentable.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    /// The live entry for `key`, if any.
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, Error>;
    /// Stores `cred` for `key` until `expires_at`.
    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), Error>;
    /// Removes the entry for `key`.
    async fn delete(&self, key: &CredentialKey) -> Result<(), Error>;
}

/// Go `InMemoryCredentialStore` (store.go:180): a concurrency-safe, process-local
/// [`CredentialStore`] with per-entry expiry and no size limit.
///
/// An expired entry is dropped when its key is read, and otherwise by a sweep that any call at
/// least a minute after the previous sweep performs, so the resident count is the number of
/// distinct keys written within one credential lifetime. Nothing runs on its own: a store nobody
/// calls keeps what it holds, so delete credential material you must not keep.
///
/// Expiry and the sweep are judged on the store's own clock, read from the
/// [`TimeProvider`](gadk_platform::TimeProvider) given at construction (the wall clock for
/// [`new`](Self::new) and [`default`](Self::default)). No caller's clock reaches it: a credential
/// dies when its issuer says it does, and the sweep judges entries other callers own.
pub struct InMemoryCredentialStore {
    /// Guards every field. Not a read-write lock: `get` evicts and can sweep, so readers write.
    state: parking_lot::Mutex<StoreState>,
    clock: Arc<dyn gadk_platform::TimeProvider>,
}

struct StoreState {
    entries: HashMap<CredentialKey, CacheEntry>,
    /// Go `lastSweep`, on the store's own clock; `None` is Go's zero time (a sweep is due).
    last_sweep: Option<DateTime<Utc>>,
    /// Go `sweepEvery`; only tests set it. `None` and `Some(Duration::ZERO)` both mean
    /// [`SWEEP_INTERVAL`], as Go's zero does, so it cannot ask for a sweep on every call.
    sweep_every: Option<Duration>,
}

struct CacheEntry {
    cred: Arc<dyn Credential>,
    expires_at: DateTime<Utc>,
}

impl InMemoryCredentialStore {
    /// Go `NewInMemoryCredentialStore`: an empty store on the wall clock (`Utc::now`).
    pub fn new() -> Self {
        Self::with_time_provider(Arc::new(Utc::now))
    }

    /// The same store reading `clock` (D5): every expiry check and the sweep pacing use it.
    pub fn with_time_provider(clock: Arc<dyn gadk_platform::TimeProvider>) -> Self {
        Self {
            state: parking_lot::Mutex::new(StoreState {
                entries: HashMap::new(),
                last_sweep: None,
                sweep_every: None,
            }),
            clock,
        }
    }
}

impl Default for InMemoryCredentialStore {
    /// Go's zero value, which is ready to use: the same as [`new`](Self::new).
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemoryCredentialStore {
    /// The entry count only: keys name apps and end users, and entries are bearer material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.state.lock().entries.len();
        f.debug_struct("InMemoryCredentialStore")
            .field("entries", &entries)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    /// Go `Get` (store.go:203): sweeps if one is due, then serves the entry for `key` unless it
    /// is expired, in which case it evicts it and reports a miss.
    async fn get(&self, key: &CredentialKey) -> Result<Option<Arc<dyn Credential>>, Error> {
        let mut state = self.state.lock();
        let now = self.clock.now();
        state.sweep(now);
        let hit = match state.entries.get(key) {
            None => return Ok(None),
            Some(e) => (!expired(now, e.expires_at)).then(|| Arc::clone(&e.cred)),
        };
        if hit.is_none() {
            state.entries.remove(key);
        }
        Ok(hit)
    }

    /// Go `Set` (store.go:220). Go's nil-credential and zero-expiry errors are unreachable: the
    /// types require both. An `expires_at` already within [`EXPIRY_SKEW`] of now is accepted and
    /// never served; declining to write it is the producer's job.
    async fn set(
        &self,
        key: &CredentialKey,
        cred: Arc<dyn Credential>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), Error> {
        let mut state = self.state.lock();
        let now = self.clock.now();
        state.sweep(now);
        state
            .entries
            .insert(key.clone(), CacheEntry { cred, expires_at });
        Ok(())
    }

    /// Go `Delete` (store.go:273): sweeps if one is due, then removes `key`. An absent key is
    /// not an error.
    async fn delete(&self, key: &CredentialKey) -> Result<(), Error> {
        let mut state = self.state.lock();
        let now = self.clock.now();
        state.sweep(now);
        state.entries.remove(key);
        Ok(())
    }
}

impl StoreState {
    /// Go `sweep` (store.go:256): drops every expired entry, at most once per sweep interval.
    /// `now` is always the store's own clock. A clock that steps backwards gives a negative gap,
    /// which is under the interval, so no sweep runs until it catches up (as in Go).
    fn sweep(&mut self, now: DateTime<Utc>) {
        let every = match self.sweep_every {
            Some(every) if !every.is_zero() => every,
            _ => SWEEP_INTERVAL,
        };
        if let Some(last) = self.last_sweep {
            let every = TimeDelta::from_std(every).unwrap_or(TimeDelta::MAX);
            if now.signed_duration_since(last) < every {
                return;
            }
        }
        self.last_sweep = Some(now);
        self.entries.retain(|_, e| !expired(now, e.expires_at));
    }
}

/// store.go:158 `sweepInterval`: the shortest time between sweeps of expired entries.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// store.go:249 `expired`: `!(expires_at > now + EXPIRY_SKEW)`. The gcp provider's caching floor
/// (`cache_until`) is its complement. Exactly [`EXPIRY_SKEW`] left counts as spent. A `now` so
/// close to the end of `DateTime`'s range that adding the skew overflows leaves nothing later, so
/// it is expired.
pub(crate) fn expired(now: DateTime<Utc>, expires_at: DateTime<Utc>) -> bool {
    let skew = TimeDelta::from_std(EXPIRY_SKEW).unwrap_or(TimeDelta::MAX);
    match now.checked_add_signed(skew) {
        Some(limit) => expires_at <= limit,
        None => true,
    }
}

#[cfg(test)]
impl InMemoryCredentialStore {
    /// store_internal_test.go:181 `arm`: makes the next call sweep. Clearing `last_sweep` makes
    /// the sweep due whatever the clock's resolution; the one-nanosecond interval keeps every
    /// later call due as well.
    pub(crate) fn arm(&self) {
        let mut state = self.state.lock();
        state.sweep_every = Some(Duration::from_nanos(1));
        state.last_sweep = None;
    }

    /// store_internal_test.go:188 `size`: the resident entry count, expired entries included.
    pub(crate) fn size(&self) -> usize {
        self.state.lock().entries.len()
    }

    /// store_internal_test.go:194 `swept`: Go `lastSweep` (`None` = zero).
    pub(crate) fn swept(&self) -> Option<DateTime<Utc>> {
        self.state.lock().last_sweep
    }

    /// Go's `s.sweepEvery = …` under `s.mu` (store_internal_test.go:112-114). `Duration::ZERO`
    /// means the default interval, as Go's zero does.
    pub(crate) fn set_sweep_every(&self, every: Duration) {
        self.state.lock().sweep_every = Some(every);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::credential::Bearer;

    /// A clock the test moves by hand.
    fn manual_clock(
        start: DateTime<Utc>,
    ) -> (
        Arc<parking_lot::Mutex<DateTime<Utc>>>,
        Arc<dyn gadk_platform::TimeProvider>,
    ) {
        let now = Arc::new(parking_lot::Mutex::new(start));
        let read = Arc::clone(&now);
        (now, Arc::new(move || *read.lock()))
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap()
    }

    fn key(user: &str) -> CredentialKey {
        CredentialKey {
            app_name: "app".into(),
            user_id: user.into(),
            key: "res".into(),
        }
    }

    fn bearer() -> Arc<dyn Credential> {
        Arc::new(Bearer {
            token: "tok-secret".into(),
        })
    }

    /// D5: expiry is judged on the injected clock, not the wall clock.
    #[tokio::test]
    async fn injected_clock_judges_expiry() {
        let (now, clock) = manual_clock(t0());
        let s = InMemoryCredentialStore::with_time_provider(clock);
        s.set(&key("u"), bearer(), t0() + TimeDelta::seconds(11))
            .await
            .unwrap();
        assert!(
            s.get(&key("u")).await.unwrap().is_some(),
            "11s left at t0 = miss, want hit"
        );

        *now.lock() = t0() + TimeDelta::seconds(1);
        assert!(
            s.get(&key("u")).await.unwrap().is_none(),
            "exactly EXPIRY_SKEW left = hit, want miss"
        );
        assert_eq!(s.size(), 0, "the expired entry was not evicted");
    }

    /// D5: the sweep is paced by the store's clock: nothing at 59s, a sweep at 60s.
    #[tokio::test]
    async fn sweep_is_paced_on_the_store_clock() {
        let (now, clock) = manual_clock(t0());
        let s = InMemoryCredentialStore::with_time_provider(clock);
        // The first call sweeps an empty map, so the next sweep is due at t0 + 60s.
        s.set(&key("gone"), bearer(), t0()).await.unwrap();
        assert_eq!(s.swept(), Some(t0()));

        *now.lock() = t0() + TimeDelta::seconds(59);
        s.get(&key("other")).await.unwrap();
        assert_eq!(s.size(), 1, "a sweep ran inside SWEEP_INTERVAL");
        assert_eq!(s.swept(), Some(t0()));

        *now.lock() = t0() + TimeDelta::seconds(60);
        s.get(&key("other")).await.unwrap();
        assert_eq!(s.size(), 0, "no sweep at SWEEP_INTERVAL");
        assert_eq!(s.swept(), Some(t0() + TimeDelta::seconds(60)));
    }

    /// Go's zero `sweepEvery` is the sentinel for the default, not "always".
    #[tokio::test]
    async fn a_zero_sweep_interval_means_the_default() {
        let (now, clock) = manual_clock(t0());
        let s = InMemoryCredentialStore::with_time_provider(clock);
        s.set_sweep_every(Duration::ZERO);
        s.set(&key("gone"), bearer(), t0()).await.unwrap();

        *now.lock() = t0() + TimeDelta::seconds(1);
        s.delete(&key("other")).await.unwrap();
        assert_eq!(s.size(), 1, "a zero interval swept on the next call");
        assert_eq!(s.swept(), Some(t0()));
    }

    /// A clock stepping backwards gives a negative gap, which never reaches the interval.
    #[tokio::test]
    async fn a_clock_stepping_back_does_not_sweep() {
        let (now, clock) = manual_clock(t0());
        let s = InMemoryCredentialStore::with_time_provider(clock);
        s.set(&key("gone"), bearer(), t0()).await.unwrap();

        *now.lock() = t0() - TimeDelta::hours(1);
        s.get(&key("other")).await.unwrap();
        assert_eq!(s.size(), 1);
        assert_eq!(s.swept(), Some(t0()));
    }

    /// `expired` at the very end of `DateTime`'s range does not panic.
    #[test]
    fn expired_does_not_overflow() {
        assert!(expired(DateTime::<Utc>::MAX_UTC, DateTime::<Utc>::MAX_UTC));
        assert!(!expired(DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC));
    }

    /// Debug shows the entry count and nothing about keys or credentials.
    #[tokio::test]
    async fn debug_shows_only_the_entry_count() {
        let s = InMemoryCredentialStore::new();
        assert_eq!(
            format!("{s:?}"),
            "InMemoryCredentialStore { entries: 0, .. }"
        );
        let k = CredentialKey {
            app_name: "app-name-marker".into(),
            user_id: "user-id-marker".into(),
            key: "slot-marker".into(),
        };
        s.set(&k, bearer(), Utc::now() + TimeDelta::hours(1))
            .await
            .unwrap();
        let got = format!("{s:?} {s:#?}");
        assert!(got.contains("entries: 1"), "{got}");
        for secret in [
            "app-name-marker",
            "user-id-marker",
            "slot-marker",
            "tok-secret",
        ] {
            assert!(!got.contains(secret), "Debug {got:?} contains {secret:?}");
        }
    }
}
