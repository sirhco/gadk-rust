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

//! Golden test of `Config::default()` (ADK-Go `plugin/agentanalytics/config.go`
//! `DefaultConfig()`, spec R1) against the probe's `config/default_config.json` projection.
//! `apply_defaults` and the derived accessors are crate-internal; their goldens over
//! `config/apply_defaults.json` are the `schema::golden_apply_defaults` unit tests
//! (`tests/support/golden_apply_defaults.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use gadk_agentanalytics::{
    ClientOptions, Config, Context, Error, GcsContentOffloader, GcsUploadRequest, Logger,
    RetryConfig, StdLogger,
};
use pretty_assertions::assert_eq;
use serde_json::Value;

#[path = "support/config_projection.rs"]
mod config_projection;

use config_projection::{build, normalize, project};

fn read_json(rel: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn default_config_projection_matches_go() {
    let go = read_json("config/default_config.json");
    assert_eq!(
        project(&Config::default()),
        normalize(&go["projection"], &[])
    );
}

#[test]
fn zero_config_projection_matches_go_zero_value() {
    let zero = read_json("config/apply_defaults.json")["cases"][0].clone();
    assert_eq!(zero["name"], "zero");
    assert_eq!(project(&Config::zero()), normalize(&zero["input"], &[]));
}

/// The projection builder is faithful: every probed input and output projection survives
/// build → project, except the negative durations Rust cannot hold.
#[test]
fn config_projections_round_trip() {
    let cases = read_json("config/apply_defaults.json")["cases"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        for side in ["input", "output"] {
            let (config, unrepresentable) = build(&case[side]);
            assert_eq!(
                normalize(&project(&config), &unrepresentable),
                normalize(&case[side], &unrepresentable),
                "{name} {side}"
            );
        }
    }
}
