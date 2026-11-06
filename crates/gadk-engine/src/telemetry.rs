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

//! `tracing` span helpers (port of the span names in Go `internal/telemetry/node_tracing.go` and
//! `telemetry.go`). No OpenTelemetry: the static span name is Go's operation and `otel.name` is Go's
//! full span name. All spans are `INFO`. The node, model and tool spans declare `otel.status_code` and
//! `error.message` as empty fields; the code that ends the span records them on error.

use chrono::{DateTime, Utc};
use gadk_genai::GenerateContentResponseUsageMetadata;
use gadk_model::Backend;
use gadk_session::{Event, zero_time};

/// Port of Go internal/telemetry startInvokeAgentSpan (name only). Engine-internal: agents' own spans
/// come from `CustomAgent::run` (gadk-agent), and engine code never wraps `Agent::run` in another one,
/// so only the span unit tests use it.
#[cfg(test)]
pub(crate) fn agent_span(agent_name: &str) -> tracing::Span {
    tracing::info_span!(
        "invoke_agent",
        otel.name = %format!("invoke_agent {agent_name}"),
        gen_ai.operation.name = "invoke_agent",
        gen_ai.agent.name = %agent_name
    )
}

/// Port of Go startInvokeWorkflowSpan (name only).
pub(crate) fn workflow_span(name: &str) -> tracing::Span {
    tracing::info_span!(
        "invoke_workflow",
        otel.name = %format!("invoke_workflow {name}"),
        gen_ai.operation.name = "invoke_workflow",
        gen_ai.workflow.name = %name
    )
}

/// Port of Go startInvokeNodeSpan; `gen_ai.node.path` is a Rust addition (the
/// `<parent>/<child>@<run_id>` path).
pub(crate) fn node_span(node_name: &str, path: &str) -> tracing::Span {
    tracing::info_span!(
        "invoke_node",
        otel.name = %format!("invoke_node {node_name}"),
        gen_ai.operation.name = "invoke_node",
        gen_ai.node.name = %node_name,
        gen_ai.node.path = %path,
        otel.status_code = tracing::field::Empty,
        error.message = tracing::field::Empty
    )
}

/// Port of Go startGenerateContentSpan (name and model only).
pub(crate) fn generate_content_span(model: &str) -> tracing::Span {
    tracing::info_span!(
        "generate_content",
        otel.name = %format!("generate_content {model}"),
        gen_ai.operation.name = "generate_content",
        gen_ai.request.model = %model,
        otel.status_code = tracing::field::Empty,
        error.message = tracing::field::Empty
    )
}

/// Port of Go StartExecuteToolSpan (name only).
pub(crate) fn execute_tool_span(tool_name: &str) -> tracing::Span {
    tracing::info_span!(
        "execute_tool",
        otel.name = %format!("execute_tool {tool_name}"),
        gen_ai.operation.name = "execute_tool",
        gen_ai.tool.name = %tool_name,
        otel.status_code = tracing::field::Empty,
        error.message = tracing::field::Empty
    )
}

/// Go internal/telemetry/compaction.go compactEventsName.
const COMPACT_EVENTS: &str = "compact_events";

/// Go telemetry.CompactionTriggerSlidingWindow.
pub(crate) const COMPACTION_TRIGGER_SLIDING_WINDOW: &str = "sliding_window";
/// Go telemetry.CompactionTriggerTokenThreshold.
pub(crate) const COMPACTION_TRIGGER_TOKEN_THRESHOLD: &str = "token_threshold";

/// Port of Go telemetry.StartCompactEventsSpanParams. Zero thresholds mean "strategy disabled"
/// and their attributes are omitted.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompactEventsSpanParams {
    pub(crate) trigger: &'static str,
    pub(crate) session_id: String,
    pub(crate) invocation_id: String,
    pub(crate) summarizer_type: String,
    pub(crate) backend: Option<Backend>,
    pub(crate) event_count: usize,
    pub(crate) compaction_interval: i32,
    pub(crate) overlap_size: i32,
    pub(crate) token_threshold: i32,
    pub(crate) event_retention_size: i32,
}

