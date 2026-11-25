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

//! The `gadk_console::traces` names of the shared types are the `gadk_analytics` types: a value
//! of each binds to the other's name.

use gadk_console::traces::{BigQueryTable, Service, SourceError, TimeRange, TraceId};

#[test]
fn the_console_trace_types_are_the_analytics_types() {
    let table: gadk_analytics::BigQueryTable = BigQueryTable::new("p", "d", "t");
    let table: BigQueryTable = table;
    assert_eq!(table, gadk_analytics::BigQueryTable::new("p", "d", "t"));

    let err: gadk_analytics::SourceError = SourceError::NotFound;
    let err: SourceError = err;
    assert_eq!(err, gadk_analytics::SourceError::NotFound);

    let service: gadk_analytics::Service = Service::Logging;
    let service: Service = service;
    assert_eq!(service, gadk_analytics::Service::Logging);

    let range: gadk_analytics::TimeRange = TimeRange::Days7;
    let range: TimeRange = range;
    assert_eq!(range, gadk_analytics::TimeRange::Days7);

    let id: gadk_analytics::TraceId = TraceId::parse("0123456789abcdef0123456789abcdef").unwrap();
    let id: TraceId = id;
    assert_eq!(id.as_str(), "0123456789abcdef0123456789abcdef");
}
