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

//! Port of Go `auth/store_internal_test.go` (package `auth`).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use pretty_assertions::assert_eq;

use crate::credential::{Bearer, Credential};
use crate::store::{CredentialKey, CredentialStore, EXPIRY_SKEW, InMemoryCredentialStore, expired};

fn key(user: &str) -> CredentialKey {
    CredentialKey {
        user_id: user.into(),
        key: "res".into(),
        ..CredentialKey::default()
    }
}

fn bearer() -> Arc<dyn Credential> {
    Arc::new(Bearer { token: "t".into() })
}

/// Go `time.Now().Add(left)`.
fn now_plus(left: TimeDelta) -> DateTime<Utc> {
    Utc::now() + left
}

// Expired entries for principals that never come back must not accumulate: nothing but the sweep
// evicts them, and a read of some other key is enough to trigger one.
#[tokio::test]
async fn test_in_memory_credential_store_sweeps_expired() {
    let s = InMemoryCredentialStore::new();
    let cred = bearer();
    for i in 0..300 {
        let k = key(&format!("gone-{i}"));
        if let Err(err) = s
            .set(&k, Arc::clone(&cred), now_plus(-TimeDelta::hours(1)))
            .await
        {
            panic!("Set() error = {err}");
        }
    }
    let got = s.size();
    assert_eq!(
        got, 300,
        "store holds {got} entries before any sweep is due, want 300"
    );

    s.arm();
    if let Err(err) = s.get(&key("someone-else")).await {
        panic!("Get() error = {err}");
    }
    let got = s.size();
    assert_eq!(
        got, 0,
        "store holds {got} entries, want the expired ones swept"
    );
}

// The sweep is paced: it costs O(entries), so it must not run on every call.
#[tokio::test]
async fn test_in_memory_credential_store_sweep_is_paced() {
    let s = InMemoryCredentialStore::new();
    if let Err(err) = s
        .set(&key("u"), bearer(), now_plus(-TimeDelta::hours(1)))
        .await
    {
        panic!("Set() error = {err}");
    }

    // Expired, but the next sweep is a minute away, so a Get for some other key must leave the
    // entry alone.
    let swept = s.swept();
    if let Err(err) = s.get(&key("other")).await {
        panic!("Get() error = {err}");
    }
    assert_eq!(
        s.swept(),
        swept,
        "a sweep ran twice inside one sweepInterval"
    );
    let got = s.size();
    assert_eq!(
        got, 1,
        "store holds {got} entries, want the expired one still resident until a sweep is due"
    );
}

// The sweep decides the fate of every entry in the store, so it must run on the store's own
// clock: one request arriving with a clock pinned to next year must not empty a store that other
// call trees share.
// parity: adapted: store methods take no context, so no caller clock can reach them; the test arms a sweep and checks live entries survive it on the store's own clock
#[tokio::test]
async fn test_in_memory_credential_store_sweep_ignores_the_callers_clock() {
    let s = InMemoryCredentialStore::new();
    for i in 0..3 {
        let k = key(&format!("live-{i}"));
        if let Err(err) = s.set(&k, bearer(), now_plus(TimeDelta::hours(1))).await {
            panic!("Set() error = {err}");
        }
    }

    s.arm();
    let swept = s.swept();
    // Go passes a context whose platform clock is five years ahead. A Rust caller's clock lives in
    // its own `gadk_platform::Platform`, and `get` has no parameter that could carry it, so the
    // call below is the only shape a caller can make.
    if let Err(err) = s.get(&key("someone-else")).await {
        panic!("Get() error = {err}");
    }
    // Without a sweep the size check below passes for the wrong reason.
    assert_ne!(
        s.swept(),
        swept,
        "the armed Get did not sweep, so this test proves nothing"
    );
    let got = s.size();
    assert_eq!(
        got, 3,
        "store holds {got} entries, want all 3 — one caller's clock must not expire another's credentials"
    );
}