/// Port of Go telemetry.GenAISystemAttr: `gcp.vertex_ai` / `gcp.gemini` (semconv v1.36), `None`
/// for a backend that cannot be named.
pub(crate) fn gen_ai_system(backend: Option<Backend>) -> Option<&'static str> {
    match backend {
        Some(Backend::VertexAi) => Some("gcp.vertex_ai"),
        Some(Backend::GeminiApi) => Some("gcp.gemini"),
        None => None,
    }
}

/// Port of Go epochSeconds: `float64(t.UnixNano()) / float64(time.Second)`.
pub(crate) fn epoch_seconds(t: DateTime<Utc>) -> f64 {
    match t.timestamp_nanos_opt() {
        Some(n) => n as f64 / 1e9,
        // Outside UnixNano's range (Go's result is undefined there); never reached for a
        // recorded bound, which is omitted when zero.
        None => t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9,
    }
}

/// Port of Go StartCompactEventsSpan: a span named `compact_events` (`otel.name`
/// `compact_events <trigger>`), a child of the caller's current span.
pub(crate) fn compact_events_span(p: &CompactEventsSpanParams) -> tracing::Span {
    use tracing::field::Empty;
    let span = tracing::info_span!(
        "compact_events",
        otel.name = %format!("{COMPACT_EVENTS} {}", p.trigger),
        gen_ai.operation.name = COMPACT_EVENTS,
        gen_ai.conversation.id = p.session_id.as_str(),
        gen_ai.compaction.trigger = p.trigger,
        gen_ai.compaction.summarizer_type = p.summarizer_type.as_str(),
        gen_ai.compaction.event_count = u64::try_from(p.event_count).unwrap_or(u64::MAX),
        gcp.vertex.agent.invocation_id = Empty,
        gen_ai.system = Empty,
        gen_ai.compaction.compaction_interval = Empty,
        gen_ai.compaction.overlap_size = Empty,
        gen_ai.compaction.token_threshold = Empty,
        gen_ai.compaction.event_retention_size = Empty,
        gen_ai.compaction.declined = Empty,
        gen_ai.compaction.result_event_id = Empty,
        gen_ai.compaction.start_timestamp = Empty,
        gen_ai.compaction.end_timestamp = Empty,
        gen_ai.usage.input_tokens = Empty,
        gen_ai.usage.output_tokens = Empty,
        otel.status_code = Empty,
        error.message = Empty
    );
    if !p.invocation_id.is_empty() {
        span.record("gcp.vertex.agent.invocation_id", p.invocation_id.as_str());
    }
    if let Some(sys) = gen_ai_system(p.backend) {
        span.record("gen_ai.system", sys);
    }
    if p.compaction_interval > 0 {
        span.record(
            "gen_ai.compaction.compaction_interval",
            i64::from(p.compaction_interval),
        );
        span.record("gen_ai.compaction.overlap_size", i64::from(p.overlap_size));
    }
    if p.token_threshold > 0 {
        span.record(
            "gen_ai.compaction.token_threshold",
            i64::from(p.token_threshold),
        );
        span.record(
            "gen_ai.compaction.event_retention_size",
            i64::from(p.event_retention_size),
        );
    }
    span
}

/// Port of Go TraceCompactionResultParams.
pub(crate) struct CompactionResultParams<'a> {
    pub(crate) result_event: Option<&'a Event>,
    pub(crate) error: Option<&'a (dyn std::error::Error + 'static)>,
    pub(crate) discard_reason: &'a str,
    pub(crate) declined: bool,
    pub(crate) usage: Option<&'a GenerateContentResponseUsageMetadata>,
}

