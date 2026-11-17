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

//! The engine's ADK telemetry as `tracing` spans and events: the spans (`invoke_workflow`,
//! `invoke_node`, `generate_content`, `execute_tool`, `compact_events`; agents' own `invoke_agent`
//! spans come from gadk-agent) and the OTel log records.
//! No OpenTelemetry: the static span name is the operation and `otel.name` the full span name;
//! every other field is an OTel attribute (structured values per `converters`). All spans are
//! `INFO`. A `tracing` span records only the fields it declares, so each constructor declares every
//! attribute that may be set on it later as `Empty`, plus `otel.status_code` and `error.message`
//! (the span status and its description).
//!
//! The module is public for [`set_env_provider`], [`LOG_TARGET`] and [`d6`] (the shared decoder of
//! this layout); everything else stays crate-private. Submodules: `env` (the process-wide
//! environment provider and its rules), the span attributes (`attrs`, `converters`, `inference`,
//! `instrumentation`, `semconv`, `genai_messages`), the OTel log records of the legacy schema
//! (`logger`), and the test digests (`span_digest`, `log_digest`).

pub(crate) mod attrs;
pub(crate) mod converters;
pub mod d6;
pub(crate) mod env;
pub(crate) mod genai_messages;
pub(crate) mod inference;
pub(crate) mod instrumentation;
#[cfg(any(test, feature = "testutil"))]
pub(crate) mod log_digest;
pub(crate) mod logger;
pub(crate) mod semconv;
#[cfg(any(test, feature = "testutil"))]
pub(crate) mod span_digest;

pub use env::set_env_provider;

use chrono::{DateTime, Utc};
use gadk_genai::GenerateContentResponseUsageMetadata;
use gadk_model::Backend;
use gadk_session::{Event, zero_time};
use serde_json::Value;

use attrs::SpanError;

/// The `tracing` target of the OTel log records; a `tracing` layer that collects them
/// (gadk-server's debug store) filters on it.
pub const LOG_TARGET: &str = "gadk_engine::telemetry::log";

/// The [`SpanIds`](d6::SpanIds) of the nearest enclosing span (the current span first, then its
/// ancestors) that carries them in its `tracing` registry extensions — the IDs gadk-telemetry's
/// bridge leaves on every ADK span it exports. The analytics plugin reads them where it needs the
/// current trace and span IDs. `None` outside any span, when no enclosing span carries IDs (no
/// bridge installed), or under a subscriber that is not a `tracing_subscriber::Registry` stack (as
/// `gadk_agent::agent::telemetry::in_workflow_scope`).
pub fn current_span_ids() -> Option<d6::SpanIds> {
    let id = tracing::Span::current().id()?;
    tracing::dispatcher::get_default(|d| {
        use tracing_subscriber::registry::LookupSpan;
        let registry = d.downcast_ref::<tracing_subscriber::Registry>()?;
        registry
            .span(&id)?
            .scope()
            .find_map(|s| s.extensions().get::<d6::SpanIds>().copied())
    })
}

/// The `invoke_agent` span. Engine-internal: agents' own spans come from `CustomAgent::run`
/// (gadk-agent), and engine code never wraps `Agent::run` in another one, so only the span unit
/// tests use it.
#[cfg(test)]
pub(crate) fn agent_span(agent_name: &str) -> tracing::Span {
    tracing::info_span!(
        "invoke_agent",
        otel.name = %format!("invoke_agent {agent_name}"),
        gen_ai.operation.name = "invoke_agent",
        gen_ai.agent.name = agent_name
    )
}

/// Installs, once per process, gadk-engine's resolver of the type name that spans record as
/// `error.type` for engine errors gadk-agent cannot see ([`crate::error::go_error_type`], which
/// names dynamic child run failures). Called where such an error is built, so it is in place before
/// one can reach a span.
pub(crate) fn install_go_error_type_resolver() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        gadk_agent::agent::telemetry::set_go_error_type_resolver(crate::error::go_error_type);
    });
}

/// Whether the current span is inside an `invoke_workflow` span (shared with gadk-agent's agent
/// spans).
pub(crate) use gadk_agent::agent::telemetry::in_workflow_scope;

