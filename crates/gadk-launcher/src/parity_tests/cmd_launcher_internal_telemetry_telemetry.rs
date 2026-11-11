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
//! Go's cases decide whether GCP export turns on from the `-otel_to_cloud` flag and a
//! `telemetry.WithOtelToCloud` option. gadk's telemetry options are `tracing` layers, none of
//! which turns export on, and export itself is sub-project 7: the flag is an error, everything else
//! installs the layers. The global dispatcher is never touched (the installation is injected).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use tracing_subscriber::{Layer, Registry};

use crate::config::{BoxedLayer, Config};
use crate::error::Error;
use crate::telemetry::init_with;

struct NoAgents;

impl gadk_agent::Loader for NoAgents {
    fn list_agents(&self) -> Vec<String> {
        Vec::new()
    }
    fn load_agent(
        &self,
        name: &str,
    ) -> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::LoaderError> {
        Err(gadk_agent::LoaderError(format!("no agent {name}")))
    }
    fn root_agent(&self) -> Arc<dyn gadk_agent::Agent> {
        unimplemented!("the telemetry tests load no agent")
    }
}

/// Stands in for a `telemetry.Option`: a layer that counts the events it sees.
struct CountingOption(Arc<AtomicUsize>);

impl Layer<Registry> for CountingOption {
    fn on_event(
        &self,
        _event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, Registry>,
    ) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn option() -> BoxedLayer {
    Box::new(CountingOption(Arc::new(AtomicUsize::new(0))))
}

/// Go `initProviders`: `true` when initialization reports that cloud export was asked for (Go:
/// a credentials error from the GCP exporter; gadk: the sub-project 7 error).
fn init_providers(config: &mut Config, otel_to_cloud: bool) -> bool {
    match init_with(config, otel_to_cloud, |_| true) {
        Ok(providers) => {
            providers.shutdown().unwrap();
            false
        }
        Err(Error::Telemetry(text)) => {
            assert_eq!(
                text,
                "-otel_to_cloud requires OpenTelemetry export, which gadk adds in sub-project 7"
            );
            true
        }
        Err(e) => panic!("init() error = {e}, want nil or the -otel_to_cloud error"),
    }
}

// parity: adapted: telemetry options are tracing layers (none turns export on); -otel_to_cloud is the sub-project 7 error
#[test]
fn test_init_and_set_global_otel_providers_otel_to_cloud() {
    struct Case {
        name: &'static str,
        options: usize,
        flag: bool,
        want_cloud_export: bool,
    }
    let cases = [
        Case {
            name: "no flag, no option",
            options: 0,
            flag: false,
            want_cloud_export: false,
        },
        Case {
            name: "flag",
            options: 0,
            flag: true,
            want_cloud_export: true,
        },
        Case {
            name: "option without flag",
            options: 1,
            flag: false,
            want_cloud_export: false,
        },
        Case {
            name: "flag with an option",
            options: 1,
            flag: true,
            want_cloud_export: true,
        },
        Case {
            name: "two options without flag",
            options: 2,
            flag: false,
            want_cloud_export: false,
        },
    ];
    for tc in cases {
        let mut config = Config::new(Arc::new(NoAgents));
        config.telemetry_layers = (0..tc.options).map(|_| option()).collect();
        assert_eq!(
            init_providers(&mut config, tc.flag),
            tc.want_cloud_export,
            "{}: export to GCP turned on",
            tc.name
        );
    }
}

// parity: adapted: the launcher takes the layers only when it installs them; a failed init leaves the embedder's list as it was
#[test]
fn test_init_and_set_global_otel_providers_keeps_telemetry_options() {
    let mut config = Config::new(Arc::new(NoAgents));
    config.telemetry_layers = vec![option()];
    init_providers(&mut config, true);
    assert_eq!(
        config.telemetry_layers.len(),
        1,
        "the launcher changed the embedder's TelemetryOptions"
    );
}