/// Port of Go RecordError (non-legacy schema), in the 1b span form: the declared status fields
/// plus an error event (the OTel `exception` event's counterpart).
pub(crate) fn record_span_error(span: &tracing::Span, err: &dyn std::fmt::Display) {
    span.record("otel.status_code", "ERROR");
    span.record("error.message", tracing::field::display(err));
    tracing::error!(
        parent: span,
        otel.status_code = "ERROR",
        otel.status_description = %err,
        "compact_events failed"
    );
}

/// Port of Go TraceCompactionResult.
pub(crate) fn trace_compaction_result(span: &tracing::Span, p: CompactionResultParams<'_>) {
    if let Some(e) = p.error {
        record_span_error(span, e);
    }
    if p.declined && p.discard_reason.is_empty() {
        span.record(
            "gen_ai.compaction.declined",
            "the summarizer returned no content",
        );
    }
    if !p.discard_reason.is_empty() {
        span.record("gen_ai.compaction.declined", p.discard_reason);
    }
    // Usage before anything conditional: the spend is not conditional on success.
    if let Some(u) = p.usage {
        record_compaction_usage(span, u);
    }
    if p.error.is_some() {
        return;
    }
    let Some(ev) = p.result_event else { return };
    let Some(rng) = &ev.actions.compaction else {
        return;
    };
    if let Some(u) = &ev.response.usage_metadata {
        record_compaction_usage(span, u);
    }
    span.record("gen_ai.compaction.result_event_id", ev.id.as_str());
    // A zero time means "no bound recorded": omit the key rather than report the year 1754.
    if rng.start_timestamp != zero_time() {
        span.record(
            "gen_ai.compaction.start_timestamp",
            epoch_seconds(rng.start_timestamp),
        );
    }
    if rng.end_timestamp != zero_time() {
        span.record(
            "gen_ai.compaction.end_timestamp",
            epoch_seconds(rng.end_timestamp),
        );
    }
}

/// Port of Go TraceCompactionDeclined: a closed span with the same attributes plus the reason.
pub(crate) fn trace_compaction_declined(p: &CompactEventsSpanParams, reason: &str) {
    let span = compact_events_span(p);
    span.record("gen_ai.compaction.declined", reason);
}

