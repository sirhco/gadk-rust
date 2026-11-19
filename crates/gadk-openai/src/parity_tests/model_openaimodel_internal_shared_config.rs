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

//! Port of ADK-Go `model/openaimodel/internal/shared/config_test.go`.

use std::time::Duration;

use gadk_genai::{GenerateContentConfig, ImageConfig, ThinkingConfig, ThinkingLevel};
use gadk_model::HttpOptions;
use pretty_assertions::assert_eq;

use crate::error::ErrorKind;
use crate::shared::config::{
    SUPPORTED_CONFIG_FIELDS, reject_unsupported_config_fields, reject_unsupported_fields,
    request_timeout, thinking_config_fields,
};

fn timeout_options(timeout: Duration) -> HttpOptions {
    HttpOptions {
        timeout: Some(timeout),
        ..HttpOptions::default()
    }
}

// parity: adapted: a `Duration` cannot be negative, so Go's `-time.Second` case has no counterpart; the zero case and the positive case are ported
#[test]
fn test_request_timeout_guards_non_positive_itself() {
    let zero = timeout_options(Duration::ZERO);
    assert_eq!(
        request_timeout(Some(&zero)),
        None,
        "RequestTimeout(0s): a non-positive bound must not become a deadline"
    );
    // The positive case still comes through, so the guard is not simply off.
    let positive = timeout_options(Duration::from_secs(1));
    assert_eq!(
        request_timeout(Some(&positive)),
        Some(Duration::from_secs(1)),
        "RequestTimeout(1s) must come through unchanged"
    );
}

/// Go's `knobs` test struct. `Later` and `Last` play fields genai adds after the supported
/// list was written; Go's unexported `internal` is never listed, as reflection skips it.
#[derive(Default)]
struct Knobs {
    supported: Option<i32>,
    later: Option<bool>,
    last: Vec<i32>,
}

impl Knobs {
    /// The field list `rejectUnsupportedFields` walks by reflection, written as
    /// `config_fields` writes it; a nil struct has none.
    fn fields(knobs: Option<&Knobs>) -> Vec<(&'static str, bool)> {
        let Some(Knobs {
            supported,
            later,
            last,
        }) = knobs
        else {
            return Vec::new();
        };
        vec![
            ("Supported", supported.is_some()),
            ("Later", later.is_some()),
            ("Last", !last.is_empty()),
        ]
    }
}

// parity: adapted: Go reflects over any struct; gadk's walk takes the `(field, set)` list exhaustive destructuring builds (`Knobs::fields`), so "untyped nil" and "nil pointer" are both a missing struct, "unexported" a field the list never names, and "struct value" the same struct by value
#[test]
fn test_reject_unsupported_fields_refuses_fields_added_later() {
    let tests: Vec<(&str, Option<Knobs>, &str)> = vec![
        ("untyped nil", None, ""),
        ("nil pointer", None, ""),
        ("nothing set", Some(Knobs::default()), ""),
        (
            "supported only",
            Some(Knobs {
                supported: Some(0),
                ..Knobs::default()
            }),
            "",
        ),
        ("unexported", Some(Knobs::default()), ""),
        (
            "added later",
            Some(Knobs {
                later: Some(false),
                ..Knobs::default()
            }),
            "Knobs.Later",
        ),
        (
            "first of two",
            Some(Knobs {
                later: Some(false),
                last: vec![1],
                ..Knobs::default()
            }),
            "Knobs.Later",
        ),
        (
            "struct value",
            Some(Knobs {
                last: vec![1],
                ..Knobs::default()
            }),
            "Knobs.Last",
        ),
    ];
    for (name, knobs, want) in tests {
        let result =
            reject_unsupported_fields("Knobs.", &Knobs::fields(knobs.as_ref()), &["Supported"]);
        if want.is_empty() {
            assert!(result.is_ok(), "{name}: error = {result:?}, want nil");
            continue;
        }
        let err = result.expect_err(name);
        let want = format!(
            "{}: {want}",
            ErrorKind::UnsupportedConfigField.sentinel_text()
        );
        assert!(err.is(ErrorKind::UnsupportedConfigField), "{name}: {err}");
        assert_eq!(err.to_string(), want, "{name}");
    }
}

// parity: adapted: Go narrows the package variable SupportedThinkingConfigFields; gadk's list is a const, so the narrowed walk runs through reject_unsupported_fields over thinking_config_fields (what reject_unsupported_config_fields does with the real list), and the top-level-first order through reject_unsupported_config_fields itself
#[test]
fn test_reject_unsupported_config_fields_walks_thinking_config() {
    let narrowed = ["include_thoughts", "thinking_budget"];
    let mut cfg = GenerateContentConfig {
        thinking_config: Some(ThinkingConfig {
            thinking_level: Some(ThinkingLevel::Low),
            ..ThinkingConfig::default()
        }),
        ..GenerateContentConfig::default()
    };
    let thinking = cfg.thinking_config.clone().unwrap_or_default();
    let err = reject_unsupported_fields(
        "thinking_config.",
        &thinking_config_fields(&thinking),
        &narrowed,
    )
    .expect_err("ThinkingLevel outside the narrowed list");
    assert_eq!(
        err.to_string(),
        format!(
            "{}: thinking_config.thinking_level",
            ErrorKind::UnsupportedConfigField.sentinel_text()
        )
    );
    // With the real list the same config passes the whole check.
    assert!(reject_unsupported_config_fields(&cfg, None, SUPPORTED_CONFIG_FIELDS).is_ok());

    // Every top-level field is reported before any ThinkingConfig field, even ImageConfig,
    // which genai declares after ThinkingConfig.
    cfg.image_config = Some(ImageConfig::default());
    let err = reject_unsupported_config_fields(&cfg, None, SUPPORTED_CONFIG_FIELDS)
        .expect_err("ImageConfig is unsupported");
    assert_eq!(
        err.to_string(),
        format!(
            "{}: image_config",
            ErrorKind::UnsupportedConfigField.sentinel_text()
        )
    );
}
