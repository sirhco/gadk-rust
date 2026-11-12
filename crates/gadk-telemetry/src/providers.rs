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

//! The telemetry providers (port of ADK-Go `telemetry/telemetry.go`).

use std::sync::Arc;

use gadk_platform::env::{EnvProvider, ProcessEnv};
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tokio::runtime::Handle;

use crate::bridge::OtelBridge;
use crate::config::TelemetryOption;
use crate::error::Error;
use crate::gcp_detector::{self, ResourceDetector};
use crate::setup;

/// Go `telemetry.Providers`: the configured providers, each `None` when nothing configures it.
#[derive(Debug, Clone, Default)]
pub struct Providers {
    /// The configured tracer provider or `None`.
    pub tracer_provider: Option<SdkTracerProvider>,
    /// The configured logger provider or `None`.
    pub logger_provider: Option<SdkLoggerProvider>,
}

impl Providers {
    /// Go `Shutdown`: shuts both providers down and returns their errors joined with `\n` (Go
    /// `errors.Join`). The SDK's blocking shutdowns run on `spawn_blocking`, so a current-thread
    /// runtime keeps driving the exports they wait for. A second call returns `Ok`, as Go's does.
    ///
    /// # Errors
    ///
    /// The providers' shutdown errors.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let tracer_provider = self.tracer_provider.clone();
        let logger_provider = self.logger_provider.clone();
        let shutdown = move || {
            let mut errs = Vec::new();
            if let Some(tp) = tracer_provider {
                push_err(&mut errs, tp.shutdown());
            }
            if let Some(lp) = logger_provider {
                push_err(&mut errs, lp.shutdown());
            }
            errs
        };
        let errs = match Handle::try_current() {
            Ok(handle) => handle
                .spawn_blocking(shutdown)
                .await
                .map_err(|e| Error::new(e.to_string()))?,
            Err(_) => shutdown(),
        };
        if errs.is_empty() {
            Ok(())
        } else {
            Err(Error::new(errs.join("\n")))
        }
    }

    /// Go `SetGlobalOtelProviders`: sets OpenTelemetry's global tracer provider when one is
    /// configured. OpenTelemetry Rust has no global logger provider.
    // Divergence (ledger 7a): no global logger provider is set (OpenTelemetry Rust has none).
    pub fn set_global_otel_providers(&self) {
        self.set_global_otel_providers_with(|tp| {
            opentelemetry::global::set_tracer_provider(tp);
        });
    }

    /// [`Providers::set_global_otel_providers`] with the global setter injected (tests).
    pub fn set_global_otel_providers_with(
        &self,
        set_tracer_provider: impl FnOnce(SdkTracerProvider),
    ) {
        if let Some(tp) = &self.tracer_provider {
            set_tracer_provider(tp.clone());
        }
    }

    /// The `tracing` layer exporting gadk's ADK spans and log records through these providers.
    pub fn layer(&self) -> OtelBridge {
        OtelBridge::new(self.tracer_provider.as_ref(), self.logger_provider.as_ref())
    }
}

/// Records a provider shutdown error; the SDK's already-shut-down error is success, as a
/// second Go `Shutdown` is.
fn push_err(errs: &mut Vec<String>, result: OTelSdkResult) {
    match result {
        Ok(()) | Err(OTelSdkError::AlreadyShutdown) => {}
        Err(e) => errs.push(e.to_string()),
    }
}

/// Go `telemetry.New`: configures the providers from `opts` and the process environment.
///
/// # Errors
///
/// Go's setup error texts (credentials, projects, resource, exporters).
pub async fn new(opts: &[TelemetryOption]) -> Result<Providers, Error> {
    let env: Arc<dyn EnvProvider> = Arc::new(ProcessEnv);
    let detector = gcp_detector::new_detector(Arc::clone(&env));
    new_with(env, &detector, opts).await
}

/// [`new`] with the environment and the GCP resource detector injected.
pub(crate) async fn new_with(
    env: Arc<dyn EnvProvider>,
    detector: &dyn ResourceDetector,
    opts: &[TelemetryOption],
) -> Result<Providers, Error> {
    let cfg = setup::configure(&env, detector, opts).await?;
    // Go initializes no meter provider (adk-go issue #479).
    Ok(Providers {
        tracer_provider: setup::init_tracer_provider(&cfg),
        logger_provider: setup::init_logger_provider(&cfg),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_sdk::logs::LogProcessor;
    use opentelemetry_sdk::trace::SpanProcessor;
    use pretty_assertions::assert_eq;
    use prost::Message as _;

    use super::*;
    use crate::config::{with_log_record_processors, with_span_processors};
    use crate::test_support::{FailingLogProcessor, FailingSpanProcessor, NoDetector, Sink, env};

    #[tokio::test]
    async fn no_options_configure_no_providers() {
        let providers = new_with(env(&[]), &NoDetector, &[]).await.unwrap();
        assert!(providers.tracer_provider.is_none());
        assert!(providers.logger_provider.is_none());
        providers.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_joins_the_provider_errors_and_is_idempotent() {
        let providers = new_with(
            env(&[]),
            &NoDetector,
            &[
                with_span_processors([
                    Box::new(FailingSpanProcessor("trace boom")) as Box<dyn SpanProcessor>
                ]),
                with_log_record_processors([
                    Box::new(FailingLogProcessor("log boom")) as Box<dyn LogProcessor>
                ]),
            ],
        )
        .await
        .unwrap();
        let err = providers.shutdown().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Operation failed: Shutdown errors: [InternalFailure(\"trace boom\")]\nOperation failed: Shutdown errors: [InternalFailure(\"log boom\")]"
        );
        providers.shutdown().await.unwrap();
    }

    // The SDK's shutdown blocks until the batch processor's export finishes, and that export runs
    // on this current-thread runtime: shutting down on `spawn_blocking` keeps it free.
    #[tokio::test]
    async fn shutdown_flushes_on_a_current_thread_runtime() {
        let sink = Sink::start().await;
        let providers = new_with(
            env(&[(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                &format!("{}/v1/traces", sink.url),
            )]),
            &NoDetector,
            &[],
        )
        .await
        .unwrap();
        providers
            .tracer_provider
            .as_ref()
            .unwrap()
            .tracer("t")
            .in_span("flushed", |_| {});
        providers.shutdown().await.unwrap();
        let got = sink.requests();
        assert_eq!(got.len(), 1);
        let req = ExportTraceServiceRequest::decode(got[0].body.as_slice()).unwrap();
        assert_eq!(
            req.resource_spans[0].scope_spans[0].spans[0].name,
            "flushed"
        );
    }

    #[tokio::test]
    async fn the_global_setter_gets_the_tracer_provider() {
        let providers = new_with(
            env(&[]),
            &NoDetector,
            &[with_span_processors([
                Box::new(FailingSpanProcessor("x")) as Box<dyn SpanProcessor>
            ])],
        )
        .await
        .unwrap();
        let calls = Cell::new(0);
        providers.set_global_otel_providers_with(|_| calls.set(calls.get() + 1));
        assert_eq!(calls.get(), 1);

        let calls = Cell::new(0);
        Providers::default().set_global_otel_providers_with(|_| calls.set(calls.get() + 1));
        assert_eq!(calls.get(), 0);
    }
}
