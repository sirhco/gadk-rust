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

//! Ports of ADK-Go `internal/telemetry/telemetrytest/span_digest_test.go`.

use rstest::rstest;

use crate::telemetry::log_digest::LogRecord;
use crate::telemetry::span_digest::{SpanStub, build_digest};

/// Go's `span` helper: a span with ID `id` under `parent` (0 = none).
fn span(name: &str, id: u64, parent: u64) -> SpanStub {
    SpanStub {
        name: name.to_owned(),
        span_id: id,
        parent_span_id: (parent != 0).then_some(parent),
        status: "UNSET".to_owned(),
        ..Default::default()
    }
}

/// Go's `logIn` helper: a record named `event` emitted under span `span_id`.
fn log_in(span_id: u64) -> LogRecord {
    LogRecord {
        event_name: "event".to_owned(),
        span_id: Some(span_id),
        ..Default::default()
    }
}

#[rstest]
#[case::log_under_a_span(vec![span("root", 1, 0), span("child", 2, 1)], vec![log_in(2)], false)]
#[case::log_outside_any_span(vec![span("root", 1, 0)], vec![log_in(9)], true)]
#[case::no_root_span(vec![], vec![], true)]
#[case::two_root_spans(vec![span("a", 1, 0), span("b", 2, 0)], vec![], true)]
fn test_build_digest(
    #[case] spans: Vec<SpanStub>,
    #[case] logs: Vec<LogRecord>,
    #[case] want_err: bool,
) {
    let got = build_digest(&spans, &logs);
    assert_eq!(got.is_err(), want_err, "build_digest() = {got:?}");
}

/// The digest's shape and Go's error texts (not in the Go test, which checks only that an error
/// occurs).
#[test]
fn build_digest_links_children_and_logs_and_reports_go_errors() {
    let d = build_digest(&[span("root", 1, 0), span("child", 2, 1)], &[log_in(2)]).unwrap();
    assert_eq!(d.root_span.name, "root");
    assert_eq!(d.root_span.children.len(), 1);
    assert_eq!(d.root_span.children[0].logs.len(), 1);
    assert_eq!(d.root_span.children[0].logs[0].event_name, "event");
    assert_eq!(
        build_digest(&[span("root", 1, 0)], &[log_in(9)]).unwrap_err(),
        "log record \"event\" was emitted outside any collected span"
    );
    assert_eq!(
        build_digest(&[], &[]).unwrap_err(),
        "expected exactly 1 root span, got 0"
    );
}
