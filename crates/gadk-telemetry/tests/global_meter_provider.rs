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

//! The global halves of ADK-Go `telemetry/telemetry_test.go`'s `TestTelemetrySmoke` (record
//! through the global meter provider `SetGlobalOtelProviders` installs) and
//! `TestTelemetryNoReadersNoMeterProvider` (no meter provider leaves the global one in place).
//! The global meter provider is process-wide, so this is its own test binary with one test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use gadk_platform::env::MapEnv;
use gadk_telemetry::gcp_detector::{PartialResource, ResourceDetector};
use gadk_telemetry::{Providers, new_with, with_metric_reader};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{
    InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, Temporality,
};

/// A GCP detector off GCP.
struct NoDetector;

#[async_trait]
impl ResourceDetector for NoDetector {
    async fn detect(&self) -> Result<Option<Resource>, PartialResource> {
        Ok(None)
    }
}

/// A [`ManualReader`] the test keeps a handle to after registering it.
#[derive(Debug, Clone)]
struct Shared(Arc<ManualReader>);

impl Shared {
    fn new() -> Self {
        Self(Arc::new(ManualReader::builder().build()))
    }

    /// The names of the metrics a collection holds.
    fn metric_names(&self) -> Vec<String> {
        let mut rm = ResourceMetrics::default();
        self.0.collect(&mut rm).unwrap();
        rm.scope_metrics()
            .flat_map(|sm| sm.metrics())
            .map(|m| m.name().to_owned())
            .collect()
    }
}

impl MetricReader for Shared {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline);
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

/// `new_with` over an empty environment.
async fn providers(opts: &[gadk_telemetry::TelemetryOption]) -> Providers {
    new_with(Arc::new(MapEnv(HashMap::new())), &NoDetector, opts)
        .await
        .unwrap()
}

/// Records one count of `name` through OpenTelemetry's global meter provider.
fn record_globally(name: &'static str) {
    opentelemetry::global::meter("test-meter")
        .u64_counter(name)
        .build()
        .add(1, &[]);
}

#[tokio::test]
async fn set_global_otel_providers_installs_only_a_configured_meter_provider() {
    // TestTelemetryNoReadersNoMeterProvider: no provider is installed globally.
    let previous = Shared::new();
    let previous_provider = SdkMeterProvider::builder()
        .with_reader(previous.clone())
        .build();
    opentelemetry::global::set_meter_provider(previous_provider.clone());
    let none = providers(&[]).await;
    assert!(none.meter_provider.is_none());
    none.set_global_otel_providers();
    record_globally("kept-counter");
    assert_eq!(
        previous.metric_names(),
        ["kept-counter"],
        "want the previous provider kept"
    );

    // TestTelemetrySmoke: record through the global meter provider, as ADK instrumentation does.
    let reader = Shared::new();
    let configured = providers(&[with_metric_reader(reader.clone())]).await;
    configured.set_global_otel_providers();
    record_globally("test-counter");
    assert_eq!(reader.metric_names(), ["test-counter"]);
    assert_eq!(previous.metric_names(), ["kept-counter"]);

    configured.shutdown().await.unwrap();
    previous_provider.shutdown().unwrap();
}
