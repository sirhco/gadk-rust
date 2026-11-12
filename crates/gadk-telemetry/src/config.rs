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

//! Telemetry options (port of ADK-Go `telemetry/config.go`): [`TelemetryOption`] values, applied
//! in order to the private [`Config`] — a later option overrides an earlier one and the processor
//! options accumulate, as Go's functional options do.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use google_cloud_auth::credentials::Credentials;
use opentelemetry::logs::Severity;
use opentelemetry::{Context, InstrumentationScope};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogProcessor, SdkLogRecord, SdkLoggerProvider};
use opentelemetry_sdk::trace::{SdkTracerProvider, Span, SpanData, SpanProcessor};
use parking_lot::RwLock;

/// Go `*google.Credentials`: a token source (`None` is allowed, as Go's tests build
/// `&google.Credentials{ProjectID: …}`) and the project ID found with it.
#[derive(Clone, Default)]
pub struct GoogleCredentials {
    /// The credentials that authenticate export requests (Go `TokenSource`); `None` sends
    /// requests without credential headers.
    pub credentials: Option<Credentials>,
    /// Go `ProjectID`: the credentials file's `project_id`, the metadata server's project on
    /// GCE, or empty.
    pub project_id: String,
}

impl fmt::Debug for GoogleCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleCredentials")
            .field("credentials", &self.credentials.as_ref().map(|_| "…"))
            .field("project_id", &self.project_id)
            .finish()
    }
}

/// One telemetry option (Go `telemetry.Option`), built by the `with_*` functions. Options are
/// values: cloning one shares its processors and providers, as Go shares the pointers.
#[derive(Clone, Debug)]
pub struct TelemetryOption(Opt);

#[derive(Clone, Debug)]
enum Opt {
    OtelToCloud(bool),
    GcpResourceProject(String),
    GcpQuotaProject(String),
    Resource(Resource),
    GoogleCredentials(GoogleCredentials),
    SpanProcessors(Vec<SharedSpanProcessor>),
    LogRecordProcessors(Vec<SharedLogProcessor>),
    TracerProvider(SdkTracerProvider),
    LoggerProvider(SdkLoggerProvider),
}

/// Go `WithOtelToCloud`: enables or disables exporting telemetry to GCP.
pub fn with_otel_to_cloud(value: bool) -> TelemetryOption {
    TelemetryOption(Opt::OtelToCloud(value))
}

/// Go `WithGcpResourceProject`: the `gcp.project_id` resource attribute.
pub fn with_gcp_resource_project(project: impl Into<String>) -> TelemetryOption {
    TelemetryOption(Opt::GcpResourceProject(project.into()))
}

/// Go `WithGcpQuotaProject`: the quota project for the telemetry export.
pub fn with_gcp_quota_project(project_id: impl Into<String>) -> TelemetryOption {
    TelemetryOption(Opt::GcpQuotaProject(project_id.into()))
}

/// Go `WithResource`: the OTel resource, merged last (its attributes win).
pub fn with_resource(resource: Resource) -> TelemetryOption {
    TelemetryOption(Opt::Resource(resource))
}

/// Go `WithGoogleCredentials`: overrides the Application Default Credentials.
pub fn with_google_credentials(credentials: GoogleCredentials) -> TelemetryOption {
    TelemetryOption(Opt::GoogleCredentials(credentials))
}

/// Go `WithSpanProcessors`: registers additional span processors (accumulates).
pub fn with_span_processors(
    processors: impl IntoIterator<Item = Box<dyn SpanProcessor>>,
) -> TelemetryOption {
    TelemetryOption(Opt::SpanProcessors(
        processors
            .into_iter()
            .map(SharedSpanProcessor::new)
            .collect(),
    ))
}

/// Go `WithLogRecordProcessors`: registers additional log processors (accumulates).
pub fn with_log_record_processors(
    processors: impl IntoIterator<Item = Box<dyn LogProcessor>>,
) -> TelemetryOption {
    TelemetryOption(Opt::LogRecordProcessors(
        processors
            .into_iter()
            .map(SharedLogProcessor::new)
            .collect(),
    ))
}

