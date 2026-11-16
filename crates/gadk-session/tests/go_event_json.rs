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

//! R18: event JSON written by ADK-Go (via the Go probe) decodes into
//! a gadk-session `Event` and re-encodes to the same JSON value.

use chrono::{DateTime, Utc};
use gadk_session::Event;
use serde_json::Value;

/// R18 equality: JSON values equal; numbers as f64; RFC 3339 strings compared as instants.
fn normalize(v: &mut Value) {
    match v {
        Value::Object(m) => m.values_mut().for_each(normalize),
        Value::Array(a) => a.iter_mut().for_each(normalize),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                *v = serde_json::json!(f);
            }
        }
        Value::String(s) => {
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                *s = t
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            }
        }
        _ => {}
    }
}

#[test]
fn go_event_json() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go_events");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 10);
    for f in files {
        let go_json = std::fs::read_to_string(&f).unwrap();
        let ev: Event = serde_json::from_str(&go_json).unwrap();
        let mut rust: Value = serde_json::to_value(&ev).unwrap();
        let mut go: Value = serde_json::from_str(&go_json).unwrap();
        normalize(&mut rust);
        normalize(&mut go);
        pretty_assertions::assert_eq!(rust, go, "{}", f.display());
    }
}
