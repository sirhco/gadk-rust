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
/// (`cache_until`) is its complement. Exactly [`EXPIRY_SKEW`] left counts as spent. A `now` so close to the end of
/// `DateTime`'s range that adding the skew overflows leaves nothing later, so it is expired.
pub(crate) fn expired(now: DateTime<Utc>, expires_at: DateTime<Utc>) -> bool {
    let skew = TimeDelta::from_std(EXPIRY_SKEW).unwrap_or(TimeDelta::MAX);
    match now.checked_add_signed(skew) {
        Some(limit) => expires_at <= limit,
        None => true,
    }
}

#[cfg(test)]
#[allow(dead_code, unused_variables, clippy::todo)]
impl InMemoryCredentialStore {
    /// store_internal_test.go:182 `arm`.
    pub(crate) fn arm(&self) {
        todo!("Task 3")
    }

    pub(crate) fn size(&self) -> usize {
        todo!("Task 3")
    }

    /// Go `lastSweep` (`None` = zero).
    pub(crate) fn swept(&self) -> Option<DateTime<Utc>> {
        todo!("Task 3")
    }

    pub(crate) fn set_sweep_every(&self, every: Duration) {
        todo!("Task 3")
    }
}