/// Go `WithTracerProvider`: a preconfigured tracer provider, returned as is by [`crate::new`].
pub fn with_tracer_provider(provider: SdkTracerProvider) -> TelemetryOption {
    TelemetryOption(Opt::TracerProvider(provider))
}

/// Go `WithLoggerProvider`: a preconfigured logger provider, returned as is by [`crate::new`].
pub fn with_logger_provider(provider: SdkLoggerProvider) -> TelemetryOption {
    TelemetryOption(Opt::LoggerProvider(provider))
}

/// Go's unexported `config`.
#[derive(Debug, Default)]
pub(crate) struct Config {
    pub(crate) otel_to_cloud: bool,
    pub(crate) gcp_resource_project: String,
    pub(crate) gcp_quota_project: String,
    pub(crate) google_credentials: Option<GoogleCredentials>,
    pub(crate) resource: Option<Resource>,
    pub(crate) span_processors: Vec<SharedSpanProcessor>,
    pub(crate) log_processors: Vec<SharedLogProcessor>,
    pub(crate) tracer_provider: Option<SdkTracerProvider>,
    pub(crate) logger_provider: Option<SdkLoggerProvider>,
}

/// Go `configFromOpts`. No gadk option can fail, so Go's `failed to apply option: %w` never
/// occurs.
pub(crate) fn config_from_opts(opts: &[TelemetryOption]) -> Config {
    let mut cfg = Config::default();
    for TelemetryOption(opt) in opts {
        match opt {
            Opt::OtelToCloud(v) => cfg.otel_to_cloud = *v,
            Opt::GcpResourceProject(v) => cfg.gcp_resource_project.clone_from(v),
            Opt::GcpQuotaProject(v) => cfg.gcp_quota_project.clone_from(v),
            Opt::Resource(v) => cfg.resource = Some(v.clone()),
            Opt::GoogleCredentials(v) => cfg.google_credentials = Some(v.clone()),
            Opt::SpanProcessors(v) => cfg.span_processors.extend(v.iter().cloned()),
            Opt::LogRecordProcessors(v) => cfg.log_processors.extend(v.iter().cloned()),
            Opt::TracerProvider(v) => cfg.tracer_provider = Some(v.clone()),
            Opt::LoggerProvider(v) => cfg.logger_provider = Some(v.clone()),
        }
    }
    cfg
}

/// A span processor shared by every clone of the option that registered it (Go shares the
/// processor pointer). The SDK needs `&mut` only for `set_resource`, at provider build.
#[derive(Clone, Debug)]
pub(crate) struct SharedSpanProcessor(Arc<RwLock<Box<dyn SpanProcessor>>>);

impl SharedSpanProcessor {
    pub(crate) fn new(processor: Box<dyn SpanProcessor>) -> Self {
        Self(Arc::new(RwLock::new(processor)))
    }

    /// Whether `self` and `other` are the same registered processor.
    #[cfg(test)]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl SpanProcessor for SharedSpanProcessor {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.0.read().on_start(span, cx);
    }

    fn on_end(&self, span: SpanData) {
        self.0.read().on_end(span);
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.read().force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.read().shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.0.read().shutdown()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.write().set_resource(resource);
    }
}

/// A log processor shared by every clone of the option that registered it.
#[derive(Clone, Debug)]
pub(crate) struct SharedLogProcessor(Arc<RwLock<Box<dyn LogProcessor>>>);

impl SharedLogProcessor {
    pub(crate) fn new(processor: Box<dyn LogProcessor>) -> Self {
        Self(Arc::new(RwLock::new(processor)))
    }

    /// Whether `self` and `other` are the same registered processor.
    #[cfg(test)]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl LogProcessor for SharedLogProcessor {
    fn emit(&self, data: &mut SdkLogRecord, instrumentation: &InstrumentationScope) {
        self.0.read().emit(data, instrumentation);
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.read().force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.read().shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.0.read().shutdown()
    }

    fn event_enabled(&self, level: Severity, target: &str, name: Option<&str>) -> bool {
        self.0.read().event_enabled(level, target, name)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.write().set_resource(resource);
    }
}
