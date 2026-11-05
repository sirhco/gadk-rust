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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use chrono::{DateTime, TimeZone, Utc};
use gadk_platform::Platform;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn test_now_default_uses_wall_clock() {
    let before = Utc::now();
    let got = Platform::default().now();
    let after = Utc::now();
    assert!(got >= before && got <= after);
}

#[test]
fn test_with_time_provider_overrides_now() {
    let fixed = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let p = Platform::default().with_time(move || fixed);
    assert_eq!(p.now(), fixed);
}

#[test]
fn test_with_time_provider_derived_context() {
    let fixed = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let derived = Platform::default().with_time(move || fixed).clone();
    assert_eq!(derived.now(), fixed);
}

#[test]
fn test_with_time_provider_nested_override() {
    let a = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    let b = Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();
    let outer = Platform::default().with_time(move || a);
    let inner = outer.clone().with_time(move || b);
    assert_eq!(outer.now(), a);
    assert_eq!(inner.now(), b);
}

#[test]
fn test_with_time_provider_is_called_each_time() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let p = Platform::default().with_time(move || {
        let i = n2.fetch_add(1, Ordering::SeqCst) as i64;
        DateTime::from_timestamp(i, 0).unwrap()
    });
    p.now();
    p.now();
    assert_eq!(n.load(Ordering::SeqCst), 2);
}

#[test]
fn deterministic_sequence_reproducible() {
    let (a, b) = (Platform::deterministic(7), Platform::deterministic(7));
    let seq_a: Vec<_> = (0..3).map(|_| (a.now(), a.new_uuid())).collect();
    let seq_b: Vec<_> = (0..3).map(|_| (b.now(), b.new_uuid())).collect();
    assert_eq!(seq_a, seq_b);
    assert!(seq_a[1].0 > seq_a[0].0);
    assert_ne!(seq_a[0].1, seq_a[1].1);
    assert!(uuid::Uuid::parse_str(&seq_a[0].1).is_ok());
}
