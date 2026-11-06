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

//! Port of ADK-Go `internal/compactioninternal/telemetry_test.go`.
//!
//! Spans are captured with `capture_spans` (a string map of fields): Go's FLOAT64 type checks
//! become "parses as an f64 equal to Go's epoch seconds", and Go's `exception` event /
//! `error.type` become the span error fields (`otel.status_code = "ERROR"`, `error.message`,
//! and the error event's `otel.status_description`).

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use gadk_genai::GenerateContentResponseUsageMetadata as Usage;
use gadk_model::Backend;
use gadk_platform::Platform;
use gadk_session::compaction::{Config, SummarizeFailure, SummarizeResult, Summarizer};
use gadk_session::{Event, Session, zero_time};
use pretty_assertions::assert_eq;

use crate::compaction::compactor::sliding_window;
use crate::compaction::test_support::{
    StaticSession, at, fakeSummarizer, model_text_event, sliding_window_stored, text_event,
};
use crate::llm::flow::span_capture::{CapturedSpan, capture_spans};
use crate::telemetry::gen_ai_system;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

fn two_invocations() -> Vec<Event> {
    vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
    ]
}

fn cfg_with(interval: i32, overlap: i32, s: Arc<dyn Summarizer>) -> Config {
    Config {
        compaction_interval: interval,
        overlap_size: overlap,
        summarizer: Some(s),
        ..Default::default()
    }
}

/// Go exp.GetSpans(): the closed compaction spans.
fn compaction_spans(spans: Vec<CapturedSpan>) -> Vec<CapturedSpan> {
    spans
        .into_iter()
        .filter(|s| s.name == "compact_events" && s.closed)
        .collect()
}

fn is_error(s: &CapturedSpan) -> bool {
    s.field("otel.status_code").as_deref() == Some("ERROR")
}

/// Go `float64(t.UnixNano()) / float64(time.Second)`, computed independently of the code under test.
fn go_epoch_seconds(t: chrono::DateTime<chrono::Utc>) -> f64 {
    t.timestamp_nanos_opt().unwrap() as f64 / 1e9
}

fn float_field(s: &CapturedSpan, k: &str) -> Option<f64> {
    s.field(k).map(|v| {
        v.parse::<f64>()
            .unwrap_or_else(|_| panic!("{k} = {v:?} is not a float"))
    })
}

/// Go panickingSummarizer.
#[allow(non_camel_case_types)]
struct panickingSummarizer;

#[async_trait]
impl Summarizer for panickingSummarizer {
    async fn summarize_events(&self, _: &[Event]) -> Result<SummarizeResult, BoxErr> {
        panic!("summarizer exploded")
    }
}

/// Go bothSummarizer. A Rust result cannot carry content and an error together, so it returns
/// the error only (see the adapted annotation on its test).
#[allow(non_camel_case_types)]
struct bothSummarizer;

#[async_trait]
impl Summarizer for bothSummarizer {
    async fn summarize_events(&self, _: &[Event]) -> Result<SummarizeResult, BoxErr> {
        Err("boom".into())
    }
}

/// Go geminiSummarizer: a fakeSummarizer that reports a backend.
#[allow(non_camel_case_types)]
struct geminiSummarizer {
    inner: Arc<fakeSummarizer>,
    backend: Option<Backend>,
}

#[async_trait]
impl Summarizer for geminiSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<SummarizeResult, BoxErr> {
        self.inner.summarize_events(events).await
    }
    fn google_variant(&self) -> Option<Backend> {
        self.backend
    }
}

/// Go usageSummarizer.
#[allow(non_camel_case_types)]
struct usageSummarizer {
    inner: Arc<fakeSummarizer>,
    prompt: i32,
    output: i32,
}

#[async_trait]
impl Summarizer for usageSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<SummarizeResult, BoxErr> {
        let mut res = self.inner.summarize_events(events).await?;
        if res.content.is_none() {
            return Ok(res);
        }
        res.usage = Some(Usage {
            prompt_token_count: self.prompt,
            candidates_token_count: self.output,
            ..Default::default()
        });
        Ok(res)
    }
}

/// Go usageReportingDecliner.
#[allow(non_camel_case_types)]
struct usageReportingDecliner {
    usage: Usage,
}

#[async_trait]
impl Summarizer for usageReportingDecliner {
    async fn summarize_events(&self, _: &[Event]) -> Result<SummarizeResult, BoxErr> {
        Ok(SummarizeResult {
            content: None,
            usage: Some(self.usage.clone()),
        })
    }
}

