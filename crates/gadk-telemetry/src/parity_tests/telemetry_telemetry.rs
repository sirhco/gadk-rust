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

//! Port of ADK-Go `telemetry/telemetry_test.go`.
//!
//! The environment (`GOOGLE_CLOUD_PROJECT`, `OTEL_EXPORTER_OTLP_*`) is injected instead of set
//! with `t.Setenv`, the GCP resource detector is a fake that detects nothing (Go's runs off GCP),
//! and the global OpenTelemetry providers are never set: `tests/global_meter_provider.rs`, its
//! own test binary, checks the global meter provider.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{Span as _, SpanKind, Tracer as _, TracerProvider as _};
use opentelemetry::{Key, KeyValue};
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{
    InMemoryLogExporter, LogProcessor, SdkLoggerProvider, SimpleLogProcessor,
};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader as _;
use opentelemetry_sdk::metrics::{ManualReader, PeriodicReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, SdkTracerProvider, SimpleSpanProcessor, SpanProcessor,
};
use pretty_assertions::assert_eq;
use prost::Message as _;
use regex::Regex;

use crate::config::{
    GoogleCredentials, SharedMetricReader, TelemetryOption, config_from_opts, with_cloud_log_name,
    with_gcp_quota_project, with_gcp_resource_project, with_google_credentials,
    with_log_record_processors, with_logger_provider, with_meter_provider, with_metric_reader,
    with_otel_to_cloud, with_resource, with_span_processors, with_tracer_provider,
};
use crate::metric_reader::{PERIODIC_READERS_BUILT, new_periodic_reader};
use crate::providers::{new_internal, new_with};
use crate::resource::{go_default_resource, new_with_attributes};
use crate::setup::{
    GCP_METRICS_ENDPOINT, GCP_TRACES_ENDPOINT, GcpEndpoints, configure, configure_exporters,
    configure_to, resolve_gcp_quota_project, resolve_gcp_resource_project, resolve_resource,
};
use crate::test_support::{NoDetector, ResourceSpanExporter, Sink, env, fake_credentials};

const RESOURCE_PROJECT: &str = "resource-project";
const QUOTA_PROJECT: &str = "quota-project";

fn span_processor(exporter: &InMemorySpanExporter) -> Box<dyn SpanProcessor> {
    Box::new(SimpleSpanProcessor::new(exporter.clone()))
}

fn log_processor(exporter: &InMemoryLogExporter) -> Box<dyn LogProcessor> {
    Box::new(SimpleLogProcessor::new(exporter.clone()))
}

fn body(exporter: &InMemoryLogExporter, i: usize) -> Option<AnyValue> {
    exporter.get_emitted_logs().unwrap()[i]
        .record
        .body()
        .cloned()
}

/// Go `extractResourceAttributes`.
fn extract_resource_attributes(res: &Resource) -> (String, String, String) {
    let get = |k: &'static str| {
        res.get(&Key::from_static_str(k))
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    (
        get("gcp.project_id"),
        get("service.name"),
        get("service.version"),
    )
}