/// The `invoke_workflow` span: `gen_ai.workflow.nested` marks a span started inside another
/// `invoke_workflow` span (default schema only).
pub(crate) fn workflow_span(name: &str, conversation_id: &str) -> tracing::Span {
    use tracing::field::Empty;
    let nested = in_workflow_scope() && !env::use_legacy_schema();
    let span = tracing::info_span!(
        "invoke_workflow",
        otel.name = %format!("invoke_workflow {name}"),
        gen_ai.operation.name = "invoke_workflow",
        gen_ai.workflow.name = name,
        gen_ai.conversation.id = conversation_id,
        gen_ai.workflow.nested = Empty,
        error.type = Empty,
        otel.status_code = Empty,
        error.message = Empty
    );
    if nested {
        span.record("gen_ai.workflow.nested", true);
    }
    span
}

/// The `invoke_node` span.
pub(crate) fn node_span(node_name: &str, conversation_id: &str) -> tracing::Span {
    use tracing::field::Empty;
    tracing::info_span!(
        "invoke_node",
        otel.name = %format!("invoke_node {node_name}"),
        gen_ai.operation.name = "invoke_node",
        gen_ai.node.name = node_name,
        gen_ai.conversation.id = conversation_id,
        error.type = Empty,
        otel.status_code = Empty,
        error.message = Empty
    )
}

/// The `generate_content` span, with its start attributes other than the provider (see
/// [`attrs::start_generate_content_span`]).
pub(crate) fn generate_content_span(model: &str, invocation_id: &str) -> tracing::Span {
    use tracing::field::Empty;
    tracing::info_span!(
        "generate_content",
        otel.name = %format!("generate_content {model}"),
        gcp.vertex.agent.invocation_id = invocation_id,
        gen_ai.operation.name = "generate_content",
        gen_ai.request.model = model,
        gen_ai.provider.name = Empty,
        gen_ai.system_instructions = Empty,
        gen_ai.input.messages = Empty,
        gen_ai.response.finish_reasons = Empty,
        gcp.vertex.agent.event_id = Empty,
        gen_ai.usage.input_tokens = Empty,
        gen_ai.usage.output_tokens = Empty,
        gen_ai.usage.cache_read.input_tokens = Empty,
        gen_ai.usage.reasoning.output_tokens = Empty,
        gen_ai.output.messages = Empty,
        error.type = Empty,
        otel.status_code = Empty,
        error.message = Empty
    )
}

/// The `execute_tool` span, with its name attributes (see [`attrs::start_execute_tool_span`]).
pub(crate) fn execute_tool_span(tool_name: &str) -> tracing::Span {
    use tracing::field::Empty;
    tracing::info_span!(
        "execute_tool",
        otel.name = %format!("execute_tool {tool_name}"),
        gen_ai.operation.name = "execute_tool",
        gen_ai.tool.name = tool_name,
        gcp.vertex.agent.tool_call_args = Empty,
        gen_ai.tool.call.arguments = Empty,
        gen_ai.tool.description = Empty,
        gcp.vertex.agent.event_id = Empty,
        gen_ai.tool.call.id = Empty,
        gen_ai.tool.call.result = Empty,
        gcp.vertex.agent.tool_response = Empty,
        error.type = Empty,
        otel.status_code = Empty,
        error.message = Empty
    )
}

/// The `execute_tool (merged)` span: over parallel tool calls, without attributes until
/// [`attrs::trace_merged_tool_calls_result`].
pub(crate) fn merged_tool_calls_span() -> tracing::Span {
    use tracing::field::Empty;
    tracing::info_span!(
        "execute_tool",
        otel.name = "execute_tool (merged)",
        gen_ai.operation.name = Empty,
        gen_ai.tool.name = Empty,
        gen_ai.tool.description = Empty,
        gcp.vertex.agent.tool_call_args = Empty,
        gcp.vertex.agent.tool_response = Empty,
        gcp.vertex.agent.event_id = Empty,
        error.type = Empty,
        otel.status_code = Empty,
        error.message = Empty
    )
}