/// Go usageReportingFailer: fails while reporting what it spent (Go-parity note 4).
#[allow(non_camel_case_types)]
struct usageReportingFailer {
    usage: Usage,
}

#[async_trait]
impl Summarizer for usageReportingFailer {
    async fn summarize_events(&self, _: &[Event]) -> Result<SummarizeResult, BoxErr> {
        Err(Box::new(SummarizeFailure {
            usage: Some(self.usage.clone()),
            source: "summarizer stopped before finishing (finish reason \"MAX_TOKENS\"), so the summary is incomplete".into(),
        }))
    }
}

// parity: adapted: FLOAT64 type checks become "parses as an f64 equal to Go's epoch seconds" (CapturedSpan fields are strings)
#[tokio::test]
async fn test_sliding_window_emits_span() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 1, fakeSummarizer::new("sum"));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    let got = got
        .unwrap()
        .expect("slidingWindowStored() produced no summary");
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.otel_name, "compact_events sliding_window");
    assert!(!is_error(span), "span status = error, want unset");
    for (k, want) in [
        ("gen_ai.operation.name", "compact_events"),
        ("gen_ai.conversation.id", "sess"),
        ("gen_ai.compaction.trigger", "sliding_window"),
        ("gen_ai.compaction.summarizer_type", "fakeSummarizer"),
        ("gen_ai.compaction.result_event_id", got.id.as_str()),
    ] {
        assert_eq!(span.field(k).as_deref(), Some(want), "attribute {k}");
    }
    assert_eq!(
        span.field("gen_ai.compaction.event_count").as_deref(),
        Some("4")
    );
    assert_eq!(
        span.field("gen_ai.compaction.compaction_interval")
            .as_deref(),
        Some("2")
    );
    assert_eq!(
        span.field("gen_ai.compaction.overlap_size").as_deref(),
        Some("1")
    );
    assert!(span.field("gen_ai.compaction.token_threshold").is_none());
    assert_eq!(
        float_field(span, "gen_ai.compaction.start_timestamp"),
        Some(go_epoch_seconds(at(1)))
    );
    assert_eq!(
        float_field(span, "gen_ai.compaction.end_timestamp"),
        Some(go_epoch_seconds(at(4)))
    );
    assert!(!got.id.is_empty(), "result_event_id is empty");
}

// parity: adapted: the OTel exception event and error.type become otel.status_code = "ERROR" and error.message
#[tokio::test]
async fn test_compaction_span_marks_a_panic() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, Arc::new(panickingSummarizer));
    let (outcome, spans) = capture_spans(
        AssertUnwindSafe(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).catch_unwind(),
    )
    .await;
    assert!(
        outcome.is_err(),
        "the panic did not propagate; compaction must not swallow it"
    );
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(
        is_error(&spans[0]),
        "a panicking summarizer must not look healthy"
    );
    assert_eq!(
        spans[0].field("error.message").as_deref(),
        Some("summarizer panicked: summarizer exploded"),
        "the failed span has no error"
    );
}

// parity: adapted: Go checks the span carries no FLOAT64 error.type; Rust checks the usage fields as strings and the error fields of the failed span
#[tokio::test]
async fn test_failed_summarization_reports_what_it_spent() {
    let sess = StaticSession::new(two_invocations());
    let usage = Usage {
        prompt_token_count: 4000,
        candidates_token_count: 8,
        ..Default::default()
    };
    let cfg = cfg_with(2, 1, Arc::new(usageReportingFailer { usage }));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    assert!(
        got.is_err(),
        "slidingWindowStored() error = None, want the summarizer's failure"
    );
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(
        spans[0].field("gen_ai.usage.input_tokens").is_some(),
        "no input tokens for a failure that reported usage"
    );
    assert!(
        spans[0].field("gen_ai.usage.output_tokens").is_some(),
        "no output tokens for a failure that reported usage"
    );
    assert!(
        spans[0]
            .field("gen_ai.compaction.result_event_id")
            .is_none(),
        "a failed compaction named a result event"
    );
}