// parity: adapted: Go's global otel.Tracer and otel.Meter are the returned providers' tracer and meter and SetGlobalOtelProviders is injected (tests never set global state; tests/global_meter_provider.rs records through the global meter); Collect after Shutdown is opentelemetry_sdk's error, not ErrReaderShutdown
#[tokio::test]
async fn test_telemetry_smoke() {
    let exporter = ResourceSpanExporter::default();
    let log_exporter = InMemoryLogExporter::default();
    let reader = SharedMetricReader::new(ManualReader::builder().build());

    // Initialize telemetry.
    let service_name = "test-service";
    let service_version = "1.2.3";
    let r = Resource::builder_empty()
        .with_attributes([
            KeyValue::new("service.name", service_name),
            KeyValue::new("service.version", service_version),
        ])
        .build();
    let providers = new_with(
        env(&[]),
        &NoDetector,
        &[
            with_span_processors([
                Box::new(SimpleSpanProcessor::new(exporter.clone())) as Box<dyn SpanProcessor>
            ]),
            with_log_record_processors([log_processor(&log_exporter)]),
            with_metric_reader(reader.clone()),
            with_gcp_resource_project(RESOURCE_PROJECT),
            with_gcp_quota_project(QUOTA_PROJECT),
            with_resource(r),
        ],
    )
    .await
    .unwrap();
    providers.set_global_otel_providers_with(|_| {});
    let tp = providers.tracer_provider.clone().unwrap();
    let lp = providers.logger_provider.clone().unwrap();
    let mp = providers.meter_provider.clone().unwrap();

    // Create test tracer.
    let tracer = tp.tracer("test-tracer");
    let span_name = "test-span";
    let mut span = tracer
        .span_builder(span_name)
        .with_kind(SpanKind::Server)
        .start(&tracer);
    span.end();

    // Create test logger and log.
    let logger = lp.logger("test-logger");
    let log_body = "test-log";
    let mut record = logger.create_log_record();
    record.set_body(AnyValue::from(log_body));
    logger.emit(record);

    // Record through the meter provider, as ADK instrumentation does.
    let counter = mp.meter("test-meter").u64_counter("test-counter").build();
    counter.add(1, &[]);

    tp.force_flush().unwrap();
    lp.force_flush().unwrap();

    // Check the reader sees the metric, with the configured resource.
    let mut rm = ResourceMetrics::default();
    reader.collect(&mut rm).unwrap();
    assert_eq!(rm.scope_metrics().count(), 1);
    let (got_resource_project, _, _) = extract_resource_attributes(rm.resource());
    assert_eq!(got_resource_project, RESOURCE_PROJECT, "metrics");

    // Check exporter contains the span.
    let spans = exporter.spans.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, span_name);
    let (got_resource_project, got_service_name, got_service_version) =
        extract_resource_attributes(&exporter.resource().unwrap());
    assert_eq!(got_resource_project, RESOURCE_PROJECT);
    assert_eq!(got_service_name, service_name);
    assert_eq!(got_service_version, service_version);

    // Check exporter contains the log.
    assert_eq!(log_exporter.get_emitted_logs().unwrap().len(), 1);
    assert_eq!(body(&log_exporter, 0), Some(AnyValue::from(log_body)));

    providers.shutdown().await.unwrap();
    assert_eq!(exporter.spans.get_finished_spans().unwrap().len(), 0);
    assert!(reader.collect(&mut rm).is_err(), "Collect() after Shutdown");

    // Go's t.Cleanup shuts down again.
    providers.shutdown().await.unwrap();
}

// parity: adapted: Go's global otel.Tracer is the returned provider's tracer and SetGlobalOtelProviders is injected (tests never set global state)
#[tokio::test]
async fn test_telemetry_custom_provider() {
    let exporter = InMemorySpanExporter::default();
    let tp = SdkTracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
        .build();
    let unused_exporter = InMemorySpanExporter::default();

    // Initialize telemetry with custom provider.
    let providers = new_with(
        env(&[]),
        &NoDetector,
        &[
            with_tracer_provider(tp),
            with_span_processors([span_processor(&unused_exporter)]),
        ],
    )
    .await
    .unwrap();
    providers.set_global_otel_providers_with(|_| {});

    // Create test tracer and span.
    let tp = providers.tracer_provider.clone().unwrap();
    let tracer = tp.tracer("test-tracer");
    let span_name = "test-span";
    tracer.start(span_name).end();

    tp.force_flush().unwrap();

    // Verify span was exported.
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, span_name);

    // Unused exporter should not have any spans.
    assert_eq!(unused_exporter.get_finished_spans().unwrap().len(), 0);

    providers.shutdown().await.unwrap();
}