/// Emits one OTel log record as a `tracing` event under `parent`: target [`LOG_TARGET`], fields
/// `event.name`, `otel.log.body` (the JSON text of the body; `null` when it has none) and, when
/// there are any, `otel.log.attributes` (a JSON object). A `DEBUG` record is emitted at the `DEBUG`
/// level, anything else at `INFO`.
pub(crate) fn emit_log_record(
    parent: &tracing::Span,
    level: tracing::Level,
    event_name: &str,
    body: &Value,
    attributes: &[(String, Value)],
) {
    let body = body.to_string();
    let attributes = (!attributes.is_empty()).then(|| {
        Value::Object(
            attributes
                .iter()
                .cloned()
                .collect::<serde_json::Map<_, _>>(),
        )
        .to_string()
    });
    if level == tracing::Level::DEBUG {
        tracing::event!(
            target: LOG_TARGET,
            parent: parent,
            tracing::Level::DEBUG,
            event.name = event_name,
            otel.log.body = body.as_str(),
            otel.log.attributes = attributes.as_deref()
        );
    } else {
        tracing::event!(
            target: LOG_TARGET,
            parent: parent,
            tracing::Level::INFO,
            event.name = event_name,
            otel.log.body = body.as_str(),
            otel.log.attributes = attributes.as_deref()
        );
    }
}

/// The name of the compaction span.
const COMPACT_EVENTS: &str = "compact_events";

/// The trigger of a sliding-window compaction.
pub(crate) const COMPACTION_TRIGGER_SLIDING_WINDOW: &str = "sliding_window";
/// The trigger of a token-threshold compaction.
pub(crate) const COMPACTION_TRIGGER_TOKEN_THRESHOLD: &str = "token_threshold";

/// The parameters of a `compact_events` span. Zero thresholds mean "strategy disabled" and their
/// attributes are omitted.
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

/// The `gen_ai.system` value: `gcp.vertex_ai` / `gcp.gemini` (semconv v1.36), `None` for a backend
/// that cannot be named.
pub(crate) fn gen_ai_system(backend: Option<Backend>) -> Option<&'static str> {
    match backend {
        Some(Backend::VertexAi) => Some("gcp.vertex_ai"),
        Some(Backend::GeminiApi) => Some("gcp.gemini"),
        None => None,
    }
}

/// Seconds since the Unix epoch as a float: nanoseconds since the epoch divided by 1e9.
pub(crate) fn epoch_seconds(t: DateTime<Utc>) -> f64 {
    match t.timestamp_nanos_opt() {
        Some(n) => n as f64 / 1e9,
        // Outside the range of nanoseconds since the epoch: whole seconds plus the sub-second part.
        // Never reached for a recorded bound, which is omitted when zero.
        None => t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9,
    }
}

/// Starts a span named `compact_events` (`otel.name` `compact_events <trigger>`), a child of the
/// caller's current span.
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
        error.type = Empty,
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

/// The outcome of a compaction, recorded by [`trace_compaction_result`].
pub(crate) struct CompactionResultParams<'a> {
    pub(crate) result_event: Option<&'a Event>,
    pub(crate) error: Option<&'a (dyn std::error::Error + 'static)>,
    pub(crate) discard_reason: &'a str,
    pub(crate) declined: bool,
    pub(crate) usage: Option<&'a GenerateContentResponseUsageMetadata>,
}

/// Records an error known only by its text on `span`.
pub(crate) fn record_span_error(span: &tracing::Span, err: &dyn std::fmt::Display) {
    attrs::record_error(span, Some(&SpanError::text(err.to_string())));
}

