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

//! Tests for the platform UUID source.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_platform::Platform;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn test_new_uuid_default_is_random_and_valid() {
    let p = Platform::default();
    let first = p.new_uuid();
    let second = p.new_uuid();
    for id in [&first, &second] {
        let parsed = uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(parsed.get_version_num(), 4, "{id} is not a v4 UUID");
    }
    assert_ne!(first, second);
}

#[test]
fn test_with_uuid_provider_overrides_new_uuid() {
    let n = Arc::new(AtomicUsize::new(0));
    let p = Platform::default().with_uuid(move || {
        let i = n.fetch_add(1, Ordering::SeqCst) + 1;
        format!("id-{i}")
    });
    for w in ["id-1", "id-2", "id-3"] {
        assert_eq!(p.new_uuid(), w);
    }
}

#[test]
fn test_with_uuid_provider_derived_context() {
    let derived = Platform::default()
        .with_uuid(|| "fixed".to_string())
        .clone();
    assert_eq!(derived.new_uuid(), "fixed");
}

#[test]
fn test_with_uuid_provider_nested_override() {
    let outer = Platform::default().with_uuid(|| "outer".to_string());
    let inner = outer.clone().with_uuid(|| "inner".to_string());
    assert_eq!(inner.new_uuid(), "inner");
    assert_eq!(outer.new_uuid(), "outer");
}