// The store is documented safe for concurrent use, and `get` writes to the map on eviction while
// the sweep deletes from it. The pacing is shortened so the sweep body runs while readers are in
// flight, but not to the point of sweeping on every call, which would remove every expired entry
// before a get could reach one. A zero interval is the sentinel for the one-minute default.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_in_memory_credential_store_concurrent() {
    let s = Arc::new(InMemoryCredentialStore::new());
    s.set_sweep_every(Duration::from_micros(200));

    let key_for = |i: usize| CredentialKey {
        app_name: "app".into(),
        user_id: format!("user-{}", i % 4),
        key: "res".into(),
    };
    let mut tasks = Vec::new();
    for g in 0..8usize {
        let s = Arc::clone(&s);
        tasks.push(tokio::spawn(async move {
            for i in 0..200usize {
                let k = key_for(g + i);
                match (g + i) % 3 {
                    0 => {
                        // Every other write is already spent, so eviction by get and the
                        // sweep's deletes run while other tasks read.
                        let expires = if (g + i) % 2 == 0 {
                            now_plus(-TimeDelta::hours(1))
                        } else {
                            now_plus(TimeDelta::hours(1))
                        };
                        if let Err(err) = s.set(&k, bearer(), expires).await {
                            panic!("Set() error = {err}");
                        }
                    }
                    1 => {
                        if let Err(err) = s.get(&k).await {
                            panic!("Get() error = {err}");
                        }
                    }
                    _ => {
                        if let Err(err) = s.delete(&k).await {
                            panic!("Delete() error = {err}");
                        }
                    }
                }
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
}

// parity: skipped TestInMemoryCredentialStoreSetStripsMonotonic: DateTime<Utc> carries no monotonic reading, so there is nothing to strip and nothing left to assert

// Delete sweeps too. A caller whose only traffic after a burst is invalidation would otherwise
// never run one, and the type's doc says any call does.
#[tokio::test]
async fn test_in_memory_credential_store_delete_sweeps() {
    let s = InMemoryCredentialStore::new();
    let stale = key("gone");
    if let Err(err) = s
        .set(&stale, bearer(), now_plus(-TimeDelta::hours(1)))
        .await
    {
        panic!("Set() error = {err}");
    }
    let got = s.size();
    assert_eq!(
        got, 1,
        "store holds {got} entries, want the expired one still resident until a sweep is due"
    );

    s.arm();
    if let Err(err) = s.delete(&key("unrelated")).await {
        panic!("Delete() error = {err}");
    }
    let got = s.size();
    assert_eq!(
        got, 0,
        "store holds {got} entries after an armed Delete, want the expired one swept"
    );
}

// Get evicts the expired entry it was asked for, rather than only reporting a miss. That write is
// the reason the store takes a full Mutex instead of a read-write lock.
#[tokio::test]
async fn test_in_memory_credential_store_get_evicts() {
    let s = InMemoryCredentialStore::new();
    let k = key("u");
    if let Err(err) = s.set(&k, bearer(), now_plus(-TimeDelta::hours(1))).await {
        panic!("Set() error = {err}");
    }
    let got = s.size();
    assert_eq!(
        got, 1,
        "store holds {got} entries, want the expired one resident until something removes it"
    );

    // No sweep is due, so a miss here can only come from eviction by get itself.
    assert!(
        s.get(&k).await.unwrap().is_none(),
        "Get() of an expired entry = hit, want miss"
    );
    assert_eq!(
        s.size(),
        0,
        "Get() reported a miss without evicting the expired entry"
    );
}

// expired's boundary is the exact complement of the floor a producer applies before caching. It
// is pinned here rather than through get because a wall clock cannot hold still.
#[test]
fn test_expired_boundary() {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let skew = TimeDelta::from_std(EXPIRY_SKEW).unwrap();
    let ns = TimeDelta::nanoseconds(1);
    let tests = [
        ("a nanosecond beyond the margin", now + skew + ns, false),
        ("exactly the margin", now + skew, true),
        ("a nanosecond inside the margin", now + skew - ns, true),
        ("already past", now - ns, true),
    ];
    for (name, expires_at, want) in tests {
        let got = expired(now, expires_at);
        assert_eq!(
            got,
            want,
            "{name}: expired(now, now{:+}ns) = {got}, want {want}",
            (expires_at - now).num_nanoseconds().unwrap()
        );
    }
}
