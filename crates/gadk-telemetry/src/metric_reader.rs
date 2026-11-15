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

//! The periodic metric reader telemetry setup builds (port of ADK-Go
//! `telemetry/metric_reader.go`): an SDK [`PeriodicReader`] that doesn't export on shutdown and
//! exports nothing while no instrument has recorded.

use std::fmt;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{InstrumentKind, PeriodicReader, Pipeline, Temporality};

/// Go `newPeriodicReader`: a reader over an SDK [`PeriodicReader`] which doesn't export on
/// shutdown, and which exports nothing while no instrument has recorded. `interval` is Go's
/// `sdkmetric.WithInterval`; `None` keeps the SDK's (`OTEL_METRIC_EXPORT_INTERVAL`, else 60 s).
pub(crate) fn new_periodic_reader<E: PushMetricExporter>(
    exporter: E,
    interval: Option<Duration>,
) -> NoFinalExportReader<E> {
    let drop_exports = Arc::new(AtomicBool::new(false));
    let guarded = GuardedExporter {
        exporter,
        drop_exports: Arc::clone(&drop_exports),
    };
    // Divergence (ledger 7a): without `interval`, opentelemetry_sdk reads
    // `OTEL_METRIC_EXPORT_INTERVAL` from the process environment, not the injected one.
    let mut builder = PeriodicReader::builder(guarded);
    if let Some(interval) = interval {
        builder = builder.with_interval(interval);
    }
    #[cfg(test)]
    PERIODIC_READERS_BUILT.with(|n| n.set(n.get() + 1));
    NoFinalExportReader {
        reader: builder.build(),
        drop_exports,
        interval,
    }
}

#[cfg(test)]
thread_local! {
    /// How many readers [`new_periodic_reader`] built on this thread (each runs its own thread
    /// from construction): `TestNewErrorLeavesNoReaderRunning` checks a failed setup built none.
    pub(crate) static PERIODIC_READERS_BUILT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Go `noFinalExportReader`: wraps the SDK [`PeriodicReader`] so it doesn't export on shutdown.
/// Every [`MetricReader`] method delegates to it; the shutdowns set the guarded exporter's flag
/// first. The exporter is shared through an `Arc<AtomicBool>` because
/// [`PeriodicReader::builder`] takes ownership of it.
pub(crate) struct NoFinalExportReader<E: PushMetricExporter> {
    reader: PeriodicReader<GuardedExporter<E>>,
    drop_exports: Arc<AtomicBool>,
    interval: Option<Duration>,
}

/// The reader's logging view (Go `PeriodicReader.MarshalLog`), the only place its interval shows.
impl<E: PushMetricExporter> fmt::Debug for NoFinalExportReader<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoFinalExportReader")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl<E: PushMetricExporter> MetricReader for NoFinalExportReader<E> {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.reader.register_pipeline(pipeline);
    }

    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.reader.collect(rm)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.reader.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.drop_exports.store(true, Ordering::SeqCst);
        self.reader.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.drop_exports.store(true, Ordering::SeqCst);
        self.reader.shutdown()
    }

    fn temporality(&self, kind: InstrumentKind) -> Temporality {
        self.reader.temporality(kind)
    }
}

/// Go `guardedExporter`: a wrapper over a [`PushMetricExporter`] that discards an empty
/// collection, and every collection once `drop_exports` is set.
pub(crate) struct GuardedExporter<E> {
    exporter: E,
    drop_exports: Arc<AtomicBool>,
}

impl<E: PushMetricExporter> PushMetricExporter for GuardedExporter<E> {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        if self.drop_exports.load(Ordering::SeqCst) || metrics.scope_metrics().next().is_none() {
            return Ok(());
        }
        self.exporter.export(metrics).await
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.exporter.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.exporter.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.exporter.shutdown()
    }

    fn temporality(&self) -> Temporality {
        self.exporter.temporality()
    }
}
