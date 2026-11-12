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

use std::sync::Arc;

use gadk_platform::env::EnvProvider;
use http::{HeaderName, HeaderValue};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{LogExporter, SpanExporter, WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use tokio::runtime::Handle;

use crate::config::{
    Config, GoogleCredentials, SharedLogProcessor, SharedSpanProcessor, TelemetryOption,
    config_from_opts,
};
use crate::credentials::{CLOUD_PLATFORM_SCOPE, find_default_credentials};
use crate::error::Error;
use crate::gcp_detector::ResourceDetector;
use crate::http_client::RuntimeHttpClient;
use crate::resource::{go_default_resource, go_merge, new_with_attributes};

/// Go `newGcpSpanExporter`'s endpoint.
pub(crate) const GCP_TRACES_ENDPOINT: &str = "https://telemetry.googleapis.com/v1/traces";

/// Why an exporter could not be created outside a Tokio runtime.
const NO_RUNTIME: &str = "no Tokio runtime: telemetry exporters must be created from within one";

/// Go `configure`: ADC when cloud export is on and no credentials were given, the quota and
/// resource projects, the resource, then the exporters' processors appended to the configured
/// ones. `env` is the injected environment and `detector` the GCP resource detector.
pub(crate) async fn configure(
    env: &Arc<dyn EnvProvider>,
    detector: &dyn ResourceDetector,
    opts: &[TelemetryOption],
) -> Result<Config, Error> {
    let mut cfg = config_from_opts(opts);

    if cfg.otel_to_cloud {
        // Load ADC if no credentials are provided in the config.
        if cfg.google_credentials.is_none() {
            let creds = find_default_credentials(env, &[CLOUD_PLATFORM_SCOPE])
                .await
                .map_err(|e| Error::new(format!("failed to find default credentials: {e}")))?;
            cfg.google_credentials = Some(creds);
        }
        cfg.gcp_quota_project = resolve_gcp_quota_project(&cfg, env.as_ref())
            .map_err(|e| Error::new(format!("failed to resolve GCP quota project: {e}")))?;
        cfg.gcp_resource_project = resolve_gcp_resource_project(&cfg, env.as_ref())
            .map_err(|e| Error::new(format!("failed to resolve GCP resource project: {e}")))?;
    }

    let resource = resolve_resource(&cfg, env.as_ref(), detector)
        .await
        .map_err(|e| Error::new(format!("failed to resolve resource: {e}")))?;
    cfg.resource = Some(resource);

    let (span_processors, log_processors) = configure_exporters(&cfg, env.as_ref())
        .map_err(|e| Error::new(format!("failed to configure exporters: {e}")))?;
    cfg.span_processors.extend(span_processors);
    cfg.log_processors.extend(log_processors);
    Ok(cfg)
}

/// Go `resolveGcpQuotaProject`.
pub(crate) fn resolve_gcp_quota_project(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<String, String> {
    resolve_project(
        &cfg.gcp_quota_project,
        cfg.google_credentials.as_ref(),
        cfg.otel_to_cloud,
        "quota",
        env,
    )
}

/// Go `resolveGcpResourceProject`.
pub(crate) fn resolve_gcp_resource_project(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<String, String> {
    resolve_project(
        &cfg.gcp_resource_project,
        cfg.google_credentials.as_ref(),
        cfg.otel_to_cloud,
        "resource",
        env,
    )
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
    let configured_project = configured_project.trim();
    if !configured_project.is_empty() {
        return Ok(configured_project.to_owned());
    }
    if let Some(creds) = creds.filter(|c| !c.project_id.is_empty()) {
        return Ok(creds.project_id.clone());
    }
    let project = env.var("GOOGLE_CLOUD_PROJECT").unwrap_or_default();
    let project = project.trim();
    if require_project && project.is_empty() {
        return Err(format!(
            "telemetry.googleapis.com requires setting the {project_type} project. Refer to telemetry.config for the available options to set the {project_type} project"
        ));
    }
    Ok(project.to_owned())
}

/// Go `resolveResource`: Go's default resource ← `gcp.project_id` and, with cloud export on,
/// the GCP detector's resource ← the configured resource.
pub(crate) async fn resolve_resource(
    cfg: &Config,
    env: &dyn EnvProvider,
    detector: &dyn ResourceDetector,
) -> Result<Resource, String> {
    let r = go_default_resource(env);

    // Go `resource.New(ctx, WithAttributes(gcp.project_id), WithDetectors(gcp.NewDetector()))`.
    let mut gcp_resource = new_with_attributes(
        "",
        [KeyValue::new(
            "gcp.project_id",
            cfg.gcp_resource_project.clone(),
        )],
    );
    if cfg.otel_to_cloud {
        let detected = match detector.detect().await {
            Ok(detected) => detected,
            Err(partial) => {
                return Err(format!(
                    "failed to create GCP resource: error detecting resource: {partial}"
                ));
            }
        };
        if let Some(detected) = detected {
            gcp_resource = go_merge(&gcp_resource, &detected).map_err(|e| {
                format!("failed to create GCP resource: error detecting resource: {e}")
            })?;
        }
    }
    // Merge with the default resource.
    let r = go_merge(&r, &gcp_resource)
        .map_err(|e| format!("failed to merge default and GCP resources: {e}"))?;
    // Lastly, merge with the resource from config.
    match &cfg.resource {
        Some(configured) => go_merge(&r, configured)
            .map_err(|e| format!("failed to merge with config resource: {e}")),
        None => Ok(r),
    }
}

/// The trimmed value of `key` in `env` (empty when unset).
fn trimmed_var(env: &dyn EnvProvider, key: &str) -> String {
    env.var(key).unwrap_or_default().trim().to_owned()
}

/// The runtime exporters send on.
fn runtime_handle() -> Result<Handle, String> {
    Handle::try_current().map_err(|_| NO_RUNTIME.to_owned())
}

/// Go `configureExporters`: batch processors for the OTLP/HTTP exporters the environment asks
/// for and, with cloud export on, the GCP span exporter. Exporters send through a
/// [`crate::RuntimeHttpClient`] on the current Tokio runtime.
// Divergence (ledger 7a): only the endpoints come from the injected environment; the other
// `OTEL_EXPORTER_OTLP_*` variables (headers, timeout, compression, protocol) are read by
// opentelemetry-otlp from the process environment.
pub(crate) fn configure_exporters(
    cfg: &Config,
    env: &dyn EnvProvider,
) -> Result<(Vec<SharedSpanProcessor>, Vec<SharedLogProcessor>), String> {
    let mut span_processors = Vec::new();
    let mut log_processors = Vec::new();

    let otel_endpoint_env = trimmed_var(env, "OTEL_EXPORTER_OTLP_ENDPOINT");
    // Tracing section.
    let otel_traces_endpoint_env = trimmed_var(env, "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    if !otel_endpoint_env.is_empty() || !otel_traces_endpoint_env.is_empty() {
        let endpoint = signal_endpoint(&otel_endpoint_env, &otel_traces_endpoint_env, "/v1/traces");
        let exporter = runtime_handle()
            .and_then(|handle| {
                SpanExporter::builder()
                    .with_http()
                    .with_http_client(RuntimeHttpClient::new(handle))
                    .with_endpoint(endpoint)
                    .build()
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("failed to create OTLP HTTP exporter: {e}"))?;
        span_processors.push(SharedSpanProcessor::new(Box::new(
            BatchSpanProcessor::builder(exporter).build(),
        )));
    }
    if cfg.otel_to_cloud {
        let exporter = new_gcp_span_exporter(cfg, GCP_TRACES_ENDPOINT)
            .map_err(|e| format!("failed to create GCP span exporter: {e}"))?;
        span_processors.push(SharedSpanProcessor::new(Box::new(
            BatchSpanProcessor::builder(exporter).build(),
        )));
    }
    // Logs section.
    let otel_logs_endpoint_env = trimmed_var(env, "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT");
    if !otel_endpoint_env.is_empty() || !otel_logs_endpoint_env.is_empty() {
        let endpoint = signal_endpoint(&otel_endpoint_env, &otel_logs_endpoint_env, "/v1/logs");
        let exporter = runtime_handle()
            .and_then(|handle| {
                LogExporter::builder()
                    .with_http()
                    .with_http_client(RuntimeHttpClient::new(handle))
                    .with_endpoint(endpoint)
                    .build()
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| format!("failed to create OTLP HTTP log exporter: {e}"))?;
        log_processors.push(SharedLogProcessor::new(Box::new(
            BatchLogProcessor::builder(exporter).build(),
        )));
    }
    // Golang OTel exporter to CloudLogging is not yet available.
    Ok((span_processors, log_processors))
}

/// The OTLP/HTTP endpoint of one signal, as Go's `otlptracehttp` / `otlploghttp` read it: the
/// per-signal variable as is, else the generic endpoint with `path` joined to its path (Go
/// `path.Join`, query dropped). An unparsable generic endpoint is returned as is, for the
/// exporter to reject.
fn signal_endpoint(generic: &str, signal: &str, path: &str) -> String {
    if !signal.is_empty() {
        return signal.to_owned();
    }
    let Ok(uri) = generic.parse::<http::Uri>() else {
        return generic.to_owned();
    };
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return generic.to_owned();
    };
    format!("{scheme}://{authority}{}", go_path_join(uri.path(), path))
}

/// Go `path.Join(base, elem)` for absolute paths: joined, then cleaned (`.`/`..` resolved,
/// repeated and trailing slashes removed).
fn go_path_join(base: &str, elem: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in base.split('/').chain(elem.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// Go `newGcpSpanExporter`: OTLP/HTTP protobuf to `endpoint` ([`GCP_TRACES_ENDPOINT`] in
/// production) with the credentials' headers and `x-goog-user-project: <quota project>`.
pub(crate) fn new_gcp_span_exporter(
    cfg: &Config,
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, String> {
    let mut client = RuntimeHttpClient::new(runtime_handle()?);
    // Go `oauth2.NewClient(ctx, nil)` is a plain client: no token source, no credentials.
    if let Some(credentials) = cfg
        .google_credentials
        .as_ref()
        .and_then(|c| c.credentials.clone())
    {
        client = client.with_credentials(credentials);
    }
    // Pass the quota project id in headers to fix auth errors.
    // https://cloud.google.com/docs/authentication/adc-troubleshooting/user-creds
    let quota_project = HeaderValue::from_str(&cfg.gcp_quota_project).map_err(|e| e.to_string())?;
    client = client.with_header(
        HeaderName::from_static("x-goog-user-project"),
        quota_project,
    );
    SpanExporter::builder()
        .with_http()
        .with_http_client(client)
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| e.to_string())
}

/// Go `initTracerProvider`: the configured provider, else `None` without span processors, else
/// a provider with the resource and the processors.
pub(crate) fn init_tracer_provider(cfg: &Config) -> Option<SdkTracerProvider> {
    if let Some(provider) = &cfg.tracer_provider {
        return Some(provider.clone());
    }
    if cfg.span_processors.is_empty() {
        return None;
    }
    let mut builder = SdkTracerProvider::builder();
    if let Some(resource) = &cfg.resource {
        builder = builder.with_resource(resource.clone());
    }
    for processor in &cfg.span_processors {
        builder = builder.with_span_processor(processor.clone());
    }
    Some(builder.build())
}

/// Go `initLoggerProvider`.
pub(crate) fn init_logger_provider(cfg: &Config) -> Option<SdkLoggerProvider> {
    if let Some(provider) = &cfg.logger_provider {
        return Some(provider.clone());
    }
    if cfg.log_processors.is_empty() {
        return None;
    }
    let mut builder = SdkLoggerProvider::builder();
    if let Some(resource) = &cfg.resource {
        builder = builder.with_resource(resource.clone());
    }
    for processor in &cfg.log_processors {
        builder = builder.with_log_processor(processor.clone());
    }
    Some(builder.build())
}

#[cfg(test)]
mod tests {
    use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _};
    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry::{Key, KeyValue};
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::common::v1::any_value;
    use opentelemetry_sdk::trace::BatchSpanProcessor;
    use pretty_assertions::assert_eq;
    use prost::Message as _;

    use super::*;
    use crate::config::{
        config_from_opts, with_gcp_quota_project, with_gcp_resource_project,
        with_google_credentials, with_otel_to_cloud, with_resource,
    };
    use crate::gcp_detector::PartialResource;
    use crate::providers::Providers;
    use crate::resource::{SCHEMA_URL, new_with_attributes};
    use crate::test_support::{Captured, FixedDetector, NoDetector, Sink, env, fake_credentials};

    fn attr(r: &Resource, key: &str) -> Option<String> {
        r.get(&Key::new(key.to_owned())).map(|v| v.to_string())
    }

    fn detected(attrs: &[(&'static str, &'static str)]) -> FixedDetector {
        FixedDetector(Ok(Some(new_with_attributes(
            SCHEMA_URL,
            attrs.iter().map(|(k, v)| KeyValue::new(*k, *v)),
        ))))
    }

    fn cloud_credentials(project_id: &str) -> GoogleCredentials {
        GoogleCredentials {
            credentials: None,
            project_id: project_id.to_owned(),
        }
    }

    #[tokio::test]
    async fn resource_without_cloud_export_has_an_empty_gcp_project_and_no_detection() {
        let cfg = config_from_opts(&[]);
        let r = resolve_resource(
            &cfg,
            env(&[]).as_ref(),
            &detected(&[("cloud.provider", "gcp")]),
        )
        .await
        .unwrap();
        assert_eq!(attr(&r, "gcp.project_id").as_deref(), Some(""));
        assert_eq!(attr(&r, "cloud.provider"), None);
        assert_eq!(attr(&r, "telemetry.sdk.language").as_deref(), Some("rust"));
        assert_eq!(r.schema_url(), Some(SCHEMA_URL));
    }

    #[tokio::test]
    async fn resource_with_cloud_export_merges_the_detector() {
        let mut cfg = config_from_opts(&[with_otel_to_cloud(true)]);
        cfg.gcp_resource_project = "resource-project".to_owned();
        let r = resolve_resource(
            &cfg,
            env(&[("OTEL_SERVICE_NAME", "env-service")]).as_ref(),
            &detected(&[("cloud.provider", "gcp"), ("service.name", "detected")]),
        )
        .await
        .unwrap();
        assert_eq!(
            attr(&r, "gcp.project_id").as_deref(),
            Some("resource-project")
        );
        assert_eq!(attr(&r, "cloud.provider").as_deref(), Some("gcp"));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("detected"));
        assert_eq!(r.schema_url(), Some(SCHEMA_URL));
    }

    #[tokio::test]
    async fn the_config_resource_wins() {
        let cfg = config_from_opts(&[
            with_gcp_resource_project("resource-project"),
            with_resource(new_with_attributes(
                "",
                [
                    KeyValue::new("service.name", "configured"),
                    KeyValue::new("gcp.project_id", "x"),
                ],
            )),
        ]);
        let r = resolve_resource(&cfg, env(&[]).as_ref(), &NoDetector)
            .await
            .unwrap();
        assert_eq!(attr(&r, "service.name").as_deref(), Some("configured"));
        assert_eq!(attr(&r, "gcp.project_id").as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn a_conflicting_config_schema_url_is_gos_error() {
        let cfg = config_from_opts(&[with_resource(new_with_attributes(
            "https://example.com/schemas/9",
            [KeyValue::new("service.name", "configured")],
        ))]);
        let err = resolve_resource(&cfg, env(&[]).as_ref(), &NoDetector)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "failed to merge with config resource: conflicting Schema URL: https://opentelemetry.io/schemas/1.43.0 and https://example.com/schemas/9"
        );
    }

    #[tokio::test]
    async fn a_conflicting_detector_schema_url_is_gos_error() {
        let cfg = config_from_opts(&[with_otel_to_cloud(true)]);
        let detector = FixedDetector(Ok(Some(new_with_attributes(
            "https://example.com/schemas/9",
            [KeyValue::new("cloud.provider", "gcp")],
        ))));
        let err = resolve_resource(&cfg, env(&[]).as_ref(), &detector)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "failed to merge default and GCP resources: conflicting Schema URL: https://opentelemetry.io/schemas/1.43.0 and https://example.com/schemas/9"
        );
    }

    #[tokio::test]
    async fn a_partial_detection_is_gos_error() {
        let cfg = config_from_opts(&[with_otel_to_cloud(true)]);
        let detector = FixedDetector(Err(PartialResource {
            resource: new_with_attributes(SCHEMA_URL, [KeyValue::new("cloud.provider", "gcp")]),
            errors: vec!["first".to_owned(), "second".to_owned()],
        }));
        let err = resolve_resource(&cfg, env(&[]).as_ref(), &detector)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "failed to create GCP resource: error detecting resource: partial resource: first\nsecond"
        );
    }

    #[tokio::test]
    async fn configure_reports_a_credentials_failure() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let missing = missing.to_string_lossy().into_owned();
        let env = env(&[("GOOGLE_APPLICATION_CREDENTIALS", &missing)]);
        let err = configure(&env, &NoDetector, &[with_otel_to_cloud(true)])
            .await
            .unwrap_err();
        let io = std::fs::read(&missing).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "failed to find default credentials: google: error getting credentials using GOOGLE_APPLICATION_CREDENTIALS environment variable: {io}"
            )
        );
    }

    #[tokio::test]
    async fn configure_reports_a_missing_quota_project() {
        let err = configure(
            &env(&[]),
            &NoDetector,
            &[
                with_otel_to_cloud(true),
                with_google_credentials(cloud_credentials("")),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to resolve GCP quota project: telemetry.googleapis.com requires setting the quota project. Refer to telemetry.config for the available options to set the quota project"
        );
    }

    #[tokio::test]
    async fn configure_reports_a_missing_resource_project() {
        let err = configure(
            &env(&[]),
            &NoDetector,
            &[
                with_otel_to_cloud(true),
                with_google_credentials(cloud_credentials("")),
                with_gcp_quota_project("quota-project"),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to resolve GCP resource project: telemetry.googleapis.com requires setting the resource project. Refer to telemetry.config for the available options to set the resource project"
        );
    }

    #[tokio::test]
    async fn configure_reports_a_resource_failure() {
        let err = configure(
            &env(&[]),
            &NoDetector,
            &[with_resource(new_with_attributes(
                "https://example.com/schemas/9",
                [],
            ))],
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to resolve resource: failed to merge with config resource: conflicting Schema URL: https://opentelemetry.io/schemas/1.43.0 and https://example.com/schemas/9"
        );
    }

    #[tokio::test]
    async fn configure_resolves_the_projects_and_resource() {
        let cfg = configure(
            &env(&[("GOOGLE_CLOUD_PROJECT", "env-project")]),
            &detected(&[("cloud.provider", "gcp")]),
            &[
                with_otel_to_cloud(true),
                with_google_credentials(cloud_credentials("")),
                with_gcp_quota_project(" quota-project "),
            ],
        )
        .await
        .unwrap();
        assert_eq!(cfg.gcp_quota_project, "quota-project");
        assert_eq!(cfg.gcp_resource_project, "env-project");
        let r = cfg.resource.unwrap();
        assert_eq!(attr(&r, "gcp.project_id").as_deref(), Some("env-project"));
        assert_eq!(attr(&r, "cloud.provider").as_deref(), Some("gcp"));
        // The GCP span exporter's batch processor.
        assert_eq!(cfg.span_processors.len(), 1);
        assert!(cfg.log_processors.is_empty());
    }

    #[test]
    fn the_generic_endpoint_gets_the_signal_path() {
        let path = "/v1/traces";
        assert_eq!(
            signal_endpoint("http://h:4318", "", path),
            "http://h:4318/v1/traces"
        );
        assert_eq!(
            signal_endpoint("http://h:4318/", "", path),
            "http://h:4318/v1/traces"
        );
        assert_eq!(
            signal_endpoint("https://h/base//", "", path),
            "https://h/base/v1/traces"
        );
        assert_eq!(
            signal_endpoint("http://h:4318?x=1", "", path),
            "http://h:4318/v1/traces"
        );
        assert_eq!(signal_endpoint("not a url", "", path), "not a url");
    }

    #[test]
    fn the_signal_endpoint_is_used_as_is_and_wins() {
        assert_eq!(
            signal_endpoint("http://h:4318", "http://t:1/custom", "/v1/logs"),
            "http://t:1/custom"
        );
    }

    fn trace_request(c: &Captured) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest::decode(c.body.as_slice()).unwrap()
    }

    fn span_names(c: &Captured) -> Vec<String> {
        trace_request(c)
            .resource_spans
            .iter()
            .flat_map(|rs| &rs.scope_spans)
            .flat_map(|ss| &ss.spans)
            .map(|s| s.name.clone())
            .collect()
    }

    fn log_bodies(c: &Captured) -> Vec<String> {
        ExportLogsServiceRequest::decode(c.body.as_slice())
            .unwrap()
            .resource_logs
            .iter()
            .flat_map(|rl| &rl.scope_logs)
            .flat_map(|sl| &sl.log_records)
            .filter_map(|r| match r.body.as_ref()?.value.as_ref()? {
                any_value::Value::StringValue(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Builds the providers from `env`, records one span and one log record, and shuts down.
    async fn export_through(env: Arc<dyn EnvProvider>) {
        let cfg = configure(&env, &NoDetector, &[]).await.unwrap();
        let providers = Providers {
            tracer_provider: init_tracer_provider(&cfg),
            logger_provider: init_logger_provider(&cfg),
        };
        let tracer = providers.tracer_provider.as_ref().unwrap().tracer("t");
        tracer.in_span("exported-span", |_| {});
        let logger = providers.logger_provider.as_ref().unwrap().logger("l");
        let mut record = logger.create_log_record();
        record.set_body(AnyValue::from("exported-log"));
        logger.emit(record);
        providers.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_generic_otlp_endpoint_exports_both_signals() {
        let sink = Sink::start().await;
        export_through(env(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            &format!(" {} ", sink.url),
        )]))
        .await;
        let got = sink.requests();
        let traces: Vec<_> = got.iter().filter(|c| c.path == "/v1/traces").collect();
        let logs: Vec<_> = got.iter().filter(|c| c.path == "/v1/logs").collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(span_names(traces[0]), ["exported-span"]);
        assert_eq!(log_bodies(logs[0]), ["exported-log"]);
        assert_eq!(traces[0].headers["content-type"], "application/x-protobuf");
        let resource = trace_request(traces[0]).resource_spans[0]
            .resource
            .clone()
            .unwrap();
        assert!(
            resource
                .attributes
                .iter()
                .any(|kv| kv.key == "gcp.project_id")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn per_signal_otlp_endpoints_are_used_as_is() {
        let sink = Sink::start().await;
        export_through(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1"),
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                &format!("{}/custom/traces", sink.url),
            ),
            (
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                &format!("{}/custom/logs", sink.url),
            ),
        ]))
        .await;
        let mut paths: Vec<_> = sink.requests().into_iter().map(|c| c.path).collect();
        paths.sort();
        assert_eq!(paths, ["/custom/logs", "/custom/traces"]);
    }

    #[tokio::test]
    async fn an_invalid_otlp_endpoint_is_gos_error_text() {
        let cfg = config_from_opts(&[]);
        let err = configure_exporters(
            &cfg,
            env(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "::")]).as_ref(),
        )
        .unwrap_err();
        assert!(
            err.starts_with("failed to create OTLP HTTP exporter: "),
            "{err}"
        );
        let err = configure_exporters(
            &cfg,
            env(&[("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", "::")]).as_ref(),
        )
        .unwrap_err();
        assert!(
            err.starts_with("failed to create OTLP HTTP log exporter: "),
            "{err}"
        );
    }

    /// Exports one span through Go's GCP span exporter pointed at `sink`.
    async fn export_to_cloud(sink: &Sink, credentials: GoogleCredentials) -> Captured {
        let mut cfg = config_from_opts(&[
            with_otel_to_cloud(true),
            with_google_credentials(credentials),
        ]);
        cfg.gcp_quota_project = "quota-project".to_owned();
        let exporter = new_gcp_span_exporter(&cfg, &format!("{}/v1/traces", sink.url)).unwrap();
        let providers = Providers {
            tracer_provider: Some(
                SdkTracerProvider::builder()
                    .with_span_processor(BatchSpanProcessor::builder(exporter).build())
                    .build(),
            ),
            logger_provider: None,
        };
        providers
            .tracer_provider
            .as_ref()
            .unwrap()
            .tracer("t")
            .in_span("cloud-span", |_| {});
        providers.shutdown().await.unwrap();
        let mut got = sink.requests();
        assert_eq!(got.len(), 1, "{got:?}");
        got.remove(0)
    }

    fn values(c: &Captured, name: &str) -> Vec<String> {
        c.headers
            .get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_gcp_exporter_sends_the_quota_project_and_credentials() {
        let sink = Sink::start().await;
        let got = export_to_cloud(
            &sink,
            GoogleCredentials {
                credentials: Some(fake_credentials(&[
                    ("authorization", "Bearer token"),
                    ("x-goog-user-project", "from-credentials"),
                ])),
                project_id: "p".to_owned(),
            },
        )
        .await;
        assert_eq!(got.path, "/v1/traces");
        assert_eq!(values(&got, "x-goog-user-project"), ["quota-project"]);
        assert_eq!(values(&got, "authorization"), ["Bearer token"]);
        assert_eq!(values(&got, "content-type"), ["application/x-protobuf"]);
        assert_eq!(span_names(&got), ["cloud-span"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_gcp_exporter_without_a_token_source_sends_no_authorization() {
        let sink = Sink::start().await;
        let got = export_to_cloud(&sink, cloud_credentials("p")).await;
        assert_eq!(values(&got, "x-goog-user-project"), ["quota-project"]);
        assert!(values(&got, "authorization").is_empty());
    }

    #[tokio::test]
    async fn providers_are_none_without_processors_and_kept_when_supplied() {
        let cfg = config_from_opts(&[]);
        assert!(init_tracer_provider(&cfg).is_none());
        assert!(init_logger_provider(&cfg).is_none());
        let cfg = config_from_opts(&[with_gcp_resource_project("p")]);
        assert!(init_tracer_provider(&cfg).is_none());
    }
}
