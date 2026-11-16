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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use google_cloud_auth::credentials::Credentials;
use opentelemetry::logs::Severity;
use opentelemetry::{Context, InstrumentationScope};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogProcessor, SdkLogRecord, SdkLoggerProvider};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{InstrumentKind, Pipeline, SdkMeterProvider, Temporality};
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
    MetricReader(SharedMetricReader),
    TracerProvider(SdkTracerProvider),
    LoggerProvider(SdkLoggerProvider),
    MeterProvider(SdkMeterProvider),
    CloudLogName(String),
    CloudLoggingStdout,
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

/// Go `WithMetricReaders`: registers an additional metric reader (accumulates). Go's variadic
/// option is one option per reader here.
pub fn with_metric_reader<R: MetricReader + 'static>(reader: R) -> TelemetryOption {
    TelemetryOption(Opt::MetricReader(SharedMetricReader::new(reader)))
}

/// Go `WithTracerProvider`: a preconfigured tracer provider, returned as is by [`crate::new`].
pub fn with_tracer_provider(provider: SdkTracerProvider) -> TelemetryOption {
    TelemetryOption(Opt::TracerProvider(provider))
}

/// Go `WithLoggerProvider`: a preconfigured logger provider, returned as is by [`crate::new`].
pub fn with_logger_provider(provider: SdkLoggerProvider) -> TelemetryOption {
    TelemetryOption(Opt::LoggerProvider(provider))
}

/// Go `WithMeterProvider`: a preconfigured meter provider, returned as is by [`crate::new`].
pub fn with_meter_provider(provider: SdkMeterProvider) -> TelemetryOption {
    TelemetryOption(Opt::MeterProvider(provider))
}

/// The Cloud Logging log ID that log records are written to with cloud export on (default
/// `gadk`).
pub fn with_cloud_log_name(name: impl Into<String>) -> TelemetryOption {
    TelemetryOption(Opt::CloudLogName(name.into()))
}

