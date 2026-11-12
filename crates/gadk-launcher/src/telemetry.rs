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

//! Go `cmd/launcher/internal/telemetry` (`InitAndSetGlobalOtelProviders`): the console and web
//! launchers build gadk-telemetry's providers from [`Config::telemetry_options`], set the global
//! tracer provider and install one global `tracing` subscriber: the providers' bridge layer
//! first, then [`Config::telemetry_layers`].
//!
//! Divergences: Go always replaces the global OpenTelemetry providers; gadk keeps a global
//! `tracing` subscriber that is already installed and logs that the launcher's layers were not
//! installed (ledger 6). OpenTelemetry Rust has no global logger provider (ledger 7a).

use std::sync::Arc;
use std::time::Duration;

use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_telemetry::Providers;
use tracing::Dispatch;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

use crate::config::{BoxedLayer, Config};
use crate::error::Error;
use crate::golog::Logger;

/// The log line written when a global subscriber is already installed.
pub(crate) const KEPT_EXISTING_SUBSCRIBER: &str = "a global tracing subscriber is already installed; the launcher keeps it and does not install its telemetry layers (launcher.Config.TelemetryOptions)";

/// Sets the global OpenTelemetry providers (Go `Providers.SetGlobalOtelProviders`).
type SetProviders = Box<dyn FnOnce(&Providers) + Send>;
/// Installs the global `tracing` subscriber; `false` when one is already installed.
type SetSubscriber = Box<dyn FnOnce(Dispatch) -> bool + Send>;

/// What launcher telemetry reaches outside the launcher: the environment gadk-telemetry reads
/// (OTLP variables, ADC, the GCP resource detector), the global OpenTelemetry providers and the
/// global `tracing` dispatcher. [`Globals::process`] is the real ones; tests inject theirs.
pub(crate) struct Globals {
    env: Arc<dyn EnvProvider>,
    set_providers: SetProviders,
    set_subscriber: SetSubscriber,
}

impl Globals {
    /// The process environment, OpenTelemetry's global tracer provider and the global `tracing`
    /// dispatcher.
    pub(crate) fn process() -> Globals {
        Globals {
            env: Arc::new(ProcessEnv),
            set_providers: Box::new(Providers::set_global_otel_providers),
            set_subscriber: Box::new(|dispatch| {
                tracing::dispatcher::set_global_default(dispatch).is_ok()
            }),
        }
    }
}

#[cfg(test)]
impl Globals {
    /// Tests: `env` as the whole environment, `set_providers` in place of the global setter and
    /// the subscriber handed to `set_subscriber`.
    pub(crate) fn injected_with(
        env: &[(&str, &str)],
        set_providers: impl FnOnce(&Providers) + Send + 'static,
        set_subscriber: impl FnOnce(Dispatch) -> bool + Send + 'static,
    ) -> Globals {
        let env = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Globals {
            env: Arc::new(gadk_platform::env::MapEnv(env)),
            set_providers: Box::new(set_providers),
            set_subscriber: Box::new(set_subscriber),
        }
    }

    /// Tests: an empty environment, the global providers left alone and the subscriber handed to
    /// `set_subscriber` (which returns `false` to act as an already installed global one).
    pub(crate) fn injected(
        set_subscriber: impl FnOnce(Dispatch) -> bool + Send + 'static,
    ) -> Globals {
        Globals::injected_with(&[], |_| {}, set_subscriber)
    }
}

/// Go `InitAndSetGlobalOtelProviders`: builds the providers from a copy of
/// `config.telemetry_options` (with `with_otel_to_cloud(true)` appended when `otel_to_cloud`,
/// Go's `-otel_to_cloud` flag), sets OpenTelemetry's global tracer provider, then installs a
/// global `tracing` subscriber of the providers' bridge layer and the drained
/// `config.telemetry_layers`; an existing global subscriber is kept and `config.log` says so.
/// With neither providers nor layers nothing is installed. Errors are gadk-telemetry's (Go's
/// texts) as `Error::Telemetry`; callers wrap them as `telemetry initialization failed: <err>`.
/// The providers export on the calling Tokio runtime: shut the guard down before it ends.
///
/// # Errors
///
/// gadk-telemetry's setup error (credentials, projects, resource, exporters).
pub async fn init(config: &mut Config, otel_to_cloud: bool) -> Result<TelemetryGuard, Error> {
    init_with(config, otel_to_cloud, Globals::process()).await
}

