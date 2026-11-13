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

//! Crate-internal half of the R1 config golden (`tests/golden_config.rs` holds the public
//! half): ADK-Go `applyDefaults` and the unexported accessors (`logMultiModalContent`, …,
//! `viewLogf`) against `config/apply_defaults.json` and `config/default_config.json`, and the
//! config constants against `config/constants.json`. `Config::apply_defaults` and the
//! accessors are `pub(crate)`, so this file is compiled into the library's unit tests (declared
//! with `#[path]` from `src/schema.rs`), not as an integration test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::config::{
    ClientOptions, Config, DEFAULT_GCS_MAX_CONCURRENT_UPLOADS, DEFAULT_MAX_INLINE_TEXT_SIZE,
    DEFAULT_MAX_PENDING_UPLOAD_BYTES, DEFAULT_TABLE_NAME, DEFAULT_VIEW_PREFIX, GCS_UPLOAD_TIMEOUT,
    GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig, SCHEMA_VERSION, SCHEMA_VERSION_KEY,
    StdLogger,
};
use crate::context::Context;
use crate::error::Error;

#[path = "config_projection.rs"]
mod config_projection;

use config_projection::{build, normalize, project};

fn read_json(rel: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The probe's `derived` object: Go's unexported accessors of `c`.
fn derived(c: &Config) -> Value {
    json!({
        "createViews": c.create_views(),
        "flushOnRunEnd": c.flush_on_run_end(),
        "logMultiModalContent": c.log_multi_modal_content(),
        "logSessionMetadata": c.log_session_metadata(),
        "maxContentLength": c.max_content_length(),
        "redactSensitiveKeys": c.redact_sensitive_keys(),
        "viewLogfIsNil": c.view_logf().is_none(),
    })
}

#[test]
fn default_config_derived_matches_go() {
    let go = read_json("config/default_config.json");
    assert_eq!(derived(&Config::default()), go["derived"]);
}

#[test]
fn apply_defaults_matches_go() {
    let cases = read_json("config/apply_defaults.json")["cases"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let (input, unrepresentable) = build(&case["input"]);
        if name == "negatives" {
            // Divergence (ledger 7b-1): negative durations do not exist in Rust; Go keeps a
            // negative BatchFlushIntv / ShutdownTimeout and replaces a negative
            // GCSUploadTimeout, Rust only sees zero there. The negative integers are compared.
            assert_eq!(
                unrepresentable,
                ["BatchFlushIntv", "ShutdownTimeout", "GCSUploadTimeout"]
            );
        } else {
            assert!(unrepresentable.is_empty(), "{name}");
        }
        assert_eq!(
            derived(&input),
            case["input_derived"],
            "{name} input_derived"
        );
        let output = input.apply_defaults();
        assert_eq!(
            normalize(&project(&output), &unrepresentable),
            normalize(&case["output"], &unrepresentable),
            "{name} output"
        );
        assert_eq!(
            derived(&output),
            case["output_derived"],
            "{name} output_derived"
        );
    }
}

#[test]
fn config_constants_match_go() {
    let go = read_json("config/constants.json");
    assert_eq!(go["SchemaVersion"], SCHEMA_VERSION);
    assert_eq!(go["schemaVersionKey"], SCHEMA_VERSION_KEY);
    assert_eq!(go["defaultTableName"], DEFAULT_TABLE_NAME);
    assert_eq!(go["defaultViewPrefix"], DEFAULT_VIEW_PREFIX);
    assert_eq!(go["defaultMaxInlineTextSize"], DEFAULT_MAX_INLINE_TEXT_SIZE);
    assert_eq!(
        go["defaultGCSMaxConcurrentUploads"],
        DEFAULT_GCS_MAX_CONCURRENT_UPLOADS
    );
    assert_eq!(
        go["defaultMaxPendingUploadBytes"],
        DEFAULT_MAX_PENDING_UPLOAD_BYTES
    );
    assert_eq!(
        go["gcsUploadTimeout_ns"],
        u64::try_from(GCS_UPLOAD_TIMEOUT.as_nanos()).unwrap()
    );
}