// parity: adapted: SetGlobalOtelProviders is injected (tests never set global state); OpenTelemetry Rust has no global logger provider
#[tokio::test]
async fn test_telemetry_custom_logger_provider() {
    let log_exporter = InMemoryLogExporter::default();
    let lp = SdkLoggerProvider::builder()
        .with_log_processor(SimpleLogProcessor::new(log_exporter.clone()))
        .build();
    let unused_log_exporter = InMemoryLogExporter::default();

    // Initialize telemetry with custom logger provider.
    let providers = new_with(
        env(&[]),
        &NoDetector,
        &[
            with_logger_provider(lp),
            with_log_record_processors([log_processor(&unused_log_exporter)]),
        ],
    )
    .await
    .unwrap();
    providers.set_global_otel_providers_with(|_| {});

    // Create test logger and emit.
    let lp = providers.logger_provider.clone().unwrap();
    let logger = lp.logger("test-logger");
    let log_body = "test-log";
    let mut record = logger.create_log_record();
    record.set_body(AnyValue::from(log_body));
    logger.emit(record);

    lp.force_flush().unwrap();

    // Verify log was exported.
    assert_eq!(log_exporter.get_emitted_logs().unwrap().len(), 1);
    assert_eq!(body(&log_exporter, 0), Some(AnyValue::from(log_body)));

    // Unused exporter should not have any logs.
    assert_eq!(unused_log_exporter.get_emitted_logs().unwrap().len(), 0);

    providers.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_resolve_resource_service_instance_id() {
    let custom = new_with_attributes(
        "",
        [KeyValue::new("service.instance.id", "custom-instance")],
    );

    struct Case {
        name: &'static str,
        opts: Vec<TelemetryOption>,
        // want is None when the resource must have no service.instance.id.
        want: Option<Regex>,
    }
    let cases = [
        Case {
            // adk-python's `<uuid4 hex>-<pid>` form.
            name: "generated per process",
            opts: vec![with_otel_to_cloud(true)],
            want: Some(Regex::new(&format!("^[0-9a-f]{{32}}-{}$", std::process::id())).unwrap()),
        },
        Case {
            name: "overridden by the config resource",
            opts: vec![with_otel_to_cloud(true), with_resource(custom)],
            want: Some(Regex::new("^custom-instance$").unwrap()),
        },
        Case {
            name: "not set without export to Google Cloud",
            opts: vec![],
            want: None,
        },
    ];
    for tc in cases {
        let cfg = config_from_opts(&tc.opts);
        let r = resolve_resource(&cfg, env(&[]).as_ref(), &NoDetector)
            .await
            .unwrap_or_else(|e| panic!("{}: resolveResource() unexpected error: {e}", tc.name));
        let got = r.get(&Key::from_static_str("service.instance.id"));
        let Some(want) = tc.want else {
            assert_eq!(got, None, "{}: want no 'service.instance.id'", tc.name);
            continue;
        };
        let got = got
            .unwrap_or_else(|| panic!("{}: resource has no 'service.instance.id'", tc.name))
            .to_string();
        assert!(
            want.is_match(&got),
            "{}: got 'service.instance.id' {got:?}, want a match for {want}",
            tc.name
        );
    }
}

// parity: adapted: SdkMeterProvider has no pointer identity; the returned provider is shown to be the one passed by recording through it into a reader of the one passed
#[tokio::test]
async fn test_telemetry_custom_meter_provider() {
    let reader = SharedMetricReader::new(ManualReader::builder().build());
    let mp = SdkMeterProvider::builder()
        .with_reader(reader.clone())
        .build();
    let unused = SharedMetricReader::new(ManualReader::builder().build());
    let providers = new_with(
        env(&[]),
        &NoDetector,
        &[with_meter_provider(mp), with_metric_reader(unused.clone())],
    )
    .await
    .unwrap();

    let got = providers.meter_provider.clone().unwrap();
    got.meter("test-meter")
        .u64_counter("test-counter")
        .build()
        .add(1, &[]);
    let mut rm = ResourceMetrics::default();
    reader.collect(&mut rm).unwrap();
    assert_eq!(
        rm.scope_metrics().count(),
        1,
        "got MeterProvider other than the one passed to with_meter_provider"
    );
    // The reader option is ignored: it is registered with no provider.
    assert!(unused.collect(&mut ResourceMetrics::default()).is_err());

    // Go's t.Cleanup.
    providers.shutdown().await.unwrap();
}

/// Go `recordingMetricExporter`: counts calls; the exported data is irrelevant.
#[derive(Clone, Default)]
struct RecordingMetricExporter {
    exports: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    shutdown_err: Option<&'static str>,
}

impl RecordingMetricExporter {
    fn counts(&self) -> (usize, usize) {
        (
            self.exports.load(Ordering::SeqCst),
            self.shutdowns.load(Ordering::SeqCst),
        )
    }
}

impl PushMetricExporter for RecordingMetricExporter {
    async fn export(&self, _metrics: &ResourceMetrics) -> OTelSdkResult {
        self.exports.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        match self.shutdown_err {
            Some(e) => Err(OTelSdkError::InternalFailure(e.to_owned())),
            None => Ok(()),
        }
    }

    // Go `DefaultTemporalitySelector`.
    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}

// parity: adapted: opentelemetry_sdk's PeriodicReader reports an exporter's shutdown error as `Failed to shutdown` (the exporter's text is dropped), so the error is matched by that text and the exporter's shutdown count instead of errors.Is; a ManualReader shut down twice returns Ok in opentelemetry_sdk
#[tokio::test]
async fn test_telemetry_shutdown_returns_reader_errors() {
    let failing = |exporter: &RecordingMetricExporter| {
        SharedMetricReader::new(
            PeriodicReader::builder(exporter.clone())
                .with_interval(Duration::from_secs(3600))
                .build(),
        )
    };
    let already_shut_down = || {
        let r = ManualReader::builder().build();
        r.shutdown().unwrap();
        SharedMetricReader::new(r)
    };
    let cases: [(&str, bool); 2] = [
        ("alone", false),
        // The SDK joins every reader's error, so this one comes back together with the
        // already shut down reader's.
        ("with a reader already shut down", true),
    ];
    for (name, with_shut_down) in cases {
        let exporter = RecordingMetricExporter {
            shutdown_err: Some("exporter shutdown failed"),
            ..RecordingMetricExporter::default()
        };
        let mut opts = vec![with_metric_reader(failing(&exporter))];
        if with_shut_down {
            opts.push(with_metric_reader(already_shut_down()));
        }
        let providers = new_with(env(&[]), &NoDetector, &opts)
            .await
            .unwrap_or_else(|e| panic!("{name}: failed to create telemetry: {e}"));
        let err = providers.shutdown().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Operation failed: [InternalFailure(\"Failed to shutdown\")]",
            "{name}"
        );
        assert_eq!(exporter.counts().1, 1, "{name}: exporter shutdowns");
    }
}

// parity: adapted: opentelemetry-otlp has no OTEL_EXPORTER_OTLP_LOGS_CERTIFICATE, so the log exporter fails on an invalid OTEL_EXPORTER_OTLP_LOGS_ENDPOINT (`::`), after the metric exporter for OTEL_EXPORTER_OTLP_ENDPOINT was built; an orphaned opentelemetry_sdk reader reports nothing, so the readers new_periodic_reader built on this thread are counted instead of ErrReaderNotRegistered ticks
#[tokio::test]
async fn test_new_error_leaves_no_reader_running() {
    PERIODIC_READERS_BUILT.with(|n| n.set(0));
    let err = new_with(
        env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318"),
            ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", "::"),
        ]),
        &NoDetector,
        &[],
    )
    .await
    .expect_err("New() succeeded, want the log exporter's error");
    assert!(
        err.to_string().starts_with(
            "failed to configure exporters: failed to create OTLP HTTP log exporter: "
        ),
        "{err}"
    );
    assert_eq!(
        PERIODIC_READERS_BUILT.with(std::cell::Cell::get),
        0,
        "metric readers New left running"
    );

    // The count sees a reader setup builds.
    let providers = new_with(
        env(&[(
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            "http://localhost:4318/v1/metrics",
        )]),
        &NoDetector,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(PERIODIC_READERS_BUILT.with(std::cell::Cell::get), 1);
    providers.shutdown().await.unwrap();
}

// parity: adapted: the global half (no meter provider installed when there is none) is in tests/global_meter_provider.rs, its own test binary, because the global meter provider is process-wide
#[tokio::test]
async fn test_telemetry_no_readers_no_meter_provider() {
    let providers = new_with(
        env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", ""),
            ("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", ""),
        ]),
        &NoDetector,
        &[],
    )
    .await
    .unwrap();
    assert!(
        providers.meter_provider.is_none(),
        "got a MeterProvider, want None without readers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_otlp_env_metric_shutdown_does_not_export() {
    let sink = Sink::start().await;
    let providers = new_with(
        env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &sink.url),
            ("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", ""),
        ]),
        &NoDetector,
        &[],
    )
    .await
    .unwrap();
    let mp = providers
        .meter_provider
        .clone()
        .expect("MeterProvider is None, want one for OTEL_EXPORTER_OTLP_ENDPOINT");
    mp.meter("test-meter")
        .u64_counter("test-counter")
        .build()
        .add(1, &[]);
    providers.shutdown().await.unwrap();
    assert_eq!(
        sink.requests().len(),
        0,
        "export requests on Shutdown: {:?}",
        sink.requests()
    );
}