/// With cloud export on, writes log records to stdout as structured JSON lines (for platforms
/// whose logging agent collects stdout) instead of calling the Cloud Logging API. The env var
/// `GADK_TELEMETRY_LOGS_STDOUT` (`1` or `true`) does the same.
pub fn with_cloud_logging_stdout() -> TelemetryOption {
    TelemetryOption(Opt::CloudLoggingStdout)
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
    pub(crate) metric_readers: Vec<SharedMetricReader>,
    pub(crate) tracer_provider: Option<SdkTracerProvider>,
    pub(crate) logger_provider: Option<SdkLoggerProvider>,
    pub(crate) meter_provider: Option<SdkMeterProvider>,
    pub(crate) cloud_log_name: String,
    pub(crate) cloud_logging_stdout: bool,
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
            Opt::MetricReader(v) => cfg.metric_readers.push(v.clone()),
            Opt::TracerProvider(v) => cfg.tracer_provider = Some(v.clone()),
            Opt::LoggerProvider(v) => cfg.logger_provider = Some(v.clone()),
            Opt::MeterProvider(v) => cfg.meter_provider = Some(v.clone()),
            Opt::CloudLogName(v) => cfg.cloud_log_name.clone_from(v),
            Opt::CloudLoggingStdout => cfg.cloud_logging_stdout = true,
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

/// A metric reader shared by every clone of the option that registered it (Go shares the reader
/// pointer). Every [`MetricReader`] method takes `&self`, so it delegates through the `Arc`.
#[derive(Clone, Debug)]
pub(crate) struct SharedMetricReader(Arc<dyn MetricReader>);

impl SharedMetricReader {
    pub(crate) fn new(reader: impl MetricReader + 'static) -> Self {
        Self(Arc::new(reader))
    }

    /// Whether `self` and `other` are the same registered reader.
    #[cfg(test)]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl MetricReader for SharedMetricReader {
    fn register_pipeline(&self, pipeline: std::sync::Weak<Pipeline>) {
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

    fn shutdown(&self) -> OTelSdkResult {
        self.0.shutdown()
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.0.temporality(kind)
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::KeyValue;
    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SimpleLogProcessor};
    use opentelemetry_sdk::metrics::ManualReader;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SimpleSpanProcessor};
    use pretty_assertions::assert_eq;

    use super::*;

    fn span_processor() -> Box<dyn SpanProcessor> {
        Box::new(SimpleSpanProcessor::new(InMemorySpanExporter::default()))
    }

    fn log_processor() -> Box<dyn LogProcessor> {
        Box::new(SimpleLogProcessor::new(InMemoryLogExporter::default()))
    }

    fn resource(name: &'static str) -> Resource {
        Resource::builder_empty()
            .with_attribute(KeyValue::new("service.name", name))
            .build()
    }

    fn credentials(project_id: &str) -> GoogleCredentials {
        GoogleCredentials {
            credentials: None,
            project_id: project_id.to_owned(),
        }
    }

    #[test]
    fn no_options_is_the_zero_config() {
        let cfg = config_from_opts(&[]);
        assert!(!cfg.otel_to_cloud);
        assert_eq!(cfg.gcp_resource_project, "");
        assert_eq!(cfg.gcp_quota_project, "");
        assert!(cfg.google_credentials.is_none());
        assert!(cfg.resource.is_none());
        assert!(cfg.span_processors.is_empty());
        assert!(cfg.log_processors.is_empty());
        assert!(cfg.metric_readers.is_empty());
        assert!(cfg.tracer_provider.is_none());
        assert!(cfg.logger_provider.is_none());
        assert!(cfg.meter_provider.is_none());
    }

    #[test]
    fn later_options_override_earlier_ones() {
        let first = InMemorySpanExporter::default();
        let second = InMemorySpanExporter::default();
        let provider = |exporter: &InMemorySpanExporter| {
            SdkTracerProvider::builder()
                .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
                .build()
        };
        let cfg = config_from_opts(&[
            with_otel_to_cloud(true),
            with_gcp_resource_project("resource-1"),
            with_gcp_quota_project("quota-1"),
            with_google_credentials(credentials("creds-1")),
            with_resource(resource("svc-1")),
            with_tracer_provider(provider(&first)),
            with_otel_to_cloud(false),
            with_gcp_resource_project("resource-2"),
            with_gcp_quota_project("quota-2"),
            with_google_credentials(credentials("creds-2")),
            with_resource(resource("svc-2")),
            with_tracer_provider(provider(&second)),
        ]);
        assert!(!cfg.otel_to_cloud);
        assert_eq!(cfg.gcp_resource_project, "resource-2");
        assert_eq!(cfg.gcp_quota_project, "quota-2");
        assert_eq!(cfg.google_credentials.unwrap().project_id, "creds-2");
        assert_eq!(cfg.resource, Some(resource("svc-2")));
        let tp = cfg.tracer_provider.unwrap();
        tp.tracer("t").in_span("s", |_| {});
        assert_eq!(first.get_finished_spans().unwrap().len(), 0);
        assert_eq!(second.get_finished_spans().unwrap().len(), 1);
    }

    #[test]
    fn later_logger_provider_overrides_an_earlier_one() {
        let first = SdkLoggerProvider::builder().build();
        let second = SdkLoggerProvider::builder().build();
        let cfg = config_from_opts(&[
            with_logger_provider(first.clone()),
            with_logger_provider(second.clone()),
        ]);
        let got = cfg.logger_provider.unwrap();
        // Shutting the chosen provider down shuts `second` down (they share their state).
        got.shutdown().unwrap();
        assert!(second.shutdown().is_err());
        assert!(first.shutdown().is_ok());
    }

    #[test]
    fn processor_options_accumulate() {
        let cfg = config_from_opts(&[
            with_span_processors([span_processor(), span_processor()]),
            with_log_record_processors([log_processor()]),
            with_otel_to_cloud(true),
            with_span_processors([span_processor()]),
            with_log_record_processors([log_processor(), log_processor()]),
            with_metric_reader(ManualReader::builder().build()),
            with_metric_reader(ManualReader::builder().build()),
        ]);
        assert_eq!(cfg.span_processors.len(), 3);
        assert_eq!(cfg.log_processors.len(), 3);
        assert_eq!(cfg.metric_readers.len(), 2);
    }

    #[test]
    fn later_meter_provider_overrides_an_earlier_one() {
        let first = SdkMeterProvider::builder().build();
        let second = SdkMeterProvider::builder().build();
        let cfg = config_from_opts(&[
            with_meter_provider(first.clone()),
            with_meter_provider(second.clone()),
        ]);
        let got = cfg.meter_provider.unwrap();
        // Shutting the chosen provider down shuts `second` down (they share their state).
        got.shutdown().unwrap();
        assert!(second.shutdown().is_err());
        assert!(first.shutdown().is_ok());
    }

    #[test]
    fn cloned_options_share_their_processors() {
        let spans = with_span_processors([span_processor()]);
        let logs = with_log_record_processors([log_processor()]);
        let readers = with_metric_reader(ManualReader::builder().build());
        let a = config_from_opts(&[spans.clone(), logs.clone(), readers.clone()]);
        let b = config_from_opts(&[spans, logs, readers]);
        assert!(a.span_processors[0].ptr_eq(&b.span_processors[0]));
        assert!(a.log_processors[0].ptr_eq(&b.log_processors[0]));
        assert!(a.metric_readers[0].ptr_eq(&b.metric_readers[0]));
    }
}
