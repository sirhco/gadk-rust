// Copyright 2026 Chris Olson
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

//! Test-only builders for SDK log records (the SDK's `SdkLogRecord::new` is crate-private).

use opentelemetry::InstrumentationScope;
use opentelemetry::logs::{Logger as _, LoggerProvider as _};
use opentelemetry_sdk::logs::{
    InMemoryLogExporter, SdkLogRecord, SdkLoggerProvider, SimpleLogProcessor,
};
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt as _};

/// One record built by `build` and emitted through a real provider.
pub(crate) fn record(
    build: impl FnOnce(&mut SdkLogRecord),
) -> (SdkLogRecord, InstrumentationScope) {
    let exporter = InMemoryLogExporter::default();
    let provider = SdkLoggerProvider::builder()
        .with_log_processor(SimpleLogProcessor::new(exporter.clone()))
        .build();
    let logger = provider.logger("gadk-test");
    let mut r = logger.create_log_record();
    build(&mut r);
    logger.emit(r);
    let mut logs = exporter.get_emitted_logs().unwrap();
    let d = logs.remove(0);
    (d.record, d.instrumentation)
}

/// The borrowed pairs `LogBatch::new` takes.
pub(crate) fn refs(
    records: &[(SdkLogRecord, InstrumentationScope)],
) -> Vec<(&SdkLogRecord, &InstrumentationScope)> {
    records.iter().map(|(r, s)| (r, s)).collect()
}

/// Collects the messages of WARN events on [`super::WARN_TARGET`].
#[derive(Clone, Default)]
pub(crate) struct Warnings(Arc<Mutex<Vec<String>>>);

impl Warnings {
    /// Installs a collecting subscriber as this thread's default until the guard drops (a
    /// current-thread Tokio test keeps every poll on this thread).
    ///
    /// A no-op global `Registry` is installed once first: without a global subscriber, other
    /// tests' thread-default subscribers coming and going on parallel threads can leave the
    /// warning callsite's cached interest (and the global max-level hint) at "off", so this
    /// thread's subscriber never sees the event.
    pub(crate) fn capture(&self) -> tracing::subscriber::DefaultGuard {
        static PIN: std::sync::Once = std::sync::Once::new();
        PIN.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
        tracing::subscriber::set_default(tracing_subscriber::registry().with(self.clone()))
    }

    /// The messages collected so far, draining them.
    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock())
    }
}

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Warnings {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if meta.target() == super::WARN_TARGET && *meta.level() == tracing::Level::WARN {
            let mut m = Message(String::new());
            event.record(&mut m);
            self.0.lock().push(m.0);
        }
    }
}