/// Go `readerInterval`: a periodic reader's interval, read off its logging view (`Debug`), the
/// only place gadk exposes it.
fn reader_interval(r: &SharedMetricReader) -> String {
    let view = format!("{r:?}");
    let start = view
        .find("interval: ")
        .unwrap_or_else(|| panic!("the Debug view of {view} has no interval"))
        + "interval: ".len();
    let rest = &view[start..];
    let end = rest.find([',', ' ']).unwrap_or(rest.len());
    rest[..end].to_owned()
}

// parity: adapted: the GCP exporters send to a local OTLP sink (configure_to's endpoints) instead of Go's recording transport, and the production endpoints are checked as constants; the reader's interval is read off its Debug view (Go: MarshalLog by reflection); Go wants exactly 1 metrics request, but the reader runs on a real 5 s interval, so a slow run may see a periodic export too: at least 1 before Shutdown, and Shutdown adds none
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_gcp_exporters() {
    assert_eq!(
        GCP_TRACES_ENDPOINT,
        "https://telemetry.googleapis.com/v1/traces"
    );
    assert_eq!(
        GCP_METRICS_ENDPOINT,
        "https://telemetry.googleapis.com/v1/metrics"
    );
    let sink = Sink::start().await;
    let traces = format!("{}/v1/traces", sink.url);
    let metrics = format!("{}/v1/metrics", sink.url);
    let logs = format!("{}/v2/entries:write", sink.url);
    let creds = GoogleCredentials {
        credentials: Some(fake_credentials(&[("authorization", "Bearer test-token")])),
        project_id: RESOURCE_PROJECT.to_owned(),
    };
    let env = env(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", ""),
        ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", ""),
        ("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", ""),
        ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", ""),
    ]);

    let cfg = configure_to(
        &env,
        &NoDetector,
        &[
            with_otel_to_cloud(true),
            with_google_credentials(creds),
            with_gcp_quota_project(QUOTA_PROJECT),
            with_cloud_log_name("a/b"),
        ],
        GcpEndpoints {
            traces: &traces,
            metrics: &metrics,
            logs: &logs,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("configure() unexpected error: {e}"));
    assert_eq!(cfg.metric_readers.len(), 1, "metric readers");
    // The literal, not the constant: Cloud Monitoring's floor is what the test pins.
    assert_eq!(
        reader_interval(&cfg.metric_readers[0]),
        "Some(5s)",
        "metric export interval"
    );

    let providers = new_internal(&cfg);
    let tp = providers.tracer_provider.clone().unwrap();
    let mp = providers.meter_provider.clone().unwrap();
    let lp = providers.logger_provider.clone().unwrap();
    tp.tracer("test-tracer").start("test-span").end();
    mp.meter("test-meter")
        .u64_counter("test-counter")
        .build()
        .add(1, &[]);
    let logger = lp.logger("test-logger");
    let mut record = logger.create_log_record();
    record.set_body(AnyValue::from("test-log"));
    logger.emit(record);
    // The SDK's flushes block until their export, which runs on this runtime's other worker.
    tokio::task::spawn_blocking(move || {
        tp.force_flush().expect("failed to flush spans");
        mp.force_flush().expect("failed to flush metrics");
        lp.force_flush().expect("failed to flush logs");
    })
    .await
    .unwrap();

    // Requests per endpoint; every GCP exporter's client sends the credentials and quota
    // project headers, so they are checked on each.
    let requests = |path: &str| -> Vec<_> {
        sink.requests()
            .into_iter()
            .filter(|c| c.path == path)
            .collect()
    };
    for path in ["/v1/traces", "/v1/metrics", "/v2/entries:write"] {
        let got = requests(path);
        // A periodic metrics export may follow the flush's on a slow run (5 s interval).
        if path == "/v1/metrics" {
            assert!(!got.is_empty(), "{path}: requests {:?}", sink.requests());
        } else {
            assert_eq!(got.len(), 1, "{path}: requests {:?}", sink.requests());
        }
        for c in &got {
            assert_eq!(
                c.headers["authorization"], "Bearer test-token",
                "{path}: Authorization header does not carry the credentials' token"
            );
            assert_eq!(c.headers["x-goog-user-project"], QUOTA_PROJECT, "{path}");
        }
    }
    // The configured log name reaches the request (spec R2).
    let log_body: serde_json::Value =
        serde_json::from_slice(&requests("/v2/entries:write")[0].body).unwrap();
    assert_eq!(
        log_body["logName"],
        serde_json::json!(format!("projects/{RESOURCE_PROJECT}/logs/a%2Fb"))
    );
    assert_eq!(
        log_body["entries"][0]["jsonPayload"]["body"],
        serde_json::json!("test-log")
    );
    // The fake collector decodes every metrics export.
    for c in requests("/v1/metrics") {
        let export = ExportMetricsServiceRequest::decode(c.body.as_slice()).unwrap();
        let names: Vec<_> = export
            .resource_metrics
            .iter()
            .flat_map(|rm| &rm.scope_metrics)
            .flat_map(|sm| &sm.metrics)
            .map(|m| m.name.clone())
            .collect();
        assert_eq!(names, ["test-counter"]);
    }

    // Shutdown exports no further metrics.
    let before = requests("/v1/metrics").len();
    providers.shutdown().await.unwrap();
    assert_eq!(
        requests("/v1/metrics").len(),
        before,
        "metric requests after Shutdown, want still {before}"
    );

    // Go's t.Cleanup.
    providers.shutdown().await.unwrap();
}

