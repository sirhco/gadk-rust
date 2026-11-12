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

//! R17: `gadk::telemetry` (feature `telemetry`, not on by default) is `gadk_telemetry`; its
//! providers export an ADK span through the bridge layer.

use std::collections::HashMap;
use std::sync::Arc;

use gadk::platform::env::{EnvProvider, MapEnv};
use gadk::telemetry::{
    Error, OtelBridge, Providers, TelemetryOption, gcp_detector, new_with, with_span_processors,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SimpleSpanProcessor, SpanProcessor};
use pretty_assertions::assert_eq;
use tracing_subscriber::layer::SubscriberExt;

/// Compiles only if each façade path names the `gadk_telemetry` item.
#[test]
fn facade_telemetry_paths_are_gadk_telemetry() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Providers>(None, None::<&gadk_telemetry::Providers>);
    same::<TelemetryOption>(None, None::<&gadk_telemetry::TelemetryOption>);
    same::<Error>(None, None::<&gadk_telemetry::Error>);
    same::<OtelBridge>(None, None::<&gadk_telemetry::OtelBridge>);
}

/// Providers built with a span processor export an ADK span (`otel.name`) recorded under their
/// bridge layer; the environment is injected (no OTLP variables, no cloud export).
#[tokio::test]
async fn facade_telemetry_exports_an_adk_span() {
    let env: Arc<dyn EnvProvider> = Arc::new(MapEnv(HashMap::new()));
    let detector = gcp_detector::new_detector(Arc::clone(&env));
    let exporter = InMemorySpanExporter::default();
    let providers = new_with(
        env,
        &detector,
        &[with_span_processors([
            Box::new(SimpleSpanProcessor::new(exporter.clone())) as Box<dyn SpanProcessor>,
        ])],
    )
    .await
    .unwrap();
    assert!(providers.logger_provider.is_none());

    let subscriber = tracing_subscriber::registry().with(providers.layer());
    tracing::subscriber::with_default(subscriber, || {
        let _span = tracing::info_span!("adk", otel.name = "invoke_agent facade").entered();
    });

    let spans = exporter.get_finished_spans().unwrap();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
    assert_eq!(names, ["invoke_agent facade"]);
    providers.shutdown().await.unwrap();
}