// parity: adapted: the attribute key set is compared with the otel.* keys excluded (tracing's otel.name carries Go's span name)
#[tokio::test]
async fn test_compaction_span_attribute_key_set() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 1, fakeSummarizer::new("SUM"));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    got.unwrap();
    let spans = compaction_spans(spans);
    let mut keys: Vec<&str> = spans[0]
        .fields
        .keys()
        .map(String::as_str)
        .filter(|k| !k.starts_with("otel."))
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "gcp.vertex.agent.invocation_id",
            "gen_ai.compaction.compaction_interval",
            "gen_ai.compaction.end_timestamp",
            "gen_ai.compaction.event_count",
            "gen_ai.compaction.overlap_size",
            "gen_ai.compaction.result_event_id",
            "gen_ai.compaction.start_timestamp",
            "gen_ai.compaction.summarizer_type",
            "gen_ai.compaction.trigger",
            "gen_ai.conversation.id",
            "gen_ai.operation.name",
        ],
        "these keys are shared with adk-python; change them together"
    );
}

#[tokio::test]
async fn test_compaction_span_records_gen_ai_system() {
    let cases = [
        ("vertex ai", Some(Backend::VertexAi), Some("gcp.vertex_ai")),
        ("gemini api", Some(Backend::GeminiApi), Some("gcp.gemini")),
        ("summarizer that does not say", None, None),
    ];
    for (name, backend, want) in cases {
        // The expectation comes from the shared mapping, so this table tracks it.
        assert_eq!(
            gen_ai_system(backend),
            want,
            "{name}: the shared mapping changed; this table is stale"
        );
        let sess = StaticSession::new(two_invocations());
        let cfg = cfg_with(
            2,
            0,
            Arc::new(geminiSummarizer {
                inner: fakeSummarizer::new("SUM"),
                backend,
            }),
        );
        let (got, spans) =
            capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
        got.unwrap();
        let spans = compaction_spans(spans);
        assert_eq!(spans.len(), 1, "{name}");
        assert_eq!(spans[0].field("gen_ai.system").as_deref(), want, "{name}");
    }
}

// parity: adapted: the OTel exception event becomes the error event's otel.status_description on the captured span
#[tokio::test]
async fn test_compaction_span_records_failure() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, fakeSummarizer::failing("boom"));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    assert!(
        got.is_err(),
        "slidingWindowStored() succeeded, want an error"
    );
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(is_error(&spans[0]), "span status is not error");
    assert!(
        spans[0].field("otel.status_description").is_some(),
        "span records no exception event, so the failure reason is lost"
    );
    for key in [
        "gen_ai.compaction.result_event_id",
        "gen_ai.compaction.start_timestamp",
        "gen_ai.compaction.end_timestamp",
    ] {
        assert!(
            spans[0].field(key).is_none(),
            "{key} is present on a failed compaction span, want it omitted"
        );
    }
}

#[tokio::test]
async fn test_no_span_when_nothing_to_compact() {
    let sess = StaticSession::new(vec![text_event("a", "inv1", 1, "q1")]);
    let cfg = cfg_with(5, 0, fakeSummarizer::new("sum"));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    let got = got.unwrap();
    assert!(got.is_none(), "slidingWindowStored() = {got:?}, want None");
    let n = spans.iter().filter(|s| s.name == "compact_events").count();
    assert_eq!(
        n, 0,
        "got {n} spans when the interval was not reached, want 0"
    );
}

#[tokio::test]
async fn test_span_records_declining_summarizer() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, fakeSummarizer::new(""));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    got.unwrap();
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(
        !is_error(&spans[0]),
        "span status = error, want success for a summarizer that merely declined"
    );
    assert!(
        spans[0]
            .field("gen_ai.compaction.result_event_id")
            .is_none(),
        "result_event_id is set although no summary was produced"
    );
}

// parity: adapted: a Rust Summarizer cannot return content and an error together, so bothSummarizer returns the error only; the failed-span assertions are Go's
#[tokio::test]
async fn test_compaction_span_omits_result_when_summarizer_also_errors() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, Arc::new(bothSummarizer));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    assert!(
        got.is_err(),
        "slidingWindowStored() succeeded, want an error"
    );
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(is_error(&spans[0]), "span status is not error");
    for key in [
        "gen_ai.compaction.result_event_id",
        "gen_ai.compaction.start_timestamp",
        "gen_ai.compaction.end_timestamp",
    ] {
        assert!(
            spans[0].field(key).is_none(),
            "{key} = {:?} on a failed compaction span, want it omitted",
            spans[0].field(key)
        );
    }
}