#[test]
fn test_periodic_reader() {
    let exporter = RecordingMetricExporter::default();
    let mp = SdkMeterProvider::builder()
        .with_reader(new_periodic_reader(
            exporter.clone(),
            Some(Duration::from_secs(3600)),
        ))
        .build();
    let counter = mp.meter("test-meter").u64_counter("test-counter").build();

    // An instrument that has not recorded yields an empty collection, which is not exported.
    mp.force_flush().expect("MeterProvider.ForceFlush() failed");
    assert_eq!(exporter.counts().0, 0, "exports before any recording");

    counter.add(1, &[]);
    mp.force_flush().expect("MeterProvider.ForceFlush() failed");
    assert_eq!(exporter.counts().0, 1, "exports after ForceFlush");
    mp.shutdown().expect("MeterProvider.Shutdown() failed");
    assert_eq!(
        exporter.counts(),
        (1, 1),
        "after Shutdown (exports, exporter shutdowns)"
    );
}

fn creds(project_id: &str) -> TelemetryOption {
    with_google_credentials(GoogleCredentials {
        credentials: None,
        project_id: project_id.to_owned(),
    })
}

struct ProjectCase {
    name: &'static str,
    opts: Vec<TelemetryOption>,
    env_var: &'static str,
    want_project: &'static str,
    want_err: bool,
}

