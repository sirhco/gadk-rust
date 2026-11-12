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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_platform::env::{EnvProvider, ProcessEnv};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;

use crate::bridge::OtelBridge;
use crate::config::TelemetryOption;
use crate::error::Error;
use crate::gcp_detector::{self, ResourceDetector};

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
        todo!()
    }

    /// Go `SetGlobalOtelProviders`: sets OpenTelemetry's global tracer provider when one is
    /// configured. OpenTelemetry Rust has no global logger provider.
    pub fn set_global_otel_providers(&self) {
        todo!()
    }

    /// [`Providers::set_global_otel_providers`] with the global setter injected (tests).
    pub fn set_global_otel_providers_with(
        &self,
        set_tracer_provider: impl FnOnce(SdkTracerProvider),
    ) {
        todo!()
    }

    /// The `tracing` layer exporting gadk's ADK spans and log records through these providers.
    pub fn layer(&self) -> OtelBridge {
        todo!()
    }
}

/// Go `telemetry.New`: configures the providers from `opts` and the process environment.
///
/// # Errors
///
/// Go's setup error texts (credentials, projects, resource, exporters).
pub async fn new(opts: &[TelemetryOption]) -> Result<Providers, Error> {
    todo!()
}

/// [`new`] with the environment and the GCP resource detector injected.
pub(crate) async fn new_with(
    env: Arc<dyn EnvProvider>,
    detector: &dyn ResourceDetector,
    opts: &[TelemetryOption],
) -> Result<Providers, Error> {
    todo!()
}