/// Port of Go recordCompactionUsage: output tokens include thoughts, per semconv.
fn record_compaction_usage(span: &tracing::Span, u: &GenerateContentResponseUsageMetadata) {
    if u.prompt_token_count > 0 {
        span.record("gen_ai.usage.input_tokens", i64::from(u.prompt_token_count));
    }
    let out = i64::from(u.candidates_token_count) + i64::from(u.thoughts_token_count);
    if out > 0 {
        span.record("gen_ai.usage.output_tokens", out);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id};
    use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

    type Captured = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

    struct Capture(Captured);
    struct Fields(BTreeMap<String, String>);
    impl Visit for Fields {
        fn record_str(&mut self, f: &Field, v: &str) {
            self.0.insert(f.name().to_string(), v.to_string());
        }
        fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
            self.0.insert(f.name().to_string(), format!("{v:?}"));
        }
    }
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: LayerContext<'_, S>) {
            let mut f = Fields(BTreeMap::new());
            attrs.record(&mut f);
            self.0
                .lock()
                .push((attrs.metadata().name().to_string(), f.0));
        }
    }

    fn capture(f: impl FnOnce()) -> Vec<(String, BTreeMap<String, String>)> {
        crate::llm::flow::span_capture::pin_tracing_interest();
        let captured: Captured = Arc::default();
        let sub = tracing_subscriber::registry().with(Capture(captured.clone()));
        tracing::subscriber::with_default(sub, f);
        captured.lock().clone()
    }

    #[test]
    fn spans_helper_agent() {
        let spans = capture(|| {
            let _s = agent_span("weather");
        });
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "invoke_agent");
        assert_eq!(spans[0].1["otel.name"], "invoke_agent weather");
        assert_eq!(spans[0].1["gen_ai.operation.name"], "invoke_agent");
        assert_eq!(spans[0].1["gen_ai.agent.name"], "weather");
    }

    #[test]
    fn spans_helper_workflow_node_model_tool() {
        let spans = capture(|| {
            let _a = workflow_span("wf");
            let _b = node_span("step", "wf/step@1");
            let _c = generate_content_span("gemini-2.5-flash");
            let _d = execute_tool_span("get_weather");
        });
        let names: Vec<_> = spans.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "invoke_workflow",
                "invoke_node",
                "generate_content",
                "execute_tool"
            ]
        );
        assert_eq!(spans[0].1["otel.name"], "invoke_workflow wf");
        assert_eq!(spans[0].1["gen_ai.workflow.name"], "wf");
        assert_eq!(spans[1].1["otel.name"], "invoke_node step");
        assert_eq!(spans[1].1["gen_ai.node.name"], "step");
        assert_eq!(spans[1].1["gen_ai.node.path"], "wf/step@1");
        assert_eq!(spans[2].1["otel.name"], "generate_content gemini-2.5-flash");
        assert_eq!(spans[2].1["gen_ai.request.model"], "gemini-2.5-flash");
        assert_eq!(spans[3].1["otel.name"], "execute_tool get_weather");
        assert_eq!(spans[3].1["gen_ai.tool.name"], "get_weather");
        // Empty fields are declared but not visited at creation.
        assert!(!spans[1].1.contains_key("otel.status_code"));
    }

    #[test]
    fn spans_helpers_declare_status_fields() {
        let mut declared = Vec::new();
        let _ = capture(|| {
            for s in [
                node_span("n", "wf/n@1"),
                generate_content_span("m"),
                execute_tool_span("t"),
            ] {
                let m = s
                    .metadata()
                    .expect("span enabled under the capture subscriber");
                declared.push((
                    m.name(),
                    m.fields().field("otel.status_code").is_some(),
                    m.fields().field("error.message").is_some(),
                ));
            }
        });
        assert_eq!(
            declared,
            [
                ("invoke_node", true, true),
                ("generate_content", true, true),
                ("execute_tool", true, true)
            ]
        );
    }

    #[tokio::test]
    async fn compact_events_span_records_go_attributes() {
        use crate::llm::flow::span_capture::capture_spans;
        let ((), spans) = capture_spans(async {
            let parent = tracing::info_span!("invoke_workflow");
            let _g = parent.enter();
            let span = compact_events_span(&CompactEventsSpanParams {
                trigger: COMPACTION_TRIGGER_SLIDING_WINDOW,
                session_id: "sess".into(),
                invocation_id: "inv2".into(),
                summarizer_type: "fakeSummarizer".into(),
                backend: Some(gadk_model::Backend::VertexAi),
                event_count: 4,
                compaction_interval: 2,
                overlap_size: 1,
                ..Default::default()
            });
            trace_compaction_declined(
                &CompactEventsSpanParams {
                    trigger: COMPACTION_TRIGGER_TOKEN_THRESHOLD,
                    token_threshold: 10,
                    event_retention_size: 50,
                    ..Default::default()
                },
                "no compactable window past the retained tail",
            );
            drop(span);
        })
        .await;
        let c: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "compact_events")
            .collect();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].otel_name, "compact_events sliding_window");
        assert_eq!(c[0].parent.as_deref(), Some("invoke_workflow"));
        for (k, v) in [
            ("gen_ai.operation.name", "compact_events"),
            ("gen_ai.conversation.id", "sess"),
            ("gen_ai.compaction.trigger", "sliding_window"),
            ("gen_ai.compaction.summarizer_type", "fakeSummarizer"),
            ("gen_ai.compaction.event_count", "4"),
            ("gcp.vertex.agent.invocation_id", "inv2"),
            ("gen_ai.system", "gcp.vertex_ai"),
            ("gen_ai.compaction.compaction_interval", "2"),
            ("gen_ai.compaction.overlap_size", "1"),
        ] {
            assert_eq!(c[0].field(k).as_deref(), Some(v), "{k}");
        }
        assert!(c[0].field("gen_ai.compaction.token_threshold").is_none());
        assert!(c[0].closed);
        assert_eq!(c[1].otel_name, "compact_events token_threshold");
        assert_eq!(
            c[1].field("gen_ai.compaction.token_threshold").as_deref(),
            Some("10")
        );
        assert_eq!(
            c[1].field("gen_ai.compaction.event_retention_size")
                .as_deref(),
            Some("50")
        );
        assert_eq!(
            c[1].field("gen_ai.compaction.event_count").as_deref(),
            Some("0")
        );
        assert!(c[1].field("gcp.vertex.agent.invocation_id").is_none());
        assert!(c[1].field("gen_ai.system").is_none());
        assert_eq!(
            c[1].field("gen_ai.compaction.declined").as_deref(),
            Some("no compactable window past the retained tail")
        );
        assert!(c[1].closed);
    }

    #[test]
    fn epoch_seconds_matches_go_and_gen_ai_system_matches_semconv() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:01.5Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(epoch_seconds(t), 1_767_225_601.5);
        assert_eq!(
            gen_ai_system(Some(gadk_model::Backend::GeminiApi)),
            Some("gcp.gemini")
        );
        assert_eq!(
            gen_ai_system(Some(gadk_model::Backend::VertexAi)),
            Some("gcp.vertex_ai")
        );
        assert_eq!(gen_ai_system(None), None);
    }

    #[tokio::test]
    async fn trace_compaction_result_records_error_usage_and_result() {
        use crate::llm::flow::span_capture::capture_spans;
        use gadk_genai::GenerateContentResponseUsageMetadata as Usage;
        let ((), spans) = capture_spans(async {
            let p = CompactEventsSpanParams {
                trigger: COMPACTION_TRIGGER_SLIDING_WINDOW,
                ..Default::default()
            };
            let failed = compact_events_span(&p);
            let err = std::io::Error::other("boom");
            let usage = Usage {
                prompt_token_count: 4000,
                candidates_token_count: 8,
                ..Default::default()
            };
            trace_compaction_result(
                &failed,
                CompactionResultParams {
                    result_event: None,
                    error: Some(&err),
                    discard_reason: "",
                    declined: false,
                    usage: Some(&usage),
                },
            );
            drop(failed);
            let ok = compact_events_span(&p);
            let mut ev = gadk_session::Event {
                id: "sum-1".into(),
                ..Default::default()
            };
            ev.actions.compaction = Some(gadk_session::EventCompaction {
                start_timestamp: gadk_session::zero_time(),
                end_timestamp: chrono::DateTime::from_timestamp(1_767_225_604, 0).unwrap(),
                compacted_content: None,
                excluded_events: vec![],
            });
            trace_compaction_result(
                &ok,
                CompactionResultParams {
                    result_event: Some(&ev),
                    error: None,
                    discard_reason: "",
                    declined: false,
                    usage: None,
                },
            );
        })
        .await;
        let c: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "compact_events")
            .collect();
        assert_eq!(c[0].field("otel.status_code").as_deref(), Some("ERROR"));
        assert_eq!(c[0].field("error.message").as_deref(), Some("boom"));
        assert_eq!(
            c[0].field("otel.status_description").as_deref(),
            Some("boom")
        );
        assert_eq!(
            c[0].field("gen_ai.usage.input_tokens").as_deref(),
            Some("4000")
        );
        assert_eq!(
            c[0].field("gen_ai.usage.output_tokens").as_deref(),
            Some("8")
        );
        assert!(c[0].field("gen_ai.compaction.result_event_id").is_none());
        assert_eq!(
            c[1].field("gen_ai.compaction.result_event_id").as_deref(),
            Some("sum-1")
        );
        assert!(c[1].field("gen_ai.compaction.start_timestamp").is_none());
        assert_eq!(
            c[1].field("gen_ai.compaction.end_timestamp")
                .unwrap()
                .parse::<f64>()
                .unwrap(),
            1_767_225_604.0
        );
        assert!(c[1].field("otel.status_code").is_none());
    }
}