fn run_project_cases(
    cases: Vec<ProjectCase>,
    resolve: fn(
        &crate::config::Config,
        &dyn gadk_platform::env::EnvProvider,
    ) -> Result<String, String>,
    project_type: &str,
) {
    for tc in cases {
        // Go sets GOOGLE_CLOUD_PROJECT (possibly empty) for every case.
        let env = env(&[("GOOGLE_CLOUD_PROJECT", tc.env_var)]);
        let cfg = config_from_opts(&tc.opts);
        let got = resolve(&cfg, env.as_ref());
        assert_eq!(got.is_err(), tc.want_err, "{}: {got:?}", tc.name);
        match got {
            Ok(project) => assert_eq!(project, tc.want_project, "{}", tc.name),
            Err(e) => assert_eq!(
                e,
                format!(
                    "telemetry.googleapis.com requires setting the {project_type} project. Set it with `with_gcp_{project_type}_project` or the GOOGLE_CLOUD_PROJECT environment variable"
                ),
                "{}",
                tc.name
            ),
        }
    }
}

#[test]
fn test_resolve_resource_project() {
    let cases = vec![
        ProjectCase {
            name: "project from options",
            opts: vec![
                with_otel_to_cloud(true),
                with_gcp_resource_project("option-project"),
                creds("cred-project"),
            ],
            env_var: "env-project",
            want_project: "option-project",
            want_err: false,
        },
        ProjectCase {
            name: "project from credentials",
            opts: vec![with_otel_to_cloud(true), creds("cred-project")],
            env_var: "env-project",
            want_project: "cred-project",
            want_err: false,
        },
        ProjectCase {
            name: "project from env var",
            opts: vec![with_otel_to_cloud(true)],
            env_var: "env-project",
            want_project: "env-project",
            want_err: false,
        },
        ProjectCase {
            name: "no project",
            opts: vec![with_otel_to_cloud(true), creds("")],
            env_var: "",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "no project no credentials",
            opts: vec![with_otel_to_cloud(true)],
            env_var: "",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "env var whitespace",
            opts: vec![with_otel_to_cloud(true), creds("")],
            env_var: " ",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "option project whitespace",
            opts: vec![with_otel_to_cloud(true), with_gcp_resource_project(" ")],
            env_var: "",
            want_project: "",
            want_err: true,
        },
    ];
    run_project_cases(cases, resolve_gcp_resource_project, "resource");
}

