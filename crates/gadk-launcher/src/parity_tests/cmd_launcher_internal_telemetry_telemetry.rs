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

//! Port of ADK-Go `cmd/launcher/internal/telemetry/telemetry_test.go`.
//!
//! Go's `t.Setenv` is the injected environment of [`Globals::injected_with`]; neither the global
//! tracer provider nor the global dispatcher is touched.

use std::sync::Arc;

use gadk_telemetry::{TelemetryOption, with_otel_to_cloud};
use pretty_assertions::assert_eq;

use crate::config::Config;
use crate::telemetry::{Globals, init_with};
use crate::test_support::NoAgents;

/// Go `withoutCloudCredentials`: export to GCP fails as soon as it is turned on, without network
/// access, because the credentials file the environment names does not exist. The error then
/// tells whether export was turned on.
struct WithoutCloudCredentials {
    _dir: tempfile::TempDir,
    missing: String,
}

impl WithoutCloudCredentials {
    fn new() -> WithoutCloudCredentials {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir
            .path()
            .join("missing.json")
            .to_string_lossy()
            .into_owned();
        WithoutCloudCredentials { _dir: dir, missing }
    }

    fn globals(&self) -> Globals {
        Globals::injected_with(
            &[
                ("GOOGLE_APPLICATION_CREDENTIALS", &self.missing),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", ""),
                ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", ""),
            ],
            |_| {},
            |_| true,
        )
    }
}

/// Go `initProviders`: `true` when initialization failed with a credentials error (export to GCP
/// was turned on).
async fn init_providers(
    env: &WithoutCloudCredentials,
    config: &mut Config,
    otel_to_cloud: bool,
) -> bool {
    match init_with(config, otel_to_cloud, env.globals()).await {
        Ok(providers) => {
            if let Err(e) = providers.shutdown().await {
                panic!("Shutdown() error = {e}");
            }
            false
        }
        Err(e) => {
            assert!(
                e.to_string().contains("credentials"),
                "InitAndSetGlobalOtelProviders() error = {e}, want nil or a credentials error"
            );
            true
        }
    }
}

#[tokio::test]
async fn test_init_and_set_global_otel_providers_otel_to_cloud() {
    let env = WithoutCloudCredentials::new();
    struct Case {
        name: &'static str,
        options: Vec<TelemetryOption>,
        flag: bool,
        want_cloud_export: bool,
    }
    let cases = [
        Case {
            name: "no flag, no option",
            options: Vec::new(),
            flag: false,
            want_cloud_export: false,
        },
        Case {
            name: "flag",
            options: Vec::new(),
            flag: true,
            want_cloud_export: true,
        },
        Case {
            name: "option without flag",
            options: vec![with_otel_to_cloud(true)],
            flag: false,
            want_cloud_export: true,
        },
        Case {
            name: "flag overrides disabling option",
            options: vec![with_otel_to_cloud(false)],
            flag: true,
            want_cloud_export: true,
        },
        Case {
            name: "disabling option without flag",
            options: vec![with_otel_to_cloud(false)],
            flag: false,
            want_cloud_export: false,
        },
    ];
    for tc in cases {
        let mut config = Config::new(Arc::new(NoAgents));
        config.telemetry_options = tc.options;
        assert_eq!(
            init_providers(&env, &mut config, tc.flag).await,
            tc.want_cloud_export,
            "{}: export to GCP turned on",
            tc.name
        );
    }
}

// parity: adapted: a Rust Vec cannot share its spare capacity with a copy, so the embedder's list keeping its one option is checked instead of Go's backing-array slot
#[tokio::test]
async fn test_init_and_set_global_otel_providers_keeps_telemetry_options() {
    let env = WithoutCloudCredentials::new();
    // Spare capacity, as Go's test gives its slice.
    let mut options = Vec::with_capacity(2);
    options.push(with_otel_to_cloud(false));
    let mut config = Config::new(Arc::new(NoAgents));
    config.telemetry_options = options;

    init_providers(&env, &mut config, true).await;

    assert_eq!(
        config.telemetry_options.len(),
        1,
        "the launcher wrote into the embedder's TelemetryOptions"
    );
}
