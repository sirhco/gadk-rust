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

//! The R8 golden: ADK-Go `validateConfig` against `config/validate_config.json`, for each probed
//! config as given and after `applyDefaults`. `validate_config` is `pub(crate)`, so this file
//! is compiled into the library's unit tests (declared with `#[path]` from `src/validate.rs`),
//! not as an integration test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

// The names `config_projection` expects in scope of the including module.
use crate::config::{
    ClientOptions, Config, GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig, StdLogger,
};
use crate::context::Context;
use crate::error::Error;
use crate::validate::validate_config;

// The projection helper is also compiled into `config::golden_apply_defaults`, whose module is
// private to `config`; a second copy keeps 7b-1's files untouched.
#[allow(clippy::duplicate_mod)]
#[path = "config_projection.rs"]
mod config_projection;

fn outcome(r: Result<BTreeSet<String>, Error>) -> Value {
    match r {
        Ok(denied) => json!({"denied": denied, "error": null}),
        Err(e) => json!({"denied": null, "error": e.to_string()}),
    }
}

/// A `RetryConfig` duration of the projection is negative (Rust cannot hold it).
fn negative_retry_duration(input: &Value) -> bool {
    ["InitialDelay", "MaxDelay"]
        .iter()
        .any(|k| input["RetryConfig"][k].as_i64().unwrap() < 0)
}

#[test]
fn validate_config_matches_go() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config/validate_config.json");
    let go: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases = go["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 35);
    let mut skipped = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        if negative_retry_duration(&case["input"]) {
            // 7b-1's ledgered divergence: a negative `time.Duration` has no Rust form.
            skipped.push(name);
            continue;
        }
        let (config, unrepresentable): (Config, Vec<&str>) =
            config_projection::build(&case["input"]);
        assert_eq!(
            outcome(validate_config(&config)),
            case["result"],
            "{name} result"
        );
        let with_defaults = outcome(validate_config(&config.apply_defaults()));
        if unrepresentable.is_empty() {
            assert_eq!(with_defaults, case["with_defaults"], "{name} with_defaults");
        } else {
            // A negative ShutdownTimeout reads as zero in Rust: rejected as Go rejects the
            // negative value, but `apply_defaults` then replaces the zero (Go keeps the
            // negative value and rejects it again) — 7b-1's ledgered divergence.
            assert_eq!(name, "shutdown_timeout_negative");
            assert_eq!(unrepresentable, ["ShutdownTimeout"]);
            assert_eq!(with_defaults, json!({"denied": [], "error": null}));
        }
    }
    assert_eq!(skipped, ["retry_initial_delay_negative"]);
}

#[test]
fn every_check_runs_on_a_valid_default_config() {
    let denied = validate_config(&Config {
        payload_column_denylist: vec!["latency_ms".into(), "content".into()],
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        denied,
        BTreeSet::from(["content".to_owned(), "latency_ms".to_owned()])
    );
    let r = RetryConfig {
        multiplier: f64::NAN,
        ..Config::default().retry_config
    };
    // Go's `NaN < 1` is false: a NaN multiplier passes, as in Go.
    assert!(
        validate_config(&Config {
            retry_config: r,
            ..Config::default()
        })
        .is_ok()
    );
}