// parity: adapted: Go's resource.Default() reads the process environment; gadk's default resource reads the injected environment (no OTEL_RESOURCE_ATTRIBUTES, so it has no gcp.project_id)
#[tokio::test]
async fn test_resolve_resource_project_id() {
    let project_id_key = Key::from_static_str("gcp.project_id");
    let env = env(&[]);
    // The default resource is compared against rather than a fixed value.
    let default_project = go_default_resource(env.as_ref()).get(&project_id_key);

    let cases = [
        (
            "no project keeps the default resource's",
            vec![],
            default_project,
        ),
        (
            "configured project",
            vec![with_gcp_resource_project("option-project")],
            Some(opentelemetry::Value::from("option-project")),
        ),
    ];
    for (name, opts, want) in cases {
        let cfg = config_from_opts(&opts);
        let r = resolve_resource(&cfg, env.as_ref(), &NoDetector)
            .await
            .unwrap_or_else(|e| panic!("{name}: resolveResource() unexpected error: {e}"));
        assert_eq!(r.get(&project_id_key), want, "{name}: gcp.project_id");
    }
}

#[test]
fn test_resolve_quota_project() {
    let cases = vec![
        ProjectCase {
            name: "project from options",
            opts: vec![
                with_otel_to_cloud(true),
                with_gcp_quota_project("option-project"),
                creds("cred-project"),
            ],
            env_var: "env-project",
            want_project: "option-project",
            want_err: false,
        },
        ProjectCase {
            name: "project from credentials",
            opts: vec![with_otel_to_cloud(true), creds("cred-project")],
            env_var: "env-project",
            want_project: "cred-project",
            want_err: false,
        },
        ProjectCase {
            name: "project from env var",
            opts: vec![with_otel_to_cloud(true)],
            env_var: "env-project",
            want_project: "env-project",
            want_err: false,
        },
        ProjectCase {
            name: "no project",
            opts: vec![with_otel_to_cloud(true), creds("")],
            env_var: "",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "no project no credentials",
            opts: vec![with_otel_to_cloud(true)],
            env_var: "",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "no project and otelToCloud disabled",
            opts: vec![with_otel_to_cloud(false), creds("")],
            env_var: "",
            want_project: "",
            want_err: false,
        },
        ProjectCase {
            name: "env var whitespace",
            opts: vec![with_otel_to_cloud(true), creds("")],
            env_var: " ",
            want_project: "",
            want_err: true,
        },
        ProjectCase {
            name: "option project whitespace",
            opts: vec![with_otel_to_cloud(true), with_gcp_quota_project(" ")],
            env_var: "",
            want_project: "",
            want_err: true,
        },
    ];
    run_project_cases(cases, resolve_gcp_quota_project, "quota");
}

