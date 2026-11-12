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

//! gadk-telemetry: OpenTelemetry export for gadk (port of ADK-Go's `telemetry` package).
//!
//! [`new`] builds the OpenTelemetry SDK tracer and logger providers ([`Providers`]) from
//! [`TelemetryOption`]s and the environment, as Go's `telemetry.New`: OTLP/HTTP exporters when
//! `OTEL_EXPORTER_OTLP_*ENDPOINT` is set, export to `telemetry.googleapis.com` with
//! [`with_otel_to_cloud`], the resource from Go's default resource, the GCP project and the GCP
//! resource detector ([`gcp_detector`]). [`Providers::layer`] is the `tracing` layer
//! ([`OtelBridge`]) that exports gadk's ADK spans and log records through those providers.
//!
//! Every exporter sends through [`RuntimeHttpClient`], which runs each request on the Tokio
//! runtime that built the providers: shut the providers down ([`Providers::shutdown`]) before
//! that runtime ends.

mod bridge;
mod config;
mod credentials;
mod error;
pub mod gcp_detector;
mod http_client;
mod providers;
mod resource;
mod setup;

#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod test_support;

pub use bridge::OtelBridge;
pub use config::{
    GoogleCredentials, TelemetryOption, with_gcp_quota_project, with_gcp_resource_project,
    with_google_credentials, with_log_record_processors, with_logger_provider, with_otel_to_cloud,
    with_resource, with_span_processors, with_tracer_provider,
};
pub use error::Error;
pub use http_client::RuntimeHttpClient;
pub use providers::{Providers, new, new_with};