/// Records a compaction's outcome on its span: any error, the decline reason, the usage and the
/// result event's range.
pub(crate) fn trace_compaction_result(span: &tracing::Span, p: CompactionResultParams<'_>) {
    if let Some(e) = p.error {
        attrs::record_error(span, Some(&SpanError::new(e)));
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

/// A closed span with the same attributes plus the reason.
pub(crate) fn trace_compaction_declined(p: &CompactEventsSpanParams, reason: &str) {
    let span = compact_events_span(p);
    span.record("gen_ai.compaction.declined", reason);
}

/// Records the compaction's token usage: output tokens include thoughts, per semconv.
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
            let _a = workflow_span("wf", "sess");
            let _b = node_span("step", "sess");
            let _c = generate_content_span("gemini-2.5-flash", "inv");
            let _d = execute_tool_span("get_weather");
            let _e = merged_tool_calls_span();
        });
        let names: Vec<_> = spans.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "invoke_workflow",
                "invoke_node",
                "generate_content",
                "execute_tool",
                "execute_tool"
            ]
        );
        assert_eq!(spans[0].1["otel.name"], "invoke_workflow wf");
        assert_eq!(spans[0].1["gen_ai.workflow.name"], "wf");
        assert_eq!(spans[0].1["gen_ai.conversation.id"], "sess");
        assert_eq!(spans[1].1["otel.name"], "invoke_node step");
        assert_eq!(spans[1].1["gen_ai.node.name"], "step");
        assert_eq!(spans[1].1["gen_ai.conversation.id"], "sess");
        assert_eq!(spans[2].1["otel.name"], "generate_content gemini-2.5-flash");
        assert_eq!(spans[2].1["gen_ai.request.model"], "gemini-2.5-flash");
        assert_eq!(spans[2].1["gcp.vertex.agent.invocation_id"], "inv");
        assert_eq!(spans[3].1["otel.name"], "execute_tool get_weather");
        assert_eq!(spans[3].1["gen_ai.tool.name"], "get_weather");
        assert_eq!(spans[4].1["otel.name"], "execute_tool (merged)");
        assert_eq!(spans[4].1.len(), 1, "{:?}", spans[4].1);
        // Empty fields are declared but not visited at creation.
        assert!(!spans[1].1.contains_key("otel.status_code"));
        assert!(!spans[0].1.contains_key("gen_ai.workflow.nested"));
    }

    #[test]
    fn spans_helpers_declare_status_fields() {
        let mut declared = Vec::new();
        let _ = capture(|| {
            for s in [
                workflow_span("w", "s"),
                node_span("n", "s"),
                generate_content_span("m", ""),
                execute_tool_span("t"),
                merged_tool_calls_span(),
            ] {
                let m = s
                    .metadata()
                    .expect("span enabled under the capture subscriber");
                declared.push((
                    m.name(),
                    m.fields().field("otel.status_code").is_some(),
                    m.fields().field("error.message").is_some(),
                    m.fields().field("error.type").is_some(),
                ));
            }
        });
        assert_eq!(
            declared,
            [
                ("invoke_workflow", true, true, true),
                ("invoke_node", true, true, true),
                ("generate_content", true, true, true),
                ("execute_tool", true, true, true),
                ("execute_tool", true, true, true)
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

    /// The model and tool spans of one LLM turn, digested: Go's attribute set on each (the
    /// wire-probe's `debug_trace` view of a successful turn and a failed call), and the
    /// inference-details record under the model span.
    #[tokio::test]
    async fn engine_spans_carry_go_attributes() {
        use crate::telemetry::span_digest::{build_digest, capture};
        use gadk_agent::InvocationContext;
        use gadk_genai::{Content, FinishReason, Part};
        use gadk_model::{LlmRequest, LlmResponse, ModelError};
        use serde_json::json;

        let _env = env::test_env(&[]);
        let ok = LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    text: "hi".into(),
                    ..Default::default()
                }],
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                ..Default::default()
            }),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        let model =
            crate::testutil::MockModel::new(vec![Ok(ok), Err(ModelError::Other("boom".into()))]);
        let ctx = InvocationContext::builder()
            .invocation_id("inv")
            .build_invocation();
        let (ids, spans, logs) = capture(async {
            let root = workflow_span("root", "sess");
            let _g = root.enter();
            let mut ids = Vec::new();
            for _ in 0..2 {
                let mut s = crate::llm::flow::generate_content(
                    &ctx,
                    model.clone(),
                    LlmRequest::default(),
                    false,
                );
                while let Some((_, id)) = futures::StreamExt::next(&mut s).await {
                    ids.push(id);
                }
            }
            ids
        })
        .await;
        assert_eq!(ids.len(), 2);
        // The success span's event ID is the one reserved for its response.
        let event_id = spans[1]
            .attributes
            .iter()
            .find(|(k, _)| k == "gcp.vertex.agent.event_id")
            .map(|(_, v)| v.clone());
        assert_eq!(event_id, Some(json!(ids[0])));
        let d = build_digest(&spans, &logs).unwrap();
        let ok_attrs = json!({
            "gcp.vertex.agent.event_id": "PRESENT",
            "gcp.vertex.agent.invocation_id": "PRESENT",
            "gen_ai.operation.name": "generate_content",
            "gen_ai.request.model": "mock",
            "gen_ai.response.finish_reasons": ["stop"],
            "gen_ai.usage.cache_read.input_tokens": 0,
            "gen_ai.usage.input_tokens": 10,
            "gen_ai.usage.output_tokens": 5,
            "gen_ai.usage.reasoning.output_tokens": 0
        });
        let failed_attrs = json!({
            "error.type": "*errors.errorString",
            "gcp.vertex.agent.invocation_id": "PRESENT",
            "gen_ai.operation.name": "generate_content",
            "gen_ai.request.model": "mock",
            "gen_ai.response.finish_reasons": ["error"]
        });
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({"root_span": {
                "name": "invoke_workflow root",
                "attributes": {
                    "gen_ai.conversation.id": "PRESENT",
                    "gen_ai.operation.name": "invoke_workflow",
                    "gen_ai.workflow.name": "root"
                },
                "status": "UNSET",
                "children": [
                    {
                        "name": "generate_content mock",
                        "attributes": ok_attrs.clone(),
                        "status": "UNSET",
                        "children": [],
                        "logs": [{
                            "event_name": "gen_ai.client.inference.operation.details",
                            "body": null,
                            "attributes": ok_attrs
                        }]
                    },
                    {
                        "name": "generate_content mock",
                        "attributes": failed_attrs.clone(),
                        "status": "ERROR",
                        "children": [],
                        "logs": [{
                            "event_name": "gen_ai.client.inference.operation.details",
                            "body": null,
                            "attributes": failed_attrs
                        }]
                    }
                ],
                "logs": []
            }})
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod span_ids_tests {
    use tracing::span::{Attributes, Id};
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    use super::current_span_ids;
    use super::d6::SpanIds;

    /// Leaves [`SpanIds`] on every span named `with_ids`, the way gadk-telemetry's bridge does
    /// on the spans it exports; the span ID's last byte counts the spans it tagged.
    #[derive(Default)]
    struct Tagger(std::sync::atomic::AtomicU8);

    impl<S> Layer<S> for Tagger
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            if attrs.metadata().name() != "with_ids" {
                return;
            }
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(ids(n));
            }
        }
    }

    fn ids(n: u8) -> SpanIds {
        SpanIds {
            trace_id: [7; 16],
            span_id: [0, 0, 0, 0, 0, 0, 0, n],
        }
    }

    fn with_tagger(f: impl FnOnce()) {
        let sub = tracing_subscriber::registry().with(Tagger::default());
        tracing::subscriber::with_default(sub, f);
    }

    #[test]
    fn none_outside_any_span_and_without_ids() {
        with_tagger(|| {
            assert_eq!(current_span_ids(), None);
            let _plain = tracing::info_span!("plain").entered();
            assert_eq!(current_span_ids(), None);
        });
    }

    #[test]
    fn the_nearest_enclosing_span_with_ids_wins() {
        with_tagger(|| {
            let outer = tracing::info_span!("with_ids").entered();
            assert_eq!(current_span_ids(), Some(ids(1)));
            let plain = tracing::info_span!("plain").entered();
            assert_eq!(
                current_span_ids(),
                Some(ids(1)),
                "an untagged child inherits"
            );
            let inner = tracing::info_span!("with_ids").entered();
            assert_eq!(current_span_ids(), Some(ids(2)));
            drop(inner);
            assert_eq!(current_span_ids(), Some(ids(1)));
            drop(plain);
            drop(outer);
            assert_eq!(current_span_ids(), None);
        });
    }

    #[test]
    fn none_under_a_subscriber_without_a_registry() {
        let sub = tracing::subscriber::NoSubscriber::default();
        tracing::subscriber::with_default(sub, || {
            let _span = tracing::info_span!("with_ids").entered();
            assert_eq!(current_span_ids(), None);
        });
    }
}
