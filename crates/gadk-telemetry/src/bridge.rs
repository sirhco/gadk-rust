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

//! [`OtelBridge`]: the `tracing` layer that exports gadk's ADK spans (the 5a D6 layout) and log
//! records through the OpenTelemetry SDK providers.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Exports every ADK span (a `tracing` span declaring `otel.name`) as an SDK span on the tracer
/// provider, parented under its nearest ADK ancestor, and every log event on
/// `gadk_engine::telemetry::LOG_TARGET` as an SDK log record on the logger provider. With
/// neither provider it records nothing. Install it before any other layer that reads the
/// `SpanIds` extension (`Registry::default().with(bridge).with(…)`).
#[derive(Debug)]
pub struct OtelBridge {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl OtelBridge {
    /// A bridge exporting through `tracer_provider` and `logger_provider`.
    pub fn new(
        tracer_provider: Option<&SdkTracerProvider>,
        logger_provider: Option<&SdkLoggerProvider>,
    ) -> Self {
        todo!()
    }
}

impl<S> Layer<S> for OtelBridge
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        todo!()
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        todo!()
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        todo!()
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        todo!()
    }
}
