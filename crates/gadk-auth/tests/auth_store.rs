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

//! Port of Go `auth/store_test.go` (package `auth_test`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use gadk_auth::{
    ApiKey, Bearer, Credential, CredentialKey, CredentialStore, EXPIRY_SKEW,
    InMemoryCredentialStore,
};

/// Go `time.Now().Add(left)`.
fn now_plus(left: TimeDelta) -> DateTime<Utc> {
    Utc::now() + left
}

/// A `time.Duration` constant (`auth.ExpirySkew`) as a `TimeDelta`.
fn td(d: Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap()
}

fn res_key() -> CredentialKey {
    CredentialKey {
        key: "res".into(),
        ..CredentialKey::default()
    }
}

fn bearer(token: &str) -> Arc<dyn Credential> {
    Arc::new(Bearer {
        token: token.into(),
    })
}

#[tokio::test]
async fn test_in_memory_credential_store_get_set() {
    let s = InMemoryCredentialStore::new();
    let key = CredentialKey {
        app_name: "app".into(),
        user_id: "user".into(),
        key: "res".into(),
    };
    let cred = bearer("tok");

    let got = s.get(&key).await;
    assert!(
        matches!(got, Ok(None)),
        "Get() on empty store = {got:?}, want Ok(None)"
    );
    if let Err(err) = s
        .set(&key, Arc::clone(&cred), now_plus(TimeDelta::hours(1)))
        .await
    {
        panic!("Set() error = {err}");
    }
    let got = s.get(&key).await;
    match &got {
        Ok(Some(c)) if Arc::ptr_eq(c, &cred) => {}
        _ => panic!("Get() after Set = {got:?}, want the stored credential"),
    }

    // A different key is a miss.
    let other = CredentialKey {
        app_name: "app".into(),
        user_id: "other".into(),
        key: "res".into(),
    };
    assert!(
        s.get(&other).await.unwrap().is_none(),
        "Get() for a different user = hit, want miss"
    );
}

#[tokio::test]
async fn test_in_memory_credential_store_expiry() {
    let s = InMemoryCredentialStore::new();
    let key = res_key();
    let cred: Arc<dyn Credential> = Arc::new(ApiKey {
        name: "X".into(),
        value: "v".into(),
    });

    s.set(&key, Arc::clone(&cred), now_plus(TimeDelta::hours(1)))
        .await
        .unwrap();
    assert!(
        s.get(&key).await.unwrap().is_some(),
        "Get() with far-future expiry = miss, want hit"
    );

    s.set(&key, Arc::clone(&cred), now_plus(-TimeDelta::seconds(1)))
        .await
        .unwrap();
    assert!(
        s.get(&key).await.unwrap().is_none(),
        "Get() with past expiry = hit, want miss"
    );
}

// Expiry is judged on the store's clock (the wall clock here), and a test drives it by choosing
// how much life to store rather than by moving a clock.
#[tokio::test]
async fn test_in_memory_credential_store_expiry_boundaries() {
    let tests = [
        ("an hour left", TimeDelta::hours(1), true),
        ("long past", -TimeDelta::hours(1), false),
        ("inside the skew window", td(EXPIRY_SKEW / 2), false),
    ];
    for (name, left, want_hit) in tests {
        let s = InMemoryCredentialStore::new();
        let key = res_key();
        s.set(&key, bearer("tok"), now_plus(left)).await.unwrap();
        let hit = s.get(&key).await.unwrap().is_some();
        assert_eq!(
            hit, want_hit,
            "{name}: Get() with {left} left = hit {hit}, want {want_hit}"
        );
    }
}

#[tokio::test]
async fn test_in_memory_credential_store_overwrite() {
    let s = InMemoryCredentialStore::new();
    let key = res_key();
    let first = bearer("a");
    let second = bearer("b");

    let expiry = now_plus(TimeDelta::hours(1));
    s.set(&key, Arc::clone(&first), expiry).await.unwrap();
    s.set(&key, Arc::clone(&second), expiry).await.unwrap();
    let got = s.get(&key).await.unwrap();
    match &got {
        Some(c) if Arc::ptr_eq(c, &second) => {}
        _ => panic!("Get() after overwrite = {got:?}, want the second credential"),
    }
}

// parity: skipped TestInMemoryCredentialStoreSetRequiresCredentialAndExpiry: set takes Arc<dyn Credential> and DateTime<Utc>, so neither a nil credential nor Go's zero time.Time{} can be passed (spec R6)

#[tokio::test]
async fn test_in_memory_credential_store_delete() {
    let s = InMemoryCredentialStore::new();
    let key = res_key();
    s.set(&key, bearer("t"), now_plus(TimeDelta::hours(1)))
        .await
        .unwrap();
    if let Err(err) = s.delete(&key).await {
        panic!("Delete() error = {err}");
    }
    assert!(
        s.get(&key).await.unwrap().is_none(),
        "Get() after Delete = hit, want miss"
    );
    if let Err(err) = s.delete(&key).await {
        panic!("Delete() of an absent key = {err}, want Ok");
    }
}

// The zero value is usable.
// parity: adapted: Go's zero value is InMemoryCredentialStore::default()
#[tokio::test]
async fn test_in_memory_credential_store_zero_value() {
    let s = InMemoryCredentialStore::default();
    let key = res_key();
    if let Err(err) = s
        .set(&key, bearer("t"), now_plus(TimeDelta::hours(1)))
        .await
    {
        panic!("Set() on a zero store error = {err}");
    }
    assert!(
        s.get(&key).await.unwrap().is_some(),
        "Get() after Set on a zero store = miss, want hit"
    );
}

// EXPIRY_SKEW is public API and the provider's caching floor. Its value is asserted against a
// literal rather than measured against itself.
#[test]
fn test_expiry_skew_value() {
    assert_eq!(
        EXPIRY_SKEW,
        Duration::from_secs(10),
        "auth.ExpirySkew = {EXPIRY_SKEW:?}, want 10s; changing it changes both what this store serves and what producers cache"
    );
}

// Which side of the margin is inclusive decides whether the store serves exactly what a producer
// will cache. The exact boundary is pinned by `test_expired_boundary` (internal); these cases carry
// enough slack that the read cannot cross a boundary on its own.
#[tokio::test]
async fn test_expiry_skew_boundary() {
    let tests = [
        (
            "well outside the margin",
            td(EXPIRY_SKEW) + TimeDelta::minutes(1),
            true,
        ),
        ("exactly the margin", td(EXPIRY_SKEW), false),
        ("well inside the margin", td(EXPIRY_SKEW / 2), false),
    ];
    for (name, left, want_hit) in tests {
        let s = InMemoryCredentialStore::new();
        let key = res_key();
        if let Err(err) = s.set(&key, bearer("t"), now_plus(left)).await {
            panic!("{name}: Set() error = {err}");
        }
        let hit = s.get(&key).await.unwrap().is_some();
        assert_eq!(
            hit, want_hit,
            "{name}: Get() with {left} left = hit {hit}, want {want_hit}"
        );
    }
}