/// [`init`] over injected [`Globals`].
pub(crate) async fn init_with(
    config: &mut Config,
    otel_to_cloud: bool,
    globals: Globals,
) -> Result<TelemetryGuard, Error> {
    // A copy: the embedder's list is never changed.
    let mut opts = config.telemetry_options.clone();
    if otel_to_cloud {
        opts.push(gadk_telemetry::with_otel_to_cloud(true));
    }
    let detector = gadk_telemetry::gcp_detector::new_detector(Arc::clone(&globals.env));
    let providers = gadk_telemetry::new_with(globals.env, &detector, &opts)
        .await
        .map_err(|e| Error::Telemetry(e.to_string()))?;
    (globals.set_providers)(&providers);
    // Divergence (ledger 7a; also ledger 6): an existing global `tracing` subscriber is kept and
    // logged; Go always replaces the global OpenTelemetry providers.
    if let Some(dispatch) = build(config, &providers)
        && !(globals.set_subscriber)(dispatch)
    {
        log_kept_existing(&config.log);
    }
    Ok(TelemetryGuard { providers })
}

/// The launcher's subscriber: `providers`' bridge layer, then the drained
/// `config.telemetry_layers` (the bridge runs first and owns the span IDs the debug layer
/// reuses); `None` when there are neither providers nor layers.
pub(crate) fn build(config: &mut Config, providers: &Providers) -> Option<Dispatch> {
    if providers.tracer_provider.is_none()
        && providers.logger_provider.is_none()
        && config.telemetry_layers.is_empty()
    {
        return None;
    }
    let mut layers: Vec<BoxedLayer> = vec![Box::new(providers.layer())];
    layers.append(&mut config.telemetry_layers);
    Some(Dispatch::new(Registry::default().with(layers)))
}

fn log_kept_existing(log: &Logger) {
    log.print(KEPT_EXISTING_SUBSCRIBER);
}

/// Go `*telemetry.Providers`, returned by [`init`].
#[derive(Debug)]
pub struct TelemetryGuard {
    providers: Providers,
}

impl TelemetryGuard {
    /// Go `Providers.Shutdown`: flushes and shuts the providers down.
    ///
    /// # Errors
    ///
    /// The providers' shutdown errors joined with `\n`, as `Error::Telemetry`.
    pub async fn shutdown(self) -> Result<(), Error> {
        self.providers
            .shutdown()
            .await
            .map_err(|e| Error::Telemetry(e.to_string()))
    }

