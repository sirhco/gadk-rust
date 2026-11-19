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

//! Port of ADK-Go `cmd/launcher/launcher_validate_test.go`.

use std::sync::Arc;

use gadk_launcher::Config;
use gadk_session::compaction;

mod support;

use support::NoAgents;

fn config(compaction: Option<compaction::Config>) -> Config {
    let mut config = Config::new(Arc::new(NoAgents));
    config.compaction = compaction;
    config
}

/// A launcher refuses to start on a compaction config that cannot work.
#[test]
fn test_config_validate_rejects_unusable_compaction() {
    let cases: [(&str, Option<compaction::Config>, bool); 5] = [
        ("nil is fine", None, true),
        (
            "valid sliding window",
            Some(compaction::Config {
                compaction_interval: 2,
                ..Default::default()
            }),
            true,
        ),
        (
            "overlap with no interval",
            Some(compaction::Config {
                overlap_size: 2,
                ..Default::default()
            }),
            false,
        ),
        (
            "threshold with no retention",
            Some(compaction::Config {
                token_threshold: 100,
                ..Default::default()
            }),
            false,
        ),
        (
            "no strategy at all",
            Some(compaction::Config::default()),
            false,
        ),
    ];
    for (name, cfg, ok) in cases {
        let err = config(cfg).validate();
        if ok {
            assert!(err.is_ok(), "{name}: validate() = {err:?}, want Ok");
            continue;
        }
        let err = err.expect_err(name);
        assert!(
            err.to_string().contains("compaction"),
            "{name}: error {err:?} does not name the field"
        );
        assert!(
            err.to_string().starts_with("invalid compaction config: "),
            "{name}: error {err} does not start with `invalid compaction config: `"
        );
    }
}

/// The entry point a program reaches (`full::new().execute`) rejects the config before parsing
/// any argument.
#[cfg(feature = "full")]
#[tokio::test]
async fn test_full_launcher_refuses_unusable_compaction() {
    use gadk_launcher::{CancellationToken, Launcher};

    let cfg = config(Some(compaction::Config {
        overlap_size: 2,
        ..Default::default()
    }));
    let err = gadk_launcher::full::new()
        .execute(CancellationToken::new(), cfg, vec!["console".to_owned()])
        .await
        .expect_err("execute() started on a compaction config that cannot work");
    assert!(
        err.to_string().contains("compaction"),
        "error {err} does not name the field an operator has to change"
    );
}