#[tokio::test]
async fn test_compaction_span_carries_invocation_and_usage() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(
        2,
        0,
        Arc::new(usageSummarizer {
            inner: fakeSummarizer::new("SUM"),
            prompt: 1234,
            output: 56,
        }),
    );
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    got.unwrap();
    let spans = compaction_spans(spans);
    let a = &spans[0];
    assert_eq!(
        a.field("gcp.vertex.agent.invocation_id").as_deref(),
        Some("inv2"),
        "invocation_id, want the turn that triggered compaction"
    );
    assert_eq!(
        a.field("gen_ai.usage.input_tokens").as_deref(),
        Some("1234")
    );
    assert_eq!(a.field("gen_ai.usage.output_tokens").as_deref(), Some("56"));
}

#[tokio::test]
async fn test_compaction_span_omits_absent_timestamps() {
    // Only the oldest event is unstamped, so the window still has a real upper bound.
    let mut first = text_event("a", "inv1", 1, "q1");
    first.timestamp = zero_time();
    let sess = StaticSession::new(vec![
        first,
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
    ]);
    let cfg = cfg_with(2, 0, fakeSummarizer::new("SUM"));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    got.unwrap();
    let spans = compaction_spans(spans);
    let a = &spans[0];
    assert!(
        a.field("gen_ai.compaction.start_timestamp").is_none(),
        "start_timestamp = {:?}, want the key to be absent for an unset bound",
        a.field("gen_ai.compaction.start_timestamp")
    );
    assert!(
        a.field("gen_ai.compaction.end_timestamp").is_some(),
        "end_timestamp is missing, want the bound that was recorded"
    );
}

#[tokio::test]
async fn test_compaction_span_reports_a_discarded_summary() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, fakeSummarizer::new("SUM"));
    let ((), spans) = capture_spans(async {
        let (summary, finish) =
            sliding_window(&Platform::default(), &cfg, Some(&*sess as &dyn Session), "")
                .await
                .unwrap();
        assert!(summary.is_some(), "SlidingWindow() produced no summary");
        // The caller decides not to keep it.
        finish.finish(
            None,
            "another compaction covering the same events landed while summarizing",
        );
    })
    .await;
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    let declined = spans[0]
        .field("gen_ai.compaction.declined")
        .expect("the span does not say the summary was discarded");
    assert!(!declined.is_empty(), "the discard reason is empty");
    assert!(
        spans[0]
            .field("gen_ai.compaction.result_event_id")
            .is_none(),
        "the span names a result event, but nothing reached the session"
    );
}

// parity: adapted: the OTel exception event becomes the error event recorded on the span (otel.status_description)
#[tokio::test]
async fn test_compaction_span_records_a_panic_as_an_exception() {
    let sess = StaticSession::new(two_invocations());
    let cfg = cfg_with(2, 0, Arc::new(panickingSummarizer));
    let (_, spans) = capture_spans(
        AssertUnwindSafe(sliding_window(
            &Platform::default(),
            &cfg,
            Some(&*sess as &dyn Session),
            "",
        ))
        .catch_unwind(),
    )
    .await;
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].field("otel.status_description").as_deref(),
        Some("summarizer panicked: summarizer exploded"),
        "no exception event was recorded"
    );
}

#[tokio::test]
async fn test_declined_summarization_reports_what_it_spent() {
    let sess = StaticSession::new(two_invocations());
    let usage = Usage {
        prompt_token_count: 120,
        candidates_token_count: 15,
        ..Default::default()
    };
    let cfg = cfg_with(2, 1, Arc::new(usageReportingDecliner { usage }));
    let (got, spans) =
        capture_spans(sliding_window_stored(&cfg, Some(&*sess as &dyn Session))).await;
    got.unwrap();
    let spans = compaction_spans(spans);
    assert_eq!(spans.len(), 1);
    assert!(
        spans[0].field("gen_ai.compaction.declined").is_some(),
        "a decline left no marker on the span"
    );
    assert!(
        spans[0].field("gen_ai.usage.input_tokens").is_some(),
        "the span records no input tokens for a decline that reported usage"
    );
    assert!(
        spans[0].field("gen_ai.usage.output_tokens").is_some(),
        "the span records no output tokens for a decline that reported usage"
    );
}

// TestTailRetentionEmitsSpan, TestCompactionSpanRecordsTailRetentionThresholds and
// TestCompactionSpanRecordsADecline need tail retention; Task 8 appends them here.