    /// Go's deferred `telemetryService.Shutdown` under `context.WithTimeout(shutdownTimeout)`:
    /// a failure is logged as `telemetry shutdown failed: <err>`, a timeout as
    /// `telemetry shutdown failed: context deadline exceeded`.
    ///
    /// Divergence (ledger 6): `<err>` is opentelemetry_sdk's provider error
    /// (`Operation failed: Shutdown errors: [InternalFailure("<processor error>")]`); Go prints the
    /// processor's error.
    pub(crate) async fn shutdown_logged(self, timeout: Duration, log: &Logger) {
        match tokio::time::timeout(timeout, self.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log.print(&format!("telemetry shutdown failed: {e}")),
            Err(_) => log.print("telemetry shutdown failed: context deadline exceeded"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use opentelemetry::Context;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
    use opentelemetry_sdk::trace::{
        InMemorySpanExporter, SimpleSpanProcessor, Span, SpanData, SpanProcessor,
    };
    use pretty_assertions::assert_eq;
    use tracing_subscriber::Layer;

    use super::*;
    use crate::golog;
    use crate::test_support::NoAgents;

    /// Counts the events it sees.
    struct Counting(Arc<AtomicUsize>);

    impl Layer<Registry> for Counting {
        fn on_event(
            &self,
            _event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, Registry>,
        ) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting() -> (BoxedLayer, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        (Box::new(Counting(n.clone())), n)
    }

    /// A span processor whose shutdown fails with `text` after `delay`.
    #[derive(Debug)]
    struct FailingShutdown {
        text: &'static str,
        delay: Duration,
    }

    impl SpanProcessor for FailingShutdown {
        fn on_start(&self, _span: &mut Span, _cx: &Context) {}
        fn on_end(&self, _span: SpanData) {}
        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }
        fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
            std::thread::sleep(self.delay);
            Err(OTelSdkError::InternalFailure(self.text.to_owned()))
        }
    }

    fn failing(text: &'static str, delay: Duration) -> gadk_telemetry::TelemetryOption {
        gadk_telemetry::with_span_processors([
            Box::new(FailingShutdown { text, delay }) as Box<dyn SpanProcessor>
        ])
    }

    fn config_with_log() -> (Config, golog::Captured) {
        let (log, out) = golog::capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(|| "2009/11/10 23:00:00 ".to_owned());
        (config, out)
    }

    /// Injected globals that keep the subscriber they are handed.
    fn capturing() -> (Globals, Arc<Mutex<Option<Dispatch>>>) {
        let slot = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&slot);
        let globals = Globals::injected(move |d| {
            *keep.lock().unwrap() = Some(d);
            true
        });
        (globals, slot)
    }

    #[tokio::test]
    async fn the_subscriber_exports_adk_spans_and_runs_every_layer() {
        let (mut config, out) = config_with_log();
        let exporter = InMemorySpanExporter::default();
        config.telemetry_options =
            vec![gadk_telemetry::with_span_processors([
                Box::new(SimpleSpanProcessor::new(exporter.clone())) as Box<dyn SpanProcessor>,
            ])];
        let (a, seen_a) = counting();
        let (b, seen_b) = counting();
        config.telemetry_layers = vec![a, b];
        let (globals, slot) = capturing();
        let guard = init_with(&mut config, false, globals).await.unwrap();
        assert!(config.telemetry_layers.is_empty(), "the layers are taken");
        assert_eq!(config.telemetry_options.len(), 1, "the options are kept");

        let dispatch = slot.lock().unwrap().take().unwrap();
        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("adk", otel.name = "invoke_agent test");
            span.in_scope(|| tracing::info!("one event"));
        });
        // Read before shutdown: shutting the exporter down clears what it kept.
        let spans = exporter.get_finished_spans().unwrap();
        let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(names, ["invoke_agent test"]);
        guard.shutdown().await.unwrap();
        assert_eq!(seen_a.load(Ordering::SeqCst), 1);
        assert_eq!(seen_b.load(Ordering::SeqCst), 1);
        assert_eq!(out.contents(), "");
    }

    #[tokio::test]
    async fn the_global_tracer_provider_is_set_when_one_is_configured() {
        for (opts, want) in [(vec![failing("x", Duration::ZERO)], 1), (Vec::new(), 0)] {
            let (mut config, _) = config_with_log();
            config.telemetry_options = opts;
            let calls = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&calls);
            let globals = Globals::injected_with(
                &[],
                move |p| {
                    p.set_global_otel_providers_with(|_| {
                        count.fetch_add(1, Ordering::SeqCst);
                    });
                },
                |_| true,
            );
            let guard = init_with(&mut config, false, globals).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), want);
            let _ = guard.shutdown().await;
        }
    }

    #[tokio::test]
    async fn an_existing_global_subscriber_is_kept_and_logged() {
        let (mut config, out) = config_with_log();
        let (layer, _) = counting();
        config.telemetry_layers = vec![layer];
        init_with(&mut config, false, Globals::injected(|_| false))
            .await
            .unwrap();
        assert_eq!(
            out.contents(),
            format!("2009/11/10 23:00:00 {KEPT_EXISTING_SUBSCRIBER}\n")
        );
    }

    #[tokio::test]
    async fn no_providers_and_no_layers_install_nothing() {
        let (mut config, out) = config_with_log();
        let called = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&called);
        init_with(
            &mut config,
            false,
            Globals::injected(move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
                true
            }),
        )
        .await
        .unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert_eq!(out.contents(), "");
    }

    #[tokio::test]
    async fn an_init_error_is_gos_text_and_leaves_the_config_alone() {
        let (mut config, _) = config_with_log();
        let bad = Resource::builder_empty()
            .with_schema_url(
                Vec::<opentelemetry::KeyValue>::new(),
                "https://conflicting.invalid/schema/v1",
            )
            .build();
        config.telemetry_options = vec![gadk_telemetry::with_resource(bad)];
        let (layer, _) = counting();
        config.telemetry_layers = vec![layer];
        let err = init_with(&mut config, false, Globals::injected(|_| true))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Telemetry(_)));
        assert_eq!(
            err.to_string(),
            "failed to resolve resource: failed to merge with config resource: conflicting Schema URL: https://opentelemetry.io/schemas/1.43.0 and https://conflicting.invalid/schema/v1"
        );
        assert_eq!(config.telemetry_layers.len(), 1);
        assert_eq!(config.telemetry_options.len(), 1);
    }

    #[tokio::test]
    async fn otel_to_cloud_turns_cloud_export_on() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let missing = missing.to_string_lossy().into_owned();
        let (mut config, _) = config_with_log();
        let globals = Globals::injected_with(
            &[("GOOGLE_APPLICATION_CREDENTIALS", &missing)],
            |_| {},
            |_| true,
        );
        let err = init_with(&mut config, true, globals).await.unwrap_err();
        let io = std::fs::read(&missing).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "failed to find default credentials: google: error getting credentials using GOOGLE_APPLICATION_CREDENTIALS environment variable: {io}"
            )
        );
        assert!(
            config.telemetry_options.is_empty(),
            "the flag's option was appended to a copy"
        );
    }

    #[tokio::test]
    async fn shutdown_returns_the_providers_error() {
        let (mut config, _) = config_with_log();
        config.telemetry_options = vec![failing("simulated flush error", Duration::ZERO)];
        let guard = init_with(&mut config, false, Globals::injected(|_| true))
            .await
            .unwrap();
        let err = guard.shutdown().await.unwrap_err();
        assert!(matches!(err, Error::Telemetry(_)));
        assert_eq!(
            err.to_string(),
            "Operation failed: Shutdown errors: [InternalFailure(\"simulated flush error\")]"
        );
    }

    #[tokio::test]
    async fn shutdown_logged_logs_a_failure_and_a_timeout() {
        let (mut config, out) = config_with_log();
        config.telemetry_options = vec![failing("simulated flush error", Duration::ZERO)];
        let guard = init_with(&mut config, false, Globals::injected(|_| true))
            .await
            .unwrap();
        guard
            .shutdown_logged(Duration::from_secs(5), &config.log)
            .await;
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 telemetry shutdown failed: Operation failed: Shutdown errors: [InternalFailure(\"simulated flush error\")]\n"
        );

        let (mut config, out) = config_with_log();
        config.telemetry_options = vec![failing("late", Duration::from_millis(300))];
        let guard = init_with(&mut config, false, Globals::injected(|_| true))
            .await
            .unwrap();
        guard
            .shutdown_logged(Duration::from_millis(10), &config.log)
            .await;
        assert_eq!(
            out.contents(),
            "2009/11/10 23:00:00 telemetry shutdown failed: context deadline exceeded\n"
        );
    }
}
