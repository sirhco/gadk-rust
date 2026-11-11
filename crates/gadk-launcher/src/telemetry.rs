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

//! Go `cmd/launcher/internal/telemetry` (`InitAndSetGlobalOtelProviders`) on `tracing`: the
//! console and web launchers install one global subscriber built from
//! [`Config::telemetry_layers`].
//!
//! Divergences (ledger 6): Go always replaces the global OpenTelemetry providers; gadk keeps a
//! global `tracing` subscriber that is already installed and logs that the launcher's layers were
//! not installed. `-otel_to_cloud` needs OpenTelemetry export (sub-project 7) and fails.

use tracing::Dispatch;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

use crate::config::Config;
use crate::error::Error;
use crate::golog::Logger;

/// The `-otel_to_cloud` failure until sub-project 7 adds OpenTelemetry export.
pub(crate) const OTEL_TO_CLOUD_UNSUPPORTED: &str =
    "-otel_to_cloud requires OpenTelemetry export, which gadk adds in sub-project 7";

/// The log line written when a global subscriber is already installed.
pub(crate) const KEPT_EXISTING_SUBSCRIBER: &str = "a global tracing subscriber is already installed; the launcher keeps it and does not install its telemetry layers (launcher.Config.TelemetryOptions)";

/// Go `InitAndSetGlobalOtelProviders`: installs a global `tracing` subscriber built from
/// `config.telemetry_layers` (drained); keeps an existing global subscriber and logs through
/// `config.log` that the launcher's layers were not installed. With no layers nothing is
/// installed (an empty subscriber records nothing, like Go's providers without exporters).
/// `otel_to_cloud` → `Err(Error::Telemetry("-otel_to_cloud requires OpenTelemetry export, which
/// gadk adds in sub-project 7"))`, leaving `config.telemetry_layers` untouched. Callers wrap
/// errors as `telemetry initialization failed: <err>`.
pub fn init(config: &mut Config, otel_to_cloud: bool) -> Result<TelemetryGuard, Error> {
    init_with(config, otel_to_cloud, |dispatch| {
        tracing::dispatcher::set_global_default(dispatch).is_ok()
    })
}

/// [`init`] with the global installation injected: `install` returns `false` when a global
/// subscriber is already set (tests never touch the process-global dispatcher).
pub(crate) fn init_with(
    config: &mut Config,
    otel_to_cloud: bool,
    install: impl FnOnce(Dispatch) -> bool,
) -> Result<TelemetryGuard, Error> {
    let Some(dispatch) = build(config, otel_to_cloud)? else {
        return Ok(TelemetryGuard { _private: () });
    };
    if !install(dispatch) {
        log_kept_existing(&config.log);
    }
    Ok(TelemetryGuard { _private: () })
}

/// Builds the launcher's subscriber from the drained `config.telemetry_layers`; `None` when there
/// are no layers. Tests and in-process servers can install the result with
/// `tracing::dispatcher::set_default` instead of globally.
pub(crate) fn build(config: &mut Config, otel_to_cloud: bool) -> Result<Option<Dispatch>, Error> {
    if otel_to_cloud {
        return Err(Error::Telemetry(OTEL_TO_CLOUD_UNSUPPORTED.to_owned()));
    }
    if config.telemetry_layers.is_empty() {
        return Ok(None);
    }
    let layers = std::mem::take(&mut config.telemetry_layers);
    Ok(Some(Dispatch::new(Registry::default().with(layers))))
}

fn log_kept_existing(log: &Logger) {
    log.print(KEPT_EXISTING_SUBSCRIBER);
}

/// Go `*telemetry.Providers`, returned by [`init`].
#[derive(Debug)]
pub struct TelemetryGuard {
    _private: (),
}

impl TelemetryGuard {
    /// Go `Providers.Shutdown`; nothing to flush in sub-project 6.
    pub fn shutdown(self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pretty_assertions::assert_eq;
    use tracing_subscriber::Layer;

    use super::*;
    use crate::config::BoxedLayer;
    use crate::golog;

    struct NoAgents;

    impl gadk_agent::Loader for NoAgents {
        fn list_agents(&self) -> Vec<String> {
            Vec::new()
        }
        fn load_agent(
            &self,
            name: &str,
        ) -> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::LoaderError> {
            Err(gadk_agent::LoaderError(format!("no agent {name}")))
        }
        fn root_agent(&self) -> Arc<dyn gadk_agent::Agent> {
            unimplemented!("the telemetry tests load no agent")
        }
    }

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

    fn config_with_log() -> (Config, golog::Captured) {
        let (log, out) = golog::capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(|| "2009/11/10 23:00:00 ".to_owned());
        (config, out)
    }

    #[test]
    fn the_installed_subscriber_runs_every_layer() {
        let (mut config, out) = config_with_log();
        let (a, seen_a) = counting();
        let (b, seen_b) = counting();
        config.telemetry_layers = vec![a, b];
        let mut installed = None;
        init_with(&mut config, false, |d| {
            installed = Some(d);
            true
        })
        .unwrap()
        .shutdown()
        .unwrap();
        assert!(config.telemetry_layers.is_empty(), "the layers are taken");
        let dispatch = installed.unwrap();
        tracing::dispatcher::with_default(&dispatch, || tracing::info!("one event"));
        assert_eq!(seen_a.load(Ordering::SeqCst), 1);
        assert_eq!(seen_b.load(Ordering::SeqCst), 1);
        assert_eq!(out.contents(), "");
    }

    #[test]
    fn an_existing_global_subscriber_is_kept_and_logged() {
        let (mut config, out) = config_with_log();
        let (layer, _) = counting();
        config.telemetry_layers = vec![layer];
        init_with(&mut config, false, |_| false).unwrap();
        assert_eq!(
            out.contents(),
            format!("2009/11/10 23:00:00 {KEPT_EXISTING_SUBSCRIBER}\n")
        );
    }

    #[test]
    fn no_layers_install_nothing() {
        let (mut config, out) = config_with_log();
        let mut called = false;
        init_with(&mut config, false, |_| {
            called = true;
            true
        })
        .unwrap();
        assert!(!called);
        assert_eq!(out.contents(), "");
    }

    #[test]
    fn otel_to_cloud_is_the_sub_project_7_error() {
        let (mut config, _) = config_with_log();
        let err = init_with(&mut config, true, |_| true).unwrap_err();
        assert!(matches!(err, Error::Telemetry(_)));
        assert_eq!(
            err.to_string(),
            "-otel_to_cloud requires OpenTelemetry export, which gadk adds in sub-project 7"
        );
        assert_eq!(
            format!("telemetry initialization failed: {err}"),
            "telemetry initialization failed: -otel_to_cloud requires OpenTelemetry export, which gadk adds in sub-project 7"
        );
    }
}