// parity: adapted: configure's own unregistered readers are shut down with their result unchecked (an unregistered opentelemetry_sdk PeriodicReader's final collect fails its shutdown)
#[tokio::test]
async fn test_configure_exporters() {
    #[derive(Clone, Copy)]
    enum Supplied {
        None,
        TracerProvider,
        LoggerProvider,
        MeterProvider,
    }
    struct Case {
        name: &'static str,
        otlp_endpoint: &'static str,
        otlp_traces_endpoint: &'static str,
        otlp_metrics_endpoint: &'static str,
        otlp_logs_endpoint: &'static str,
        otel_to_cloud: bool,
        supplied: Supplied,
        want_span_processors: usize,
        want_log_processors: usize,
        want_metric_exporters: usize,
    }
    let case = |name, endpoints: [&'static str; 4], otel_to_cloud, want: [usize; 3]| Case {
        name,
        otlp_endpoint: endpoints[0],
        otlp_traces_endpoint: endpoints[1],
        otlp_metrics_endpoint: endpoints[2],
        otlp_logs_endpoint: endpoints[3],
        otel_to_cloud,
        supplied: Supplied::None,
        want_span_processors: want[0],
        want_log_processors: want[1],
        want_metric_exporters: want[2],
    };
    const OTLP: &str = "http://localhost:4318";
    const TRACES: &str = "http://localhost:4318/v1/traces";
    const METRICS: &str = "http://localhost:4318/v1/metrics";
    const LOGS: &str = "http://localhost:4318/v1/logs";
    let cases = [
        case("no processors", ["", "", "", ""], false, [0, 0, 0]),
        case(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            [OTLP, "", "", ""],
            false,
            [1, 1, 1],
        ),
        case(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            ["", TRACES, "", ""],
            false,
            [1, 0, 0],
        ),
        case(
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            ["", "", METRICS, ""],
            false,
            [0, 0, 1],
        ),
        case(
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            ["", "", "", LOGS],
            false,
            [0, 1, 0],
        ),
        case(
            "OTEL_EXPORTER_OTLP_ENDPOINT and otel_to_cloud",
            [OTLP, "", "", ""],
            true,
            [2, 2, 2],
        ),
        case(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT and otel_to_cloud",
            ["", TRACES, "", ""],
            true,
            [2, 1, 1],
        ),
        case(
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT and otel_to_cloud",
            ["", "", "", LOGS],
            true,
            [1, 2, 1],
        ),
        // Nothing is built for a supplied provider: it would ignore it, and an
        // unregistered periodic reader reports an error on every tick.
        Case {
            supplied: Supplied::TracerProvider,
            ..case(
                "OTEL_EXPORTER_OTLP_ENDPOINT, otel_to_cloud and WithTracerProvider",
                [OTLP, "", "", ""],
                true,
                [0, 2, 2],
            )
        },
        Case {
            supplied: Supplied::LoggerProvider,
            ..case(
                "OTEL_EXPORTER_OTLP_ENDPOINT, otel_to_cloud and WithLoggerProvider",
                [OTLP, "", "", ""],
                true,
                [2, 0, 2],
            )
        },
        Case {
            supplied: Supplied::MeterProvider,
            ..case(
                "OTEL_EXPORTER_OTLP_ENDPOINT, otel_to_cloud and WithMeterProvider",
                [OTLP, "", "", ""],
                true,
                [2, 2, 0],
            )
        },
    ];
    for tc in cases {
        let env = env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", tc.otlp_endpoint),
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                tc.otlp_traces_endpoint,
            ),
            (
                "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
                tc.otlp_metrics_endpoint,
            ),
            ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", tc.otlp_logs_endpoint),
            // Set the quota project needed to configure GCP exporters.
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
        ]);
        let mut opts = if tc.otel_to_cloud {
            vec![with_otel_to_cloud(true), creds("test-project")]
        } else {
            vec![]
        };
        match tc.supplied {
            Supplied::None => {}
            Supplied::TracerProvider => {
                opts.push(with_tracer_provider(SdkTracerProvider::builder().build()));
            }
            Supplied::LoggerProvider => {
                opts.push(with_logger_provider(SdkLoggerProvider::builder().build()));
            }
            Supplied::MeterProvider => {
                opts.push(with_meter_provider(SdkMeterProvider::builder().build()));
            }
        }
        let cfg = configure(&env, &NoDetector, &opts)
            .await
            .unwrap_or_else(|e| panic!("{}: configure() unexpected error: {e}", tc.name));
        // Go `shutdownReaders`: stops the threads of the periodic readers configure built but
        // never registered with a provider.
        for r in &cfg.metric_readers {
            let _ = r.shutdown();
        }
        let got = configure_exporters(&cfg, env.as_ref(), GcpEndpoints::TELEMETRY_API)
            .unwrap_or_else(|e| panic!("{}: configureExporters() unexpected error: {e}", tc.name));
        assert_eq!(
            got.span_processors.len(),
            tc.want_span_processors,
            "{}: span processors",
            tc.name
        );
        assert_eq!(
            got.log_processors.len(),
            tc.want_log_processors,
            "{}: log processors",
            tc.name
        );
        assert_eq!(
            got.metric_exporters.len(),
            tc.want_metric_exporters,
            "{}: metric exporters",
            tc.name
        );
    }
}
