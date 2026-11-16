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
