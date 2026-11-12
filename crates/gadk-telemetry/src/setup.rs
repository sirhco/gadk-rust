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

//! Telemetry setup (port of ADK-Go `telemetry/setup_otel.go`): credentials, projects, the
//! resource and the exporters, then the providers. Error texts are Go's.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_platform::env::EnvProvider;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;

use crate::config::{
    Config, GoogleCredentials, SharedLogProcessor, SharedSpanProcessor, TelemetryOption,
};
use crate::error::Error;
use crate::gcp_detector::ResourceDetector;

/// Go `newGcpSpanExporter`'s endpoint.
pub(crate) const GCP_TRACES_ENDPOINT: &str = "https://telemetry.googleapis.com/v1/traces";

/// Go `configure`: ADC when cloud export is on and no credentials were given, the quota and
/// resource projects, the resource, then the exporters' processors appended to the configured
/// ones. `env` is the injected environment and `detector` the GCP resource detector.
pub(crate) async fn configure(
    env: &Arc<dyn EnvProvider>,
    detector: &dyn ResourceDetector,
    opts: &[TelemetryOption],
) -> Result<Config, Error> {
    todo!()
}

/// Go `resolveGcpQuotaProject`.
pub(crate) fn resolve_gcp_quota_project(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<String, String> {
    todo!()
}

/// Go `resolveGcpResourceProject`.
pub(crate) fn resolve_gcp_resource_project(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<String, String> {
    todo!()
}

/// Go `resolveProject`: the trimmed configured project, else the credentials' project ID, else
/// the trimmed `GOOGLE_CLOUD_PROJECT`; empty is an error only when `require_project`.
pub(crate) fn resolve_project(
    configured_project: &str,
    creds: Option<&GoogleCredentials>,
    require_project: bool,
    project_type: &str,
    env: &dyn EnvProvider,
) -> Result<String, String> {
    todo!()
}

/// Go `resolveResource`: Go's default resource ← `gcp.project_id` and, with cloud export on,
/// the GCP detector's resource ← the configured resource.
pub(crate) async fn resolve_resource(
    cfg: &Config,
    env: &dyn EnvProvider,
    detector: &dyn ResourceDetector,
) -> Result<Resource, String> {
    todo!()
}

/// Go `configureExporters`: batch processors for the OTLP/HTTP exporters the environment asks
/// for and, with cloud export on, the GCP span exporter. Exporters send through a
/// [`crate::RuntimeHttpClient`] on the current Tokio runtime.
pub(crate) fn configure_exporters(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<(Vec<SharedSpanProcessor>, Vec<SharedLogProcessor>), String> {
    todo!()
}

/// Go `newGcpSpanExporter`: OTLP/HTTP protobuf to `endpoint` ([`GCP_TRACES_ENDPOINT`] in
/// production) with the credentials' headers and `x-goog-user-project: <quota project>`.
pub(crate) fn new_gcp_span_exporter(
    cfg: &Config,
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, String> {
    todo!()
}

/// Go `initTracerProvider`: the configured provider, else `None` without span processors, else
/// a provider with the resource and the processors.
pub(crate) fn init_tracer_provider(cfg: &Config) -> Option<SdkTracerProvider> {
    todo!()
}

/// Go `initLoggerProvider`.
pub(crate) fn init_logger_provider(cfg: &Config) -> Option<SdkLoggerProvider> {
    todo!()
}
